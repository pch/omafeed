use crate::ui::Ui;
use adw::prelude::*;
use gtk::{gio, glib};
use omafeed_core::db::{Library, Scope};
use std::{cell::RefCell, rc::Rc};

struct Form {
    dialog: adw::AlertDialog,
    content: gtk::Box,
    parent: gtk::Window,
}
impl Form {
    fn content_area(&self) -> gtk::Box {
        self.content.clone()
    }
    async fn run_future(&self) -> gtk::ResponseType {
        if self.dialog.clone().choose_future(Some(&self.parent)).await == "save" {
            gtk::ResponseType::Accept
        } else {
            gtk::ResponseType::Cancel
        }
    }
}
fn dialog(parent: &impl IsA<gtk::Window>, title: &str) -> Form {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let dialog = adw::AlertDialog::builder()
        .heading(title)
        .content_width(480)
        .extra_child(&content)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    Form {
        dialog,
        content,
        parent: parent.clone().upcast(),
    }
}
fn entry(d: &Form, title: &str, value: &str) -> gtk::Entry {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    d.content_area().append(&l);
    let e = gtk::Entry::builder()
        .text(value)
        .activates_default(true)
        .build();
    d.content_area().append(&e);
    e
}
fn folders(lib: &Library) -> Vec<(Option<i64>, String)> {
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
fn folder_picker(
    d: &Form,
    lib: &Library,
    selected: Option<i64>,
) -> (gtk::DropDown, Vec<Option<i64>>) {
    let options = folders(lib);
    let names = options.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>();
    let dd = gtk::DropDown::from_strings(&names);
    dd.set_selected(
        options
            .iter()
            .position(|(id, _)| *id == selected)
            .unwrap_or(0) as u32,
    );
    d.content_area().append(&gtk::Label::new(Some("Folder")));
    d.content_area().append(&dd);
    (dd, options.into_iter().map(|(id, _)| id).collect())
}
async fn confirm(parent: &impl IsA<gtk::Window>, heading: &str, body: &str) -> bool {
    let d = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    d.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    d.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    d.set_close_response("cancel");
    d.choose_future(Some(parent.as_ref())).await == "remove"
}
pub fn library(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let window = gtk::Window::builder()
            .title("Manage library")
            .transient_for(&u.window)
            .modal(true)
            .default_width(740)
            .default_height(650)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(16);
        root.set_margin_end(16);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
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
        let add_folder = gtk::Button::with_label("New folder");
        let add_feed = gtk::Button::with_label("Add feed");
        let edit = gtk::Button::with_label("Edit / Move");
        let delete = gtk::Button::with_label("Remove");
        delete.add_css_class("destructive-action");
        for b in [&add_folder, &add_feed, &edit, &delete] {
            toolbar.insert(b, -1);
        }
        root.append(&toolbar);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&list)
            .build();
        root.append(&scroller);
        let detail = gtk::Label::new(None);
        detail.set_wrap(true);
        detail.set_xalign(0.0);
        detail.set_selectable(true);
        root.append(&detail);
        let close = gtk::Button::with_label("Done");
        root.append(&close);
        window.set_child(Some(&root));
        let rows = Rc::new(RefCell::new(Vec::<Scope>::new()));
        let (tx, rx) = async_channel::unbounded::<String>();
        for (button, name) in [
            (add_folder, "folder"),
            (add_feed, "feed"),
            (edit, "edit"),
            (delete, "delete"),
        ] {
            let tx = tx.clone();
            button.connect_clicked(move |_| {
                let _ = tx.try_send(name.into());
            });
        }
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
        let tx_close = tx.clone();
        window.connect_close_request(move |_| {
            tx_close.close();
            glib::Propagation::Proceed
        });
        let rows2 = rows.clone();
        let u2 = u.clone();
        list.connect_row_selected(move |_, row| {
            let text = row
                .and_then(|r| rows2.borrow().get(r.index() as usize).cloned())
                .and_then(|s| {
                    if let Scope::Feed(id) = s {
                        u2.library
                            .borrow()
                            .feeds
                            .iter()
                            .find(|f| f.id == id)
                            .map(|f| {
                                format!(
                                    "{}{}",
                                    f.url,
                                    f.error
                                        .as_ref()
                                        .map(|e| format!("\nLast refresh: {e}"))
                                        .unwrap_or_default()
                                )
                            })
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            detail.set_text(&text);
        });
        window.present();
        loop {
            let lib = match u.db.call(|s| s.library()).await {
                Ok(l) => l,
                Err(e) => {
                    u.error(e);
                    break;
                }
            };
            *u.library.borrow_mut() = lib;
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            rows.borrow_mut().clear();
            {
                let lib = u.library.borrow();
                for (folder, path) in folders(&lib) {
                    if let Some(id) = folder {
                        let row = gtk::Label::new(Some(&format!("▾ {path}")));
                        row.set_xalign(0.0);
                        row.set_margin_top(12);
                        row.set_margin_bottom(12);
                        row.set_margin_start(12);
                        row.add_css_class("heading");
                        list.append(&row);
                        rows.borrow_mut().push(Scope::Folder(id));
                    }
                    for f in lib.feeds.iter().filter(|f| f.folder == folder) {
                        let row = gtk::Label::new(Some(&format!(
                            "    {}{}",
                            f.title,
                            if f.error.is_some() { "  ⚠" } else { "" }
                        )));
                        row.set_xalign(0.0);
                        row.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        row.set_margin_start(16);
                        row.set_margin_top(10);
                        row.set_margin_bottom(10);
                        list.append(&row);
                        rows.borrow_mut().push(Scope::Feed(f.id));
                    }
                }
            }
            let Ok(action) = rx.recv().await else {
                break;
            };
            let selected = list
                .selected_row()
                .and_then(|r| rows.borrow().get(r.index() as usize).cloned());
            if action == "delete" {
                let Some(selected) = selected else {
                    u.toast("Select a feed or folder first");
                    continue;
                };
                let (heading, body) = match selected {
                    Scope::Folder(_) => (
                        "Remove folder?",
                        "Feeds and child folders will move to its parent. Your articles and read state are kept.",
                    ),
                    _ => (
                        "Remove subscription?",
                        "This removes the feed and its downloaded articles, stars, and read state from Omafeed.",
                    ),
                };
                if confirm(&window, heading, body).await {
                    let result =
                        u.db.call(move |s| match selected {
                            Scope::Folder(id) => s.delete_folder(id),
                            Scope::Feed(id) => s.delete_feed(id),
                            _ => Ok(()),
                        })
                        .await;
                    if let Err(e) = result {
                        u.error(e);
                    } else {
                        u.query.borrow_mut().scope = Scope::All;
                        u.settings.borrow_mut().scope = Scope::All.encode();
                        *u.selected.borrow_mut() = None;
                        u.settings.borrow_mut().selected_article = None;
                        u.render();
                        u.save();
                        u.reload();
                    }
                }
                continue;
            }
            if action == "edit" && selected.is_none() {
                u.toast("Select a feed or folder first");
                continue;
            }
            let editing_folder = action == "folder"
                || (action == "edit" && matches!(selected, Some(Scope::Folder(_))));
            let d = dialog(
                &window,
                if editing_folder {
                    "Folder"
                } else {
                    "Subscription"
                },
            );
            let (name, url, parent, id) = {
                let lib = u.library.borrow();
                match (&*action, selected.as_ref()) {
                    ("edit", Some(Scope::Folder(id))) => {
                        let f = lib.folders.iter().find(|f| f.id == *id).unwrap();
                        (f.name.clone(), String::new(), f.parent, Some(f.id))
                    }
                    ("edit", Some(Scope::Feed(id))) => {
                        let f = lib.feeds.iter().find(|f| f.id == *id).unwrap();
                        (f.title.clone(), f.url.clone(), f.folder, Some(f.id))
                    }
                    _ => (
                        String::new(),
                        String::new(),
                        match selected {
                            Some(Scope::Folder(id)) => Some(id),
                            Some(Scope::Feed(id)) => {
                                lib.feeds.iter().find(|f| f.id == id).and_then(|f| f.folder)
                            }
                            _ => None,
                        },
                        None,
                    ),
                }
            };
            let name_entry = entry(&d, "Name", &name);
            let url_entry = if !editing_folder {
                let e = entry(&d, "Feed URL (RSS, Atom, or JSON Feed)", &url);
                Some(e)
            } else {
                None
            };
            let (picker, ids) = folder_picker(&d, &u.library.borrow(), parent);
            loop {
                if d.run_future().await != gtk::ResponseType::Accept {
                    break;
                }
                let name = name_entry.text().to_string();
                let parent = ids[picker.selected() as usize];
                let url = url_entry
                    .as_ref()
                    .map(|e| e.text().to_string())
                    .unwrap_or_default();
                let result =
                    u.db.call(move |s| {
                        if editing_folder {
                            if let Some(id) = id {
                                s.conn.execute_batch("SAVEPOINT folder_edit")?;
                                let r = s
                                    .rename_folder(id, &name)
                                    .and_then(|_| s.move_folder(id, parent));
                                if let Err(e) = r {
                                    s.conn.execute_batch(
                                        "ROLLBACK TO folder_edit; RELEASE folder_edit",
                                    )?;
                                    return Err(e);
                                }
                                s.conn.execute_batch("RELEASE folder_edit")?;
                            } else {
                                s.add_folder(&name, parent)?;
                            }
                        } else if let Some(id) = id {
                            s.update_feed(id, &name, &url, parent)?;
                        } else {
                            s.add_feed(&name, &url, parent)?;
                        }
                        Ok(())
                    })
                    .await;
                match result {
                    Ok(()) => {
                        u.reload();
                        if !editing_folder {
                            u.start_refresh(true);
                        }
                        break;
                    }
                    Err(e) => {
                        let msg = gtk::Label::new(Some(&e.to_string()));
                        msg.set_wrap(true);
                        msg.add_css_class("error");
                        d.content_area().append(&msg);
                    }
                }
            }
            drop(d);
        }
        window.close();
        u.reload();
    });
}

pub fn import(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let dialog = gtk::FileDialog::builder()
            .title("Import subscriptions")
            .build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("OPML subscriptions"));
        filter.add_pattern("*.opml");
        filter.add_pattern("*.xml");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        if let Ok(file) = dialog.open_future(Some(&u.window)).await
            && let Some(path) = file.path()
        {
            let result =
                u.db.call(move |s| {
                    let text = std::fs::read_to_string(path)?;
                    s.import(&text)
                })
                .await;
            match result {
                Ok(r) => {
                    u.toast(&format!(
                        "Imported {} · Already subscribed {} · Invalid {}",
                        r.added,
                        r.skipped,
                        r.invalid.len()
                    ));
                    if !r.invalid.is_empty() {
                        let d = adw::AlertDialog::builder()
                            .heading("Some subscriptions could not be imported")
                            .body(r.invalid.join("\n"))
                            .build();
                        d.add_response("ok", "OK");
                        d.present(Some(&u.window));
                    }
                    u.reload();
                    u.start_refresh(true);
                }
                Err(e) => u.error(e),
            }
        }
    });
}
pub fn export(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let dialog = gtk::FileDialog::builder()
            .title("Export subscriptions")
            .initial_name("Omafeed.opml")
            .build();
        if let Ok(file) = dialog.save_future(Some(&u.window)).await
            && let Some(path) = file.path()
        {
            match u
                .db
                .call(move |s| {
                    std::fs::write(path, s.export()?)?;
                    Ok(())
                })
                .await
            {
                Ok(()) => u.toast("Subscriptions exported"),
                Err(e) => u.error(e),
            }
        }
    });
}
pub fn settings(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let d = dialog(&u.window, "Settings");
        let current = u.settings.borrow().clone();
        let refresh = gtk::SpinButton::with_range(5.0, 1440.0, 5.0);
        refresh.set_value(current.refresh_minutes as f64);
        d.content_area().append(&gtk::Label::new(Some(
            "Refresh interval (minutes, while Omafeed is open)",
        )));
        d.content_area().append(&refresh);
        let font = gtk::SpinButton::with_range(12.0, 32.0, 1.0);
        font.set_value(current.font_size as f64);
        d.content_area()
            .append(&gtk::Label::new(Some("Article text size")));
        d.content_area().append(&font);
        let images = gtk::CheckButton::with_label("Load remote images when reading online");
        images.set_active(current.remote_images);
        d.content_area().append(&images);
        let themes = ["omarchy", "light", "dark"];
        let theme = gtk::DropDown::from_strings(&["Follow Omarchy", "Light", "Dark"]);
        theme.set_selected(themes.iter().position(|t| *t == current.theme).unwrap_or(0) as u32);
        d.content_area()
            .append(&gtk::Label::new(Some("Appearance")));
        d.content_area().append(&theme);
        if d.run_future().await == gtk::ResponseType::Accept {
            let mut s = u.settings.borrow_mut();
            s.font_size = font.value_as_int() as u32;
            s.refresh_minutes = refresh.value_as_int() as u32;
            s.remote_images = images.is_active();
            s.theme = themes[theme.selected() as usize].into();
            let palette = omafeed_core::settings::Palette::load(&s.theme);
            drop(s);
            *u.palette.borrow_mut() = palette;
            u.save();
            u.apply_theme();
            u.render();
        }
        drop(d);
    });
}
pub fn shortcuts(u: Rc<Ui>) {
    let d=adw::AlertDialog::builder().heading("Keyboard shortcuts").body("J / K or ↓ / ↑   Next / previous article\nN   Next unread article\nM   Toggle read\nS   Toggle star\nO   Open original\nSpace / Shift+Space   Scroll article\nCtrl+R   Refresh\nCtrl+F   Search\nCtrl+L   Manage library\nCtrl+Shift+A   Mark current view read\nCtrl+Z   Undo bulk mark read\nCtrl+Q   Quit\n?   Show shortcuts").build();
    d.add_response("ok", "Done");
    d.present(Some(&u.window));
}
pub fn about(u: Rc<Ui>) {
    let d = adw::AboutDialog::builder()
        .application_name("Omafeed")
        .application_icon("io.github.pch.Omafeed")
        .developer_name("pch")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/pch/omafeed")
        .license_type(gtk::License::MitX11)
        .comments("A quiet home for your feeds. Built with Rust, GTK, and WebKitGTK.")
        .build();
    d.present(Some(&u.window));
}
