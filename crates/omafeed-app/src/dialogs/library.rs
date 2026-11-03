//! "Manage library" window: add, edit, move, and remove feeds and folders.
use super::{Form, confirm, wait_closed};
use crate::ui::{Ui, widgets::margins};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::{
    Error,
    ingest::discovery::Candidate,
    model::{FeedId, FolderId, Library, Scope, TreeItem},
    subscriptions,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    NewFolder,
    AddFeed,
    Edit,
    Remove,
}

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
    fn new(form: &Form, lib: &Library, selected: Option<FolderId>) -> Self {
        let (ids, names): (Vec<_>, Vec<_>) = folder_paths(lib).into_iter().unzip();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::from_strings(&names);
        let index = ids.iter().position(|id| *id == selected).unwrap_or(0);
        dropdown.set_selected(index as u32);
        form.labeled("Folder", &dropdown);
        Self { dropdown, ids }
    }

    fn selected(&self) -> Option<FolderId> {
        self.ids
            .get(self.dropdown.selected() as usize)
            .copied()
            .flatten()
    }
}

struct Manager {
    window: gtk::Window,
    list: gtk::ListBox,
    rows: Rc<RefCell<Vec<Scope>>>,
    actions: async_channel::Receiver<Action>,
}

impl Manager {
    fn new(u: &Rc<Ui>) -> Self {
        let window = gtk::Window::builder()
            .title("Manage library")
            .transient_for(&u.window)
            .modal(true)
            .default_width(740)
            .default_height(650)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&root, 16);
        let desc = gtk::Label::new(Some(
            "Organize your subscriptions. Edit a feed to rename it or move it to another folder.",
        ));
        desc.set_wrap(true);
        desc.set_xalign(0.0);
        root.append(&desc);
        let toolbar = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(1)
            .max_children_per_line(4)
            .column_spacing(8)
            .row_spacing(8)
            .build();
        let (tx, actions) = async_channel::unbounded();
        for (label, action) in [
            ("New folder", Action::NewFolder),
            ("Add feed", Action::AddFeed),
            ("Edit / Move", Action::Edit),
            ("Remove", Action::Remove),
        ] {
            let button = gtk::Button::with_label(label);
            if action == Action::Remove {
                button.add_css_class("destructive-action");
            }
            let tx = tx.clone();
            button.connect_clicked(move |_| {
                let _ = tx.try_send(action);
            });
            toolbar.insert(&button, -1);
        }
        root.append(&toolbar);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        root.append(
            &gtk::ScrolledWindow::builder()
                .vexpand(true)
                .child(&list)
                .build(),
        );
        let detail = gtk::Label::new(None);
        detail.set_wrap(true);
        detail.set_xalign(0.0);
        detail.set_selectable(true);
        root.append(&detail);
        let close = gtk::Button::with_label("Done");
        root.append(&close);
        window.set_child(Some(&root));
        close.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| window.close()
        ));
        // Closing the channel ends the manager's event loop.
        window.connect_close_request(move |_| {
            tx.close();
            glib::Propagation::Proceed
        });
        let rows = Rc::new(RefCell::new(Vec::new()));
        list.connect_row_selected(glib::clone!(
            #[weak]
            u,
            #[strong]
            rows,
            move |_, row| {
                let scope = row.and_then(|r| rows.borrow().get(r.index() as usize).cloned());
                let text = match scope {
                    Some(Scope::Feed(id)) => u
                        .library
                        .borrow()
                        .feed(id)
                        .map(|f| match &f.error {
                            Some(e) => format!("{}\nLast refresh: {e}", f.url),
                            None => f.url.clone(),
                        })
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                detail.set_text(&text);
            }
        ));
        Self {
            window,
            list,
            rows,
            actions,
        }
    }

    fn populate(&self, lib: &Library) {
        self.list.remove_all();
        let mut rows = self.rows.borrow_mut();
        rows.clear();
        for (folder, path) in folder_paths(lib) {
            if let Some(id) = folder {
                let row = gtk::Label::new(Some(&path));
                row.set_xalign(0.0);
                margins(&row, 12);
                row.set_margin_end(0);
                row.add_css_class("heading");
                self.list.append(&row);
                rows.push(Scope::Folder(id));
            }
            for f in lib.feeds_in(folder) {
                let warning = if f.error.is_some() { "  ⚠" } else { "" };
                let row = gtk::Label::new(Some(&format!("    {}{warning}", f.title)));
                row.set_xalign(0.0);
                row.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.set_margin_start(16);
                row.set_margin_top(10);
                row.set_margin_bottom(10);
                self.list.append(&row);
                rows.push(Scope::Feed(f.id));
            }
        }
    }

    fn selected(&self) -> Option<Scope> {
        let row = self.list.selected_row()?;
        self.rows.borrow().get(row.index() as usize).cloned()
    }
}

pub fn library(u: Rc<Ui>) {
    glib::spawn_future_local(run(u));
}

