//! Modal dialogs: feed and folder forms, OPML import/export, content filters, the Miniflux
//! account, settings, and help.
mod account;
mod filters;
mod library;

use crate::ui::{Ui, widgets::label};
use adw::prelude::*;
use gtk::{gio, glib};
use omafeed_core::{Settings, config::Theme};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub use account::account;
pub use filters::filters;
pub use library::{add_feed, edit, new_folder, remove};

/// A form dialog in omafeed's own style: a title, labeled fields, and Cancel and a primary
/// action at the bottom. It stays open across attempts, so a failed save can show its error
/// beside the fields, and closes when dropped.
struct Form {
    dialog: adw::Dialog,
    /// The title, an optional note, and the fields, in order.
    body: gtk::Box,
    error: gtk::Label,
    cancel: gtk::Button,
    save: gtk::Button,
    parent: gtk::Widget,
    responses: async_channel::Receiver<bool>,
    presented: Cell<bool>,
    closed: Rc<Cell<bool>>,
    /// The widget to focus once the dialog is shown.
    focus: RefCell<Option<gtk::Widget>>,
}

impl Form {
    fn new(parent: &impl IsA<gtk::Widget>, title: &str, save_label: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("form");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let heading = label(title, "form-title");
        body.append(&heading);
        root.append(&body);
        let error = label("", "form-error");
        error.set_wrap(true);
        error.set_visible(false);
        root.append(&error);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("flat");
        let save = gtk::Button::with_label(save_label);
        save.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.add_css_class("form-actions");
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&save);
        root.append(&actions);
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(460)
            .child(&root)
            .default_widget(&save)
            .build();
        dialog.add_css_class("form-dialog");
        let (tx, responses) = async_channel::unbounded();
        let closed = Rc::new(Cell::new(false));
        save.connect_clicked(glib::clone!(
            #[strong]
            tx,
            move |save| {
                // Until the caller has dealt with this attempt and asks again.
                save.set_sensitive(false);
                let _ = tx.try_send(true);
            }
        ));
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        dialog.connect_closed(glib::clone!(
            #[strong]
            closed,
            move |_| {
                closed.set(true);
                let _ = tx.try_send(false);
            }
        ));
        Self {
            dialog,
            body,
            error,
            cancel,
            save,
            parent: parent.clone().upcast(),
            responses,
            presented: Cell::new(false),
            closed,
            focus: RefCell::default(),
        }
    }

    /// Only the primary action, for a dialog that has nothing to cancel.
    fn without_cancel(self) -> Self {
        self.cancel.set_visible(false);
        self
    }

    /// Wider than the default, for content laid out in columns.
    fn width(self, width: i32) -> Self {
        self.dialog.set_content_width(width);
        self
    }

    /// A line of explanation under the title.
    fn note(&self, text: &str) {
        let note = label(text, "form-note");
        note.set_wrap(true);
        note.set_margin_top(-8);
        self.body.append(&note);
    }

    /// A heading over the fields that follow, for forms with several groups.
    fn section(&self, title: &str) {
        let heading = label(title, "form-section");
        self.body.append(&heading);
    }

    /// Add `widget` below the fields so far.
    fn append(&self, widget: &impl IsA<gtk::Widget>) {
        self.body.append(widget);
    }

    /// `widget` with a label above it.
    fn field(&self, title: &str, widget: &impl IsA<gtk::Widget>) {
        let field = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let caption = label(title, "form-label");
        caption.set_mnemonic_widget(Some(widget));
        field.append(&caption);
        field.append(widget);
        self.body.append(&field);
    }

    /// A text field that saves the form on Enter.
    fn entry(&self, title: &str, value: &str) -> gtk::Entry {
        let entry = gtk::Entry::builder()
            .text(value)
            .activates_default(true)
            .build();
        self.field(title, &entry);
        entry
    }

    /// A choice among `names`, starting at `selected`.
    fn choice(&self, title: &str, names: &[&str], selected: usize) -> gtk::DropDown {
        let dropdown = gtk::DropDown::from_strings(names);
        dropdown.set_selected(u32::try_from(selected).unwrap_or(0));
        self.field(title, &dropdown);
        dropdown
    }

    /// A setting: its name, with an optional explanation, beside `control`.
    fn setting(&self, title: &str, subtitle: Option<&str>, control: &impl IsA<gtk::Widget>) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("form-setting");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&label(title, "form-setting-title"));
        if let Some(s) = subtitle {
            text.append(&label(s, "form-note"));
        }
        row.append(&text);
        control.as_ref().set_valign(gtk::Align::Center);
        row.append(control);
        self.body.append(&row);
    }

    /// Focus `widget` when the form is shown.
    fn focus(&self, widget: &impl IsA<gtk::Widget>) {
        self.focus.replace(Some(widget.clone().upcast()));
    }

    /// Show an error below the fields, replacing any previous one.
    fn show_error(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(true);
    }

    /// Present the form, or bring it back for another attempt; returns true if the user
    /// chose the primary action, false once they cancel or close it.
    async fn run(&self) -> bool {
        if self.closed.get() {
            return false;
        }
        self.save.set_sensitive(true);
        if !self.presented.replace(true) {
            self.dialog.present(Some(&self.parent));
            if let Some(widget) = self.focus.take() {
                self.dialog.set_focus(Some(&widget));
            }
        }
        self.responses.recv().await.unwrap_or(false) && !self.closed.get()
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        if !self.closed.get() {
            self.dialog.close();
        }
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
    confirm_as(parent, heading, body, "Remove").await
}

