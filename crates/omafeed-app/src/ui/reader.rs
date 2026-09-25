//! Reading pane: a locked-down WebKit view plus article actions.
use super::{Ui, widgets::icon, widgets::margins};
use adw::prelude::*;
use gtk::{gio, glib};
use omafeed_core::db::Article;
use std::rc::Rc;
use webkit6::prelude::*;

pub(super) struct ReaderPane {
    pub root: gtk::Box,
    pub back: gtk::Button,
    read: gtk::Button,
    star: gtk::Button,
    open: gtk::Button,
    copy: gtk::Button,
    pub web: webkit6::WebView,
    stack: gtk::Stack,
}

impl ReaderPane {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&toolbar, 10);
        let back = icon("go-previous-symbolic", "Show article list");
        toolbar.append(&back);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        toolbar.append(&spacer);
        let read = icon("mail-unread-symbolic", "Mark unread (M)");
        let star = icon("non-starred-symbolic", "Star (S)");
        let copy = icon("edit-copy-symbolic", "Copy article link");
        let open = icon("adw-external-link-symbolic", "Open original (O)");
        for b in [&read, &star, &copy, &open] {
            toolbar.append(b);
        }
        root.append(&toolbar);
        let web = web_view();
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let empty = adw::StatusPage::builder()
            .icon_name("application-rss+xml-symbolic")
            .title("A little space to read")
            .description(
                "Choose an article, or import your subscriptions\nfrom the menu to get started.",
            )
            .build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&web, Some("article"));
        root.append(&stack);
        Self {
            root,
            back,
            read,
            star,
            open,
            copy,
            web,
            stack,
        }
    }
}

fn web_view() -> webkit6::WebView {
    let web = webkit6::WebView::new();
    web.set_hexpand(true);
    web.set_vexpand(true);
    if let Some(s) = WebViewExt::settings(&web) {
        // JavaScript stays enabled only so the app can call `evaluate_javascript`
        // (Space scrolling). Page scripts cannot run: markup JavaScript is off, the
        // document CSP is `default-src 'none'`, and ammonia strips scripts.
        s.set_enable_javascript(true);
        s.set_enable_javascript_markup(false);
        s.set_enable_html5_database(false);
        s.set_enable_html5_local_storage(false);
        s.set_media_playback_requires_user_gesture(true);
        s.set_enable_page_cache(false);
    }
    web.connect_decide_policy(|_, decision, kind| {
        if !matches!(
            kind,
            webkit6::PolicyDecisionType::NavigationAction
                | webkit6::PolicyDecisionType::NewWindowAction
        ) {
            return false;
        }
        let Some(mut action) = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|nav| nav.navigation_action())
        else {
            return false;
        };
        let uri = action
            .request()
            .and_then(|r| r.uri())
            .map(|u| u.to_string());
        // Clicked links open in the browser; nothing else may navigate the reader.
        if action.navigation_type() == webkit6::NavigationType::LinkClicked {
            if let Some(uri) = &uri {
                open_uri(uri);
            }
            decision.ignore();
            return true;
        }
        if kind == webkit6::PolicyDecisionType::NewWindowAction
            || uri.is_some_and(|u| u != "about:blank")
        {
            decision.ignore();
            return true;
        }
        false
    });
    web
}

/// Open web and mail links with the default handler; other schemes are ignored.
pub fn open_uri(uri: &str) {
    if !(uri.starts_with("https://") || uri.starts_with("http://") || uri.starts_with("mailto:")) {
        return;
    }
    if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        eprintln!("Open link: {e}");
    }
}

impl Ui {
    pub(super) fn connect_reader_signals(self: &Rc<Self>) {
        use glib::clone;
        let r = &self.reader;
        r.star.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.toggle_star()
        ));
        r.read.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.toggle_read()
        ));
        r.open.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.open_original()
        ));
        r.copy.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.copy_link()
        ));
        r.back.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.show_reader(false)
        ));
        r.web.connect_load_changed(clone!(
            #[weak(rename_to = u)]
            self,
            move |_, event| {
                if event == webkit6::LoadEvent::Finished {
                    u.mark_opened();
                }
            }
        ));
    }

    pub(super) fn render(&self) {
        let selected = self.selected.borrow();
        let Some(a) = selected.as_ref() else {
            self.reader.stack.set_visible_child_name("empty");
            return;
        };
        let s = self.settings.borrow();
        let html = omafeed_core::article::document(
            a,
            &self.palette.borrow(),
            s.font_size,
            s.remote_images,
        );
        self.reader.stack.set_visible_child_name("article");
        self.reader.web.load_html(&html, Some("about:blank"));
        self.reader.open.set_sensitive(!a.url.is_empty());
        self.reader.copy.set_sensitive(!a.url.is_empty());
        self.update_buttons(a);
    }

    pub(super) fn update_buttons(&self, a: &Article) {
        let r = &self.reader;
        let (star_icon, star_tip) = if a.starred {
            ("starred-symbolic", "Unstar (S)")
        } else {
            ("non-starred-symbolic", "Star (S)")
        };
        r.star.set_icon_name(star_icon);
        r.star.set_tooltip_text(Some(star_tip));
        let (read_icon, read_tip) = if a.read {
            ("mail-unread-symbolic", "Mark unread (M)")
        } else {
            ("mail-read-symbolic", "Mark read (M)")
        };
        r.read.set_icon_name(read_icon);
        r.read.set_tooltip_text(Some(read_tip));
    }

    /// Mark the article read once its content has loaded (not on re-renders).
    fn mark_opened(self: &Rc<Self>) {
        let Some(loaded) = self.pending_mark.take() else {
            return;
        };
        let id = {
            let mut selected = self.selected.borrow_mut();
            match selected.as_mut() {
                Some(a) if a.id == loaded && !a.read => {
                    a.read = true;
                    self.update_buttons(a);
                    a.id
                }
                _ => return,
            }
        };
        self.set_state(id, Some(true), None);
    }

    pub(super) fn open_original(&self) {
        if let Some(a) = self.selected.borrow().as_ref() {
            open_uri(&a.url);
        }
    }

    fn copy_link(&self) {
        if let Some(a) = self.selected.borrow().as_ref()
            && !a.url.is_empty()
        {
            self.window.clipboard().set_text(&a.url);
            self.toast("Link copied");
        }
    }
}