async fn run(u: Rc<Ui>) {
    let m = Manager::new(&u);
    m.window.present();
    loop {
        match u.db.call(|s| s.library()).await {
            Ok(lib) => {
                u.close_sidebar_menu();
                *u.library.borrow_mut() = lib;
            }
            Err(e) => {
                u.error(e);
                break;
            }
        }
        m.populate(&u.library.borrow());
        let Ok(action) = m.actions.recv().await else {
            break;
        };
        let selected = m.selected();
        // New items go into the selected folder, or the selected feed's folder.
        let parent = match selected {
            Some(Scope::Folder(id)) => Some(id),
            Some(Scope::Feed(id)) => u.library.borrow().feed(id).and_then(|f| f.folder),
            _ => None,
        };
        match (action, selected) {
            (Action::Remove, Some(scope)) => {
                if remove_confirmed(&u, &m.window, scope).await {
                    u.reset_selection();
                }
            }
            (Action::Remove | Action::Edit, None) => u.toast("Select a feed or folder first"),
            (Action::Edit, Some(Scope::Folder(id))) => {
                edit_folder(&u, &m.window, Some(id), None).await;
            }
            (Action::Edit, Some(Scope::Feed(id))) => edit_feed(&u, &m.window, Some(id), None).await,
            (Action::NewFolder, _) => edit_folder(&u, &m.window, None, parent).await,
            (Action::AddFeed, _) => edit_feed(&u, &m.window, None, parent).await,
            (Action::Edit, Some(_)) => {}
        }
    }
    m.window.close();
    u.reload();
}

/// Edit a feed or folder from the main window, as "Edit / Move" does in the manager.
pub fn edit(u: Rc<Ui>, scope: Scope) {
    glib::spawn_future_local(async move {
        let window = u.window.clone().upcast::<gtk::Window>();
        match scope {
            Scope::Feed(id) => edit_feed(&u, &window, Some(id), None).await,
            Scope::Folder(id) => edit_folder(&u, &window, Some(id), None).await,
            _ => {}
        }
    });
}

/// Remove a feed or folder from the main window after the manager's confirmation, keeping
/// the view shown unless it was the one removed.
pub fn remove(u: Rc<Ui>, scope: Scope) {
    glib::spawn_future_local(async move {
        let window = u.window.clone().upcast::<gtk::Window>();
        if remove_confirmed(&u, &window, scope.clone()).await {
            u.removed(&scope);
        }
    });
}

/// Ask, then remove a feed or folder. Returns whether it is gone.
async fn remove_confirmed(u: &Rc<Ui>, window: &gtk::Window, scope: Scope) -> bool {
    let (heading, body) = match scope {
        Scope::Folder(_) => (
            "Remove folder?",
            "Feeds and child folders will move to its parent. Your articles and read state are kept.",
        ),
        _ => (
            "Remove subscription?",
            "This removes the feed and its downloaded articles, stars, and read state from Omafeed.",
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
    window: &gtk::Window,
    id: Option<FolderId>,
    parent: Option<FolderId>,
) {
    let form = Form::new(window, "Folder");
    let (name, parent) = {
        let lib = u.library.borrow();
        match id.and_then(|id| lib.folder(id)) {
            Some(f) => (f.name.clone(), f.parent),
            None => (String::new(), parent),
        }
    };
    let name_entry = form.entry("Name", &name);
    let picker = FolderPicker::new(&form, &u.library.borrow(), parent);
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
async fn edit_feed(u: &Rc<Ui>, window: &gtk::Window, id: Option<FeedId>, parent: Option<FolderId>) {
    let form = Form::new(window, "Subscription");
    let (title, original_url, parent) = {
        let lib = u.library.borrow();
        match id.and_then(|id| lib.feed(id)) {
            Some(f) => (f.title.clone(), f.url.clone(), f.folder),
            None => (String::new(), String::new(), parent),
        }
    };
    let name_entry = form.entry("Name (optional)", &title);
    name_entry.set_placeholder_text(Some("Use feed title"));
    let url_entry = form.entry("Website or feed URL", &original_url);
    let picker = FolderPicker::new(&form, &u.library.borrow(), parent);
    while form.run().await {
        let mut name = name_entry.text().to_string();
        let mut url = url_entry.text().to_string();
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
            Some(id) => subscriptions::update(&u.db, id, name, url.clone(), parent).await,
            None => subscriptions::subscribe(&u.db, name, url.clone(), parent)
                .await
                .map(drop),
        };
        match result {
            Ok(()) => {
                u.reload();
                u.start_refresh(true);
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
    window: &gtk::Window,
    url: String,
) -> Option<anyhow::Result<Candidate>> {
    u.set_status("Looking for feeds…");
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
    u.set_status("Ready");
    let mut feeds = match discovered? {
        Ok(feeds) => feeds,
        Err(e) => return Some(Err(e)),
    };
    if feeds.len() == 1 {
        return Some(Ok(feeds.remove(0)));
    }
    let choose = Form::new(window, "Choose a feed").save_label("Subscribe");
    let labels: Vec<String> = feeds
        .iter()
        .map(|f| format!("{} — {}", f.title, f.url))
        .collect();
    let names: Vec<&str> = labels.iter().map(String::as_str).collect();
    let picker = gtk::DropDown::from_strings(&names);
    choose.append(&picker);
    if !choose.run().await {
        return None;
    }
    let index = (picker.selected() as usize).min(feeds.len() - 1);
    Some(Ok(feeds.remove(index)))
}