/// Ask whether to do something that cannot be undone, named `action` on its button.
async fn confirm_as(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    action: &str,
) -> bool {
    let d = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    d.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
    d.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "confirm"
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
        let result = u
            .change_library(move |db, client| async move {
                omafeed_core::subscriptions::import(&db, client.as_ref(), text).await
            })
            .await;
        let done = match result {
            Ok(done) => done,
            Err(e) => return u.error(e),
        };
        let r = &done.report;
        let mut text = format!(
            "Imported {} · Already subscribed {} · Invalid {}",
            r.added,
            r.skipped,
            r.invalid.len()
        );
        if let Some(Ok(synced)) = &done.synced
            && synced.subscribed > 0
        {
            text.push_str(&format!(" · {} added to Miniflux", synced.subscribed));
        }
        u.toast(&text);
        if !r.invalid.is_empty() {
            let d = adw::AlertDialog::builder()
                .heading("Some subscriptions could not be imported")
                .body(r.invalid.join("\n"))
                .build();
            d.add_response("ok", "OK");
            d.present(Some(&u.window));
        }
        u.reload();
        match done.synced {
            // Imported feeds have never been fetched, so they are all due.
            None => u.start_refresh(omafeed_core::ingest::Which::Due),
            Some(Ok(synced)) if !synced.refused.is_empty() => {
                account::refused(&u, &synced.refused);
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => u.error(format!(
                "The sync with Miniflux failed: {e:#}. The imported feeds go to Miniflux with \
                 the next one."
            )),
        }
    });
}

