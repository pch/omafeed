//! Modal dialogs: feed and folder forms, OPML import/export, content filters, settings, and help.
mod filters;
mod library;

use crate::ui::Ui;
use adw::prelude::*;
use gtk::{gio, glib};
use omafeed_core::{Settings, config::Theme};
use std::{cell::Cell, rc::Rc};

pub use filters::filters;
pub use library::{add_feed, edit, new_folder, remove};

/// A dialog of grouped form rows under a header bar with Cancel and a suggested action. It
/// stays open across attempts, so a failed save can show its error beside the fields, and
/// closes when dropped.
struct Form {
    dialog: adw::Dialog,
    fields: gtk::Box,
    error: gtk::Label,
    save: gtk::Button,
    parent: gtk::Widget,
    responses: async_channel::Receiver<bool>,
    presented: Cell<bool>,
    closed: Rc<Cell<bool>>,
}

impl Form {
    fn new(parent: &impl IsA<gtk::Widget>, title: &str, save_label: &str) -> Self {
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::with_label(save_label);
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let groups = gtk::Box::new(gtk::Orientation::Vertical, 12);
        groups.set_margin_top(12);
        groups.set_margin_bottom(24);
        groups.set_margin_start(24);
        groups.set_margin_end(24);
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 24);
        groups.append(&fields);
        let error = gtk::Label::new(None);
        error.set_wrap(true);
        error.set_xalign(0.0);
        error.add_css_class("error");
        error.set_visible(false);
        groups.append(&error);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&groups));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(520)
            .child(&view)
            .default_widget(&save)
            .build();
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
            fields,
            error,
            save,
            parent: parent.clone().upcast(),
            responses,
            presented: Cell::new(false),
            closed,
        }
    }

    /// A new group of rows, with an optional title and description above it.
    fn group(&self, title: &str, description: Option<&str>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title(glib::markup_escape_text(title))
            .build();
        if let Some(d) = description {
            group.set_description(Some(&glib::markup_escape_text(d)));
        }
        self.fields.append(&group);
        group
    }

    /// Show an error below the fields, replacing any previous one.
    fn show_error(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(true);
    }

    /// Present the form, or bring it back for another attempt; returns true if the user
    /// chose the suggested action, false once they cancel or close it.
    async fn run(&self) -> bool {
        if self.closed.get() {
            return false;
        }
        self.save.set_sensitive(true);
        if !self.presented.replace(true) {
            self.dialog.present(Some(&self.parent));
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

/// A text field that saves the form on Enter.
fn entry_row(group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .activates_default(true)
        .build();
    row.set_text(value);
    group.add(&row);
    row
}

/// A choice among `names`, starting at `selected`.
fn combo_row(
    group: &adw::PreferencesGroup,
    title: &str,
    names: &[&str],
    selected: usize,
) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(names))
        .selected(u32::try_from(selected).unwrap_or(0))
        .build();
    group.add(&row);
    row
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
                // Imported feeds have never been fetched, so they are all due.
                u.start_refresh(omafeed_core::ingest::Which::Due);
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
        let form = Form::new(&u.window, "Settings", "Save");
        let current = u.settings.borrow().clone();
        let reading = form.group("Reading", None);
        let sizes = Settings::FONT_SIZES;
        let font = adw::SpinRow::with_range((*sizes.start()).into(), (*sizes.end()).into(), 1.0);
        font.set_title("Text size");
        font.set_value(current.font_size.into());
        reading.add(&font);
        let images = adw::SwitchRow::builder()
            .title("Load remote images")
            .subtitle("When reading articles online")
            .active(current.remote_images)
            .build();
        reading.add(&images);
        let labels = Theme::ALL.map(Theme::label);
        let selected = Theme::ALL.iter().position(|t| *t == current.theme);
        let theme = combo_row(&reading, "Appearance", &labels, selected.unwrap_or(0));
        let feeds = form.group("Feeds", None);
        let minutes = Settings::REFRESH_MINUTES;
        let refresh =
            adw::SpinRow::with_range((*minutes.start()).into(), (*minutes.end()).into(), 5.0);
        refresh.set_title("Refresh every");
        refresh.set_subtitle("Minutes, while Omafeed is open");
        refresh.set_value(current.refresh_minutes.into());
        feeds.add(&refresh);
        if !form.run().await {
            return;
        }
        let mut s = u.settings.borrow().clone();
        s.font_size = font.value() as u32;
        s.refresh_minutes = refresh.value() as u32;
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
Ctrl+N   Add feed
Ctrl+Shift+N   New folder
Menu / Shift+F10   Menu of the sidebar item
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
