//! Application CSS and live Omarchy palette following.
use super::Ui;
use gtk::{gio, glib, prelude::*};
use omafeed_core::settings::Palette;
use std::{rc::Rc, time::Duration};

/// Theme switches touch several files; wait for them to settle before reloading.
const THEME_SETTLE: Duration = Duration::from_millis(300);

impl Ui {
    pub(super) fn apply_theme(&self) {
        let p = self.palette.borrow();
        adw::StyleManager::default().set_color_scheme(if p.dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        });
        let css = include_str!("../../../../data/style.css")
            .replace("{{background}}", &p.background)
            .replace("{{foreground}}", &p.foreground)
            .replace("{{accent}}", &p.accent);
        self.css.load_from_string(&css);
    }

    /// Reload the palette; with `force`, re-render even if it did not change.
    pub(super) fn reload_palette(&self, force: bool) {
        let palette = Palette::load(self.settings.borrow().theme);
        let changed = palette != *self.palette.borrow();
        if changed {
            *self.palette.borrow_mut() = palette;
            self.apply_theme();
        }
        if changed || force {
            self.render();
        }
    }

    /// Watch the Omarchy theme directory and its parent, since switching themes
    /// may rewrite `colors.toml` or replace the whole directory.
    pub(super) fn watch_theme(self: &Rc<Self>) {
        let mut monitors = Vec::new();
        for file in Palette::omarchy_files() {
            let theme_dir = file.parent();
            let current_dir = theme_dir.and_then(|d| d.parent());
            for dir in [theme_dir, current_dir].into_iter().flatten() {
                let Ok(monitor) = gio::File::for_path(dir).monitor_directory(
                    gio::FileMonitorFlags::WATCH_MOVES,
                    None::<&gio::Cancellable>,
                ) else {
                    continue;
                };
                monitor.connect_changed(glib::clone!(
                    #[weak(rename_to = u)]
                    self,
                    move |_, _, _, _| u.theme_files_changed()
                ));
                monitors.push(monitor);
            }
        }
        *self.theme_monitors.borrow_mut() = monitors;
    }

    fn theme_files_changed(self: &Rc<Self>) {
        if self.theme_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(THEME_SETTLE, move || {
            if let Some(u) = weak.upgrade() {
                u.theme_pending.set(false);
                u.reload_palette(false);
                // Re-arm in case the watched directory itself was replaced.
                u.watch_theme();
            }
        });
    }
}
