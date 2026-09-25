//! Window actions, accelerators, and single-key reading shortcuts.
use super::Ui;
use gtk::{gdk, gio, glib, prelude::*};
use std::rc::Rc;
use webkit6::prelude::*;

/// Scroll one screen; evaluates to `true` when already at the bottom.
const SCROLL_DOWN: &str = "(() => {
    if (window.scrollY + window.innerHeight >= document.documentElement.scrollHeight - 4) return true;
    window.scrollBy(0, window.innerHeight * 0.85);
    return false;
})()";
const SCROLL_UP: &str = "window.scrollBy(0, -window.innerHeight * 0.85); false";

impl Ui {
    fn action(self: &Rc<Self>, name: &str, callback: impl Fn(Rc<Self>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(u) = weak.upgrade() {
                callback(u);
            }
        });
        self.window.add_action(&action);
    }

    pub(super) fn actions(self: &Rc<Self>, app: &adw::Application) {
        use crate::dialogs;
        self.action("library", dialogs::library);
        self.action("import", dialogs::import);
        self.action("export", dialogs::export);
        self.action("settings", dialogs::settings);
        self.action("shortcuts", dialogs::shortcuts);
        self.action("about", dialogs::about);
        self.action("refresh", |u| u.start_refresh(true));
        self.action("search", |u| {
            u.header.search.grab_focus();
        });
        self.action("undo", |u| u.undo());
        self.action("quit", |u| u.window.close());
        self.action("mark", |u| u.mark_all());
        for (action, accel) in [
            ("refresh", "<Control>r"),
            ("search", "<Control>f"),
            ("undo", "<Control>z"),
            ("quit", "<Control>q"),
            ("library", "<Control>l"),
            ("mark", "<Control><Shift>a"),
        ] {
            app.set_accels_for_action(&format!("win.{action}"), &[accel]);
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = u)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, mods| u.on_key(key, mods)
        ));
        self.window.add_controller(keys);
    }

    fn typing(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|w| {
            w.is::<gtk::Text>() || w.is::<gtk::Entry>() || w.is::<gtk::SearchEntry>()
        })
    }

    fn on_key(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
        let modified = gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK;
        if mods.intersects(modified) || self.typing() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::j | gdk::Key::Down => self.navigate(1, false),
            gdk::Key::k | gdk::Key::Up => self.navigate(-1, false),
            gdk::Key::n => self.navigate(1, true),
            gdk::Key::m => self.toggle_read(),
            gdk::Key::s => self.toggle_star(),
            gdk::Key::o => self.open_original(),
            gdk::Key::space => self.scroll_page(mods.contains(gdk::ModifierType::SHIFT_MASK)),
            gdk::Key::question => crate::dialogs::shortcuts(self.clone()),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// Scroll the reader by a screen; at the bottom, move to the next unread article.
    fn scroll_page(self: &Rc<Self>, backwards: bool) {
        let script = if backwards { SCROLL_UP } else { SCROLL_DOWN };
        let weak = Rc::downgrade(self);
        self.reader.web.evaluate_javascript(
            script,
            Some("omafeed"),
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if result.is_ok_and(|v| v.to_boolean())
                    && let Some(u) = weak.upgrade()
                {
                    u.navigate(1, true);
                }
            },
        );
    }
}
