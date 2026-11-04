//! Feed and folder forms: subscribing, creating folders, editing and removing either.
use super::{Form, confirm, wait_closed};
use crate::ui::{Ui, widgets::label};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::{
    Error,
    ingest::{Which, discovery::Candidate},
    model::{FeedId, FolderId, Library, Scope, TreeItem},
    subscriptions,
};
use std::rc::Rc;

/// Folder choices as (id, "Parent / Child" path), starting with the top level.
fn folder_paths(lib: &Library) -> Vec<(Option<FolderId>, String)> {
    let mut out = vec![(None, "Top level".into())];
    let mut names: Vec<&str> = Vec::new();
    for item in lib.tree() {
        match item {
            TreeItem::Folder { folder, .. } => {
                names.push(&folder.name);
                out.push((Some(folder.id), names.join(" / ")));
            }
            TreeItem::FolderEnd(_) => {
                names.pop();
            }
            TreeItem::Feed { .. } => {}
        }
    }
    out
}

struct FolderPicker {
    dropdown: gtk::DropDown,
    ids: Vec<Option<FolderId>>,
}

impl FolderPicker {
    /// A folder choice in `group`, leaving out `except` and the folders inside it, where a
    /// folder cannot go.
    fn new(
        form: &Form,
        lib: &Library,
        selected: Option<FolderId>,
        except: Option<FolderId>,
    ) -> Self {
        let (ids, names): (Vec<_>, Vec<_>) = folder_paths(lib)
            .into_iter()
            .filter(|(id, _)| match (id, except) {
                (Some(id), Some(except)) => {
                    !lib.folder_path(Some(*id)).iter().any(|f| f.id == except)
                }
                _ => true,
            })
            .unzip();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let index = ids.iter().position(|id| *id == selected).unwrap_or(0);
        let dropdown = form.choice("Folder", &names, index);
        Self { dropdown, ids }
    }

    fn selected(&self) -> Option<FolderId> {
        self.ids
            .get(self.dropdown.selected() as usize)
            .copied()
            .flatten()
    }
}

/// Subscribe to a feed in `folder`, or at the top level.
pub fn add_feed(u: Rc<Ui>, folder: Option<FolderId>) {
    glib::spawn_future_local(async move {
        let window = u.window.clone();
        edit_feed(&u, &window, None, folder).await;
    });
}

/// Create a folder inside `parent`, or at the top level.
pub fn new_folder(u: Rc<Ui>, parent: Option<FolderId>) {
    glib::spawn_future_local(async move {
        let window = u.window.clone();
        edit_folder(&u, &window, None, parent).await;
    });
}

/// Edit a feed or folder: rename it, move it, or change a feed's URL.
pub fn edit(u: Rc<Ui>, scope: Scope) {
    glib::spawn_future_local(async move {
        let window = u.window.clone();
        match scope {
            Scope::Feed(id) => edit_feed(&u, &window, Some(id), None).await,
            Scope::Folder(id) => edit_folder(&u, &window, Some(id), None).await,
            _ => {}
        }
    });
}

/// Remove a feed or folder after confirmation, keeping the view shown unless it was the one
/// removed.
pub fn remove(u: Rc<Ui>, scope: Scope) {
    glib::spawn_future_local(async move {
        let window = u.window.clone();
        if remove_confirmed(&u, &window, scope.clone()).await {
            u.removed(&scope);
        }
    });
}

/// Ask, then remove a feed or folder. Returns whether it is gone.
async fn remove_confirmed(u: &Rc<Ui>, window: &adw::ApplicationWindow, scope: Scope) -> bool {
    let (heading, body) = match scope {
        Scope::Folder(_) => (
            "Remove folder?",
            "Feeds and child folders will move to its parent. Your articles and read state are kept.",
        ),
        _ => (
            "Remove subscription?",
            "This removes the feed and its downloaded articles, stars, and read state from omafeed.",
        ),
    };
    if !confirm(window, heading, body).await {
        return false;
    }
    let result = match scope {
        Scope::Folder(id) => u.db.call(move |s| s.delete_folder(id)).await,
        Scope::Feed(id) => subscriptions::unsubscribe(&u.db, id, true)
            .await
            .map(drop)
            // Removed by another program meanwhile: it is gone either way.
            .or_else(|e| match e.downcast_ref::<Error>() {
                Some(Error::FeedNotFound(_)) => Ok(()),
                _ => Err(e),
            }),
        _ => Ok(()),
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            u.error(e);
            false
        }
    }
}

/// Create a folder (`id` is `None`) or rename/move an existing one.
async fn edit_folder(
    u: &Rc<Ui>,
    window: &adw::ApplicationWindow,
    id: Option<FolderId>,
    parent: Option<FolderId>,
) {
    let (name, parent) = {
        let lib = u.library.borrow();
        match id.and_then(|id| lib.folder(id)) {
            Some(f) => (f.name.clone(), f.parent),
            None => (String::new(), parent),
        }
    };
    let form = match id {
        Some(_) => Form::new(window, "Edit folder", "Save"),
        None => Form::new(window, "New folder", "Create"),
    };
    let name_entry = form.entry("Name", &name);
    let picker = FolderPicker::new(&form, &u.library.borrow(), parent, id);
    form.focus(&name_entry);
    while form.run().await {
        let name = name_entry.text().to_string();
        let parent = picker.selected();
        let result =
            u.db.call(move |s| match id {
                Some(id) => s.edit_folder(id, &name, parent),
                None => s.add_folder(&name, parent).map(drop),
            })
            .await;
        match result {
            Ok(()) => {
                u.reload();
                return;
            }
            Err(e) => form.show_error(&e.to_string()),
        }
    }
}