pub fn export(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let dialog = gtk::FileDialog::builder()
            .title("Export subscriptions")
            .initial_name("omafeed.opml")
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
        let form = Form::new(&u.window, "Settings", "Save");
        let current = u.settings.borrow().clone();
        form.section("READING");
        let sizes = Settings::FONT_SIZES;
        let font = gtk::SpinButton::with_range((*sizes.start()).into(), (*sizes.end()).into(), 1.0);
        font.set_value(current.font_size.into());
        form.setting("Text size", None, &font);
        let images = gtk::Switch::builder().active(current.remote_images).build();
        form.setting(
            "Load remote images",
            Some("When reading articles online"),
            &images,
        );
        let labels = Theme::ALL.map(Theme::label);
        let selected = Theme::ALL.iter().position(|t| *t == current.theme);
        let theme = gtk::DropDown::from_strings(&labels);
        theme.set_selected(selected.unwrap_or(0) as u32);
        form.setting("Appearance", None, &theme);
        form.section("FEEDS");
        let minutes = Settings::REFRESH_MINUTES;
        let refresh =
            gtk::SpinButton::with_range((*minutes.start()).into(), (*minutes.end()).into(), 5.0);
        refresh.set_value(current.refresh_minutes.into());
        let note = if u.library.borrow().account.is_some() {
            "Minutes between syncs with Miniflux, while omafeed is open"
        } else {
            "Minutes, while omafeed is open"
        };
        form.setting("Refresh every", Some(note), &refresh);
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

/// The shortcuts dialog's columns: sections of actions, each with its keys. A key's `+`
/// joins keys pressed together; several keys for one action are alternatives.
/// An action and its keys.
type Shortcut = (&'static str, &'static [&'static str]);
/// A titled section of shortcuts.
type Section = (&'static str, &'static [Shortcut]);

const SHORTCUTS: [&[Section]; 2] = [
    &[(
        "READING",
        &[
            ("Next article", &["J", "↓"]),
            ("Previous article", &["K", "↑"]),
            ("Next unread article", &["N"]),
            ("Scroll article", &["Space"]),
            ("Scroll back", &["Shift+Space"]),
            ("Toggle read", &["M"]),
            ("Toggle star", &["S"]),
            ("Open original", &["O"]),
        ],
    )],
    &[
        (
            "LIBRARY",
            &[
                ("Refresh", &["Ctrl+R"]),
                ("Search", &["Ctrl+F"]),
                ("Add feed", &["Ctrl+N"]),
                ("New folder", &["Ctrl+Shift+N"]),
                ("Mark view as read", &["Ctrl+Shift+A"]),
                ("Undo mark as read", &["Ctrl+Z"]),
                ("Context menu", &["Menu", "Shift+F10"]),
            ],
        ),
        (
            "OMAFEED",
            &[("Keyboard shortcuts", &["?"]), ("Quit", &["Ctrl+Q"])],
        ),
    ],
];

/// `keys` as key caps: alternatives apart, keys pressed together side by side.
fn key_caps(keys: &[&str]) -> gtk::Box {
    let caps = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    caps.set_valign(gtk::Align::Center);
    for (i, alternative) in keys.iter().enumerate() {
        if i > 0 {
            caps.append(&label("or", "shortcut-or"));
        }
        let chord = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        for key in alternative.split('+') {
            chord.append(&label(key, "keycap"));
        }
        caps.append(&chord);
    }
    caps
}

pub fn shortcuts(u: Rc<Ui>) {
    glib::spawn_future_local(async move {
        let form = Form::new(&u.window, "Keyboard shortcuts", "Done")
            .without_cancel()
            .width(640);
        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 32);
        columns.set_homogeneous(true);
        for sections in SHORTCUTS {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
            for (i, (title, rows)) in sections.iter().enumerate() {
                let heading = label(title, "form-section");
                if i > 0 {
                    heading.set_margin_top(18);
                }
                column.append(&heading);
                for (action, keys) in *rows {
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                    row.add_css_class("shortcut");
                    let name = label(action, "shortcut-action");
                    name.set_hexpand(true);
                    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    row.append(&name);
                    row.append(&key_caps(keys));
                    column.append(&row);
                }
            }
            columns.append(&column);
        }
        form.append(&columns);
        form.run().await;
    });
}

pub fn about(u: Rc<Ui>) {
    let d = adw::AboutDialog::builder()
        .application_name("omafeed")
        .application_icon(crate::APP_ID)
        .developer_name("pch")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/pch/omafeed")
        .license_type(gtk::License::MitX11)
        .comments("A quiet home for your feeds. Built with Rust, GTK, and WebKitGTK.")
        .build();
    d.present(Some(&u.window));
}
