//! Modal dialogs: library management, OPML import/export, settings, and help.
mod library;

use crate::ui::Ui;
use adw::prelude::*;
use gtk::{gio, glib};
use omafeed_core::settings::Theme;
use std::rc::Rc;

pub use library::library;

/// A Cancel/Save dialog around a box of form fields, reusable across attempts.
struct Form {
    heading: String,
    save_label: String,
    content: gtk::Box,
    error: gtk::Label,
    parent: gtk::Window,
}

impl Form {
    fn new(parent: &impl IsA<gtk::Window>, heading: &str) -> Self {
        let error = gtk::Label::new(None);
        error.set_wrap(true);
        error.add_css_class("error");
        Self {
            heading: heading.into(),
            save_label: "Save".into(),
            content: gtk::Box::new(gtk::Orientation::Vertical, 12),
            error,
            parent: parent.clone().upcast(),
        }
    }

    fn save_label(mut self, label: &str) -> Self {
        self.save_label = label.into();
        self
    }

    fn append(&self, widget: &impl IsA<gtk::Widget>) {
        self.content.append(widget);
    }

    fn entry(&self, title: &str, value: &str) -> gtk::Entry {
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        self.append(&label);
        let entry = gtk::Entry::builder()
            .text(value)
            .activates_default(true)
            .build();
        self.append(&entry);
        entry
    }

    fn labeled(&self, title: &str, widget: &impl IsA<gtk::Widget>) {
        self.append(&gtk::Label::new(Some(title)));
        self.append(widget);
    }

    /// Show an error below the fields, replacing any previous one.
    fn show_error(&self, message: &str) {
        if self.error.parent().is_none() {
            self.append(&self.error);
        }
        self.error.set_text(message);
    }

    /// Present the form; returns true if the user chose Save.
    async fn run(&self) -> bool {
        // A fresh dialog each time avoids reparenting into a previously closed AdwDialog.
        let dialog = adw::AlertDialog::builder()
            .heading(&self.heading)
            .content_width(480)
            .extra_child(&self.content)
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("save", &self.save_label)]);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let closed = wait_closed(&dialog);
        let response = dialog.clone().choose_future(Some(&self.parent)).await;
        closed.await;
        dialog.set_extra_child(None::<&gtk::Widget>);
        response == "save"
    }
}

/// Resolves once the dialog has fully closed, so its child can be detached safely.
fn wait_closed(dialog: &impl IsA<adw::Dialog>) -> impl std::future::Future<Output = ()> {
    let (tx, rx) = async_channel::bounded(1);
    dialog.connect_closed(move |_| {
        let _ = tx.try_send(());
    });
    async move {
        let _ = rx.recv().await;
    }
}

async fn confirm(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) -> bool {
    let d = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    d.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    d.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "remove"
}

pub fn import(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("OPML subscriptions"));
        filter.add_pattern("*.opml");
        filter.add_pattern("*.xml");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import subscriptions")
            .filters(&filters)
            .build();
        let Ok(file) = dialog.open_future(Some(&u.window)).await else {
            return;
        };
        let text = match file.load_contents_future().await {
            Ok((bytes, _)) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => return u.error(e),
        };
        match u.db.call(move |s| s.import(&text)).await {
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
    });
}

pub fn export(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let dialog = gtk::FileDialog::builder()
            .title("Export subscriptions")
            .initial_name("Omafeed.opml")
            .build();
        let Ok(file) = dialog.save_future(Some(&u.window)).await else {
            return;
        };
        let opml = match u.db.call(|s| s.export()).await {
            Ok(opml) => opml,
            Err(e) => return u.error(e),
        };
        let result = file
            .replace_contents_future(
                opml.into_bytes(),
                None,
                false,
                gio::FileCreateFlags::REPLACE_DESTINATION,
            )
            .await;
        match result {
            Ok(_) => u.toast("Subscriptions exported"),
            Err((_, e)) => u.error(e),
        }
    });
}

pub fn settings(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let form = Form::new(&u.window, "Settings");
        let current = u.settings.borrow().clone();
        let refresh = gtk::SpinButton::with_range(5.0, 1440.0, 5.0);
        refresh.set_value(current.refresh_minutes.into());
        form.labeled(
            "Refresh interval (minutes, while Omafeed is open)",
            &refresh,
        );
        let font = gtk::SpinButton::with_range(12.0, 32.0, 1.0);
        font.set_value(current.font_size.into());
        form.labeled("Article text size", &font);
        let images = gtk::CheckButton::with_label("Load remote images when reading online");
        images.set_active(current.remote_images);
        form.append(&images);
        let labels = Theme::ALL.map(Theme::label);
        let theme = gtk::DropDown::from_strings(&labels);
        let selected = Theme::ALL.iter().position(|t| *t == current.theme);
        theme.set_selected(selected.unwrap_or(0) as u32);
        form.labeled("Appearance", &theme);
        if !form.run().await {
            return;
        }
        let mut s = u.settings.borrow().clone();
        s.font_size = font.value_as_int() as u32;
        s.refresh_minutes = refresh.value_as_int() as u32;
        s.remote_images = images.is_active();
        s.theme = Theme::ALL
            .get(theme.selected() as usize)
            .copied()
            .unwrap_or_default();
        u.apply_settings(s);
    });
}

const SHORTCUTS: &str = "\
J / K or ↓ / ↑   Next / previous article
N   Next unread article
M   Toggle read
S   Toggle star
O   Open original
Space / Shift+Space   Scroll article
Ctrl+R   Refresh
Ctrl+F   Search
Ctrl+L   Manage library
Ctrl+Shift+A   Mark current view read
Ctrl+Z   Undo bulk mark read
Ctrl+Q   Quit
?   Show shortcuts";

pub fn shortcuts(u: Rc<Ui>) {
    let d = adw::AlertDialog::builder()
        .heading("Keyboard shortcuts")
        .body(SHORTCUTS)
        .build();
    d.add_response("ok", "Done");
    d.present(Some(&u.window));
}

pub fn about(u: Rc<Ui>) {
    let d = adw::AboutDialog::builder()
        .application_name("Omafeed")
        .application_icon(crate::APP_ID)
        .developer_name("pch")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/pch/omafeed")
        .license_type(gtk::License::MitX11)
        .comments("A quiet home for your feeds. Built with Rust, GTK, and WebKitGTK.")
        .build();
    d.present(Some(&u.window));
}