/// Subscribe (`id` is `None`) or edit a feed. New or changed URLs go through discovery.
async fn edit_feed(
    u: &Rc<Ui>,
    window: &adw::ApplicationWindow,
    id: Option<FeedId>,
    parent: Option<FolderId>,
) {
    let (title, original_url, parent) = {
        let lib = u.library.borrow();
        match id.and_then(|id| lib.feed(id)) {
            Some(f) => (f.title.clone(), f.url.clone(), f.folder),
            None => (String::new(), String::new(), parent),
        }
    };
    let form = match id {
        Some(_) => Form::new(window, "Edit feed", "Save"),
        None => Form::new(window, "Add feed", "Add"),
    };
    // A new feed starts from its address; an existing one is usually renamed.
    let (url_entry, name_entry) = match id {
        None => {
            form.note(
                "Paste a website or feed address. omafeed finds its RSS, Atom, or JSON feed.",
            );
            let url = form.entry("Address", "");
            url.set_placeholder_text(Some("example.com"));
            let name = form.entry("Name", "");
            name.set_placeholder_text(Some("The feed's own title"));
            (url, name)
        }
        Some(_) => {
            let name = form.entry("Name", &title);
            let url = form.entry("Feed address", &original_url);
            (url, name)
        }
    };
    let picker = FolderPicker::new(&form, &u.library.borrow(), parent, None);
    if id.is_none() {
        form.focus(&url_entry);
    } else {
        form.focus(&name_entry);
    }
    while form.run().await {
        let mut name = name_entry.text().to_string();
        let mut url = url_entry.text().trim().to_string();
        let parent = picker.selected();
        if id.is_none() || url != original_url {
            let feed = match find_feed(u, window, url).await {
                None => continue,
                Some(Err(e)) => {
                    form.show_error(&format!("{e:#}"));
                    continue;
                }
                Some(Ok(feed)) => feed,
            };
            url = feed.url;
            if name.trim().is_empty() {
                name = feed.title;
            }
        }
        let result = match id {
            Some(id) => subscriptions::update(&u.db, id, name, url.clone(), parent)
                .await
                .map(|()| id),
            None => subscriptions::subscribe(&u.db, name, url.clone(), parent).await,
        };
        match result {
            Ok(id) => {
                u.reload();
                u.start_refresh(Which::Only(vec![id]));
                return;
            }
            Err(e) if matches!(e.downcast_ref::<Error>(), Some(Error::DuplicateFeed(_))) => {
                let title = subscriptions::subscribed_title(&u.db, url.clone())
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(url);
                u.toast(&format!("Already subscribed to {title}"));
                return;
            }
            Err(e) => form.show_error(&e.to_string()),
        }
    }
}

/// Discover feeds at `url` behind a cancellable progress dialog, letting the user
/// choose when a site offers several. Returns `None` if the user cancelled.
async fn find_feed(
    u: &Rc<Ui>,
    window: &adw::ApplicationWindow,
    url: String,
) -> Option<anyhow::Result<Candidate>> {
    let progress = adw::AlertDialog::builder()
        .heading("Looking for feeds…")
        .body("Checking the website for RSS, Atom, and JSON feeds.")
        .build();
    let spinner = gtk::Spinner::new();
    spinner.start();
    progress.set_extra_child(Some(&spinner));
    progress.add_response("cancel", "Cancel");
    progress.set_close_response("cancel");
    let (cancel, cancelled) = async_channel::bounded::<()>(1);
    progress.connect_response(None, move |_, _| {
        let _ = cancel.try_send(());
    });
    let closed = wait_closed(&progress);
    progress.present(Some(window));
    let discovered = tokio::select! {
        result = u.discover(url) => Some(result),
        _ = cancelled.recv() => None,
    };
    if discovered.is_some() {
        progress.close();
    }
    closed.await;
    let mut feeds = match discovered? {
        Ok(feeds) => feeds,
        Err(e) => return Some(Err(e)),
    };
    if feeds.len() == 1 {
        return Some(Ok(feeds.remove(0)));
    }
    let choose = Form::new(window, "Choose a feed", "Subscribe");
    choose.note("This website offers several feeds.");
    // Sites may offer a dozen: past a few, the list scrolls instead of outgrowing the screen.
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    rows.add_css_class("form-choices");
    let mut checks: Vec<gtk::CheckButton> = Vec::new();
    for f in &feeds {
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.append(&label(&f.title, "form-setting-title"));
        let url = label(&f.url, "form-note");
        url.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        text.append(&url);
        let check = gtk::CheckButton::builder().child(&text).build();
        check.set_group(checks.first());
        check.set_active(checks.is_empty());
        rows.append(&check);
        checks.push(check);
    }
    choose.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .child(&rows)
            .build(),
    );
    if !choose.run().await {
        return None;
    }
    let index = checks.iter().position(|c| c.is_active()).unwrap_or(0);
    Some(Ok(feeds.remove(index)))
}
