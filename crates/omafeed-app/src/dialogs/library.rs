//! "Manage library" window: add, edit, move, and remove feeds and folders.
use super::{Form, confirm, wait_closed};
use crate::ui::{Ui, widgets::margins};
use adw::prelude::*;
use gtk::glib;
use omafeed_core::{
    db::{Library, Scope},
    discovery::Candidate,
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
fn folder_paths(lib: &Library) -> Vec<(Option<i64>, String)> {
    fn walk(
        lib: &Library,
        parent: Option<i64>,
        prefix: &str,
        out: &mut Vec<(Option<i64>, String)>,
    ) {
        for f in lib.folders.iter().filter(|f| f.parent == parent) {
            let name = if prefix.is_empty() {
                f.name.clone()
            } else {
                format!("{prefix} / {}", f.name)
            };
            out.push((Some(f.id), name.clone()));
            walk(lib, Some(f.id), &name, out);
        }
    }
    let mut out = vec![(None, "Top level".into())];
    walk(lib, None, "", &mut out);
    out
}

struct FolderPicker {
    dropdown: gtk::DropDown,
    ids: Vec<Option<i64>>,
}

impl FolderPicker {
    fn new(form: &Form, lib: &Library, selected: Option<i64>) -> Self {
        let (ids, names): (Vec<_>, Vec<_>) = folder_paths(lib).into_iter().unzip();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::from_strings(&names);
        let index = ids.iter().position(|id| *id == selected).unwrap_or(0);
        dropdown.set_selected(index as u32);
        form.labeled("Folder", &dropdown);
        Self { dropdown, ids }
    }

    fn selected(&self) -> Option<i64> {
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
                        .feeds
                        .iter()
                        .find(|f| f.id == id)
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
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
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
            for f in lib.feeds.iter().filter(|f| f.folder == folder) {
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
            Ok(lib) => *u.library.borrow_mut() = lib,
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
            Some(Scope::Feed(id)) => {
                let lib = u.library.borrow();
                lib.feeds.iter().find(|f| f.id == id).and_then(|f| f.folder)
            }
            _ => None,
        };
        match (action, selected) {
            (Action::Remove, Some(scope)) => remove(&u, &m.window, scope).await,
            (Action::Remove | Action::Edit, None) => u.toast("Select a feed or folder first"),
            (Action::Edit, Some(Scope::Folder(id))) => {
                edit_folder(&u, &m.window, Some(id), None).await
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

async fn remove(u: &Rc<Ui>, window: &gtk::Window, scope: Scope) {
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
        return;
    }
    let result =
        u.db.call(move |s| match scope {
            Scope::Folder(id) => s.delete_folder(id),
            Scope::Feed(id) => s.delete_feed(id),
            _ => Ok(()),
        })
        .await;
    match result {
        Ok(()) => u.reset_selection(),
        Err(e) => u.error(e),
    }
}

/// Create a folder (`id` is `None`) or rename/move an existing one.
async fn edit_folder(u: &Rc<Ui>, window: &gtk::Window, id: Option<i64>, parent: Option<i64>) {
    let form = Form::new(window, "Folder");
    let (name, parent) = {
        let lib = u.library.borrow();
        match id.and_then(|id| lib.folders.iter().find(|f| f.id == id)) {
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
async fn edit_feed(u: &Rc<Ui>, window: &gtk::Window, id: Option<i64>, parent: Option<i64>) {
    let form = Form::new(window, "Subscription");
    let (title, original_url, parent) = {
        let lib = u.library.borrow();
        match id.and_then(|id| lib.feeds.iter().find(|f| f.id == id)) {
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
            let duplicate = u
                .library
                .borrow()
                .feeds
                .iter()
                .find(|f| f.url == url && Some(f.id) != id)
                .map(|f| f.title.clone());
            if let Some(title) = duplicate {
                u.toast(&format!("Already subscribed to {title}"));
                return;
            }
        }
        let result =
            u.db.call(move |s| match id {
                Some(id) => s.update_feed(id, &name, &url, parent),
                None => s.add_feed(&name, &url, parent).map(drop),
            })
            .await;
        match result {
            Ok(()) => {
                u.reload();
                u.start_refresh(true);
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
