//! Reading pane: a locked-down WebKit view plus article actions.
use super::{Ui, widgets::icon, widgets::margins};
use adw::prelude::*;
use gtk::{gio, glib};
use omafeed_core::{
    full_article,
    model::{Article, ArticleId},
    render::{Source, document},
};
use std::{cell::RefCell, collections::HashSet, rc::Rc};
use webkit6::prelude::*;

const FETCH_FULL: &str = "Fetch the full article from its web page (D)";

pub(super) struct ReaderPane {
    pub root: gtk::Box,
    pub back: gtk::Button,
    read: gtk::Button,
    star: gtk::Button,
    full: gtk::Button,
    /// The articles being fetched in full, whose button waits until they are.
    fetching: RefCell<HashSet<ArticleId>>,
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
        let full = icon("folder-download-symbolic", FETCH_FULL);
        let copy = icon("edit-copy-symbolic", "Copy article link");
        let open = icon("adw-external-link-symbolic", "Open original (O)");
        for b in [&read, &star, &full, &copy, &open] {
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
            full,
            fetching: RefCell::default(),
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
            .and_then(webkit6::NavigationPolicyDecision::navigation_action)
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

/// Whether [`open_uri`] opens `uri`: web and mail links only.
pub fn opens(uri: &str) -> bool {
    uri.starts_with("https://") || uri.starts_with("http://") || uri.starts_with("mailto:")
}

/// Open web and mail links with the default handler; other schemes are ignored.
pub fn open_uri(uri: &str) {
    if !opens(uri) {
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
        r.full.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.toggle_full()
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
        let view = self.view.borrow();
        let Some(a) = view.open() else {
            self.reader.stack.set_visible_child_name("empty");
            return;
        };
        let site = self
            .library
            .borrow()
            .feed(a.feed_id)
            .map(|f| f.website().to_owned())
            .unwrap_or_default();
        // Favicons are small normalized PNGs, so reading one per render is cheap.
        let icon = (!site.is_empty())
            .then(|| {
                std::fs::read(omafeed_core::ingest::icons::path(&self.paths.cache, &site)).ok()
            })
            .flatten();
        let source = Source {
            url: &site,
            icon: icon.as_deref(),
        };
        let s = self.settings.borrow();
        let html = document(
            a,
            &source,
            &self.palette.borrow(),
            s.font_size,
            s.remote_images,
        );
        drop(s);
        self.reader.open.set_sensitive(!a.url.is_empty());
        self.reader.copy.set_sensitive(!a.url.is_empty());
        self.update_buttons(a);
        // Loading signals may change the view, so nothing of it stays borrowed here.
        drop(view);
        self.reader.stack.set_visible_child_name("article");
        self.reader.web.load_html(&html, Some("about:blank"));
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
        let fetching = r.fetching.borrow().contains(&a.id);
        let (full_icon, full_tip) = if a.full {
            ("document-revert-symbolic", "Show the feed's version (D)")
        } else if fetching {
            ("folder-download-symbolic", "Fetching the full article…")
        } else {
            ("folder-download-symbolic", FETCH_FULL)
        };
        r.full.set_icon_name(full_icon);
        r.full.set_tooltip_text(Some(full_tip));
        r.full
            .set_sensitive(!fetching && (!a.url.is_empty() || a.full));
    }

    /// Fetch the open article in full from its web page, or go back to the feed's version if
    /// it is the full article already. The button waits while the article is fetched, even
    /// if another one is opened and this one again meanwhile.
    pub(super) fn toggle_full(self: &Rc<Self>) {
        let Some((id, full, url)) = self
            .view
            .borrow()
            .open()
            .map(|a| (a.id, a.full, a.url.clone()))
        else {
            return;
        };
        if (!full && url.is_empty()) || !self.reader.fetching.borrow_mut().insert(id) {
            return;
        }
        if let Some(a) = self.view.borrow().open() {
            self.update_buttons(a);
        }
        let u = self.clone();
        glib::spawn_future_local(async move {
            let result = if full {
                // Going back needs no server, nor its key.
                let db = u.db.clone();
                u.background(async move {
                    full_article::forget(&db, id).await?;
                    db.call(move |s| s.article(id)).await
                })
                .await
            } else {
                let db = u.db.clone();
                u.background(async move {
                    crate::account::fetch_full_article(&db, id).await?;
                    db.call(move |s| s.article(id)).await
                })
                .await
            };
            u.reader.fetching.borrow_mut().remove(&id);
            let shown = match result {
                Ok(a) => u.view.borrow_mut().reload_content(a),
                Err(e) => {
                    u.error(if full {
                        format!("Could not show the feed's version: {e:#}")
                    } else {
                        format!("Could not fetch the full article: {e:#}")
                    });
                    false
                }
            };
            if shown {
                u.render();
            } else if let Some(a) = u.view.borrow().open() {
                // Failed, or another article is open now: show its buttons as they are.
                u.update_buttons(a);
            }
        });
    }

    /// Mark the article read once its content has loaded (not on re-renders).
    fn mark_opened(self: &Rc<Self>) {
        let loaded = self.view.borrow_mut().take_pending_mark();
        // Shown at once and put back if the write fails, like any other change.
        if let Some(id) = loaded {
            self.set_state(id, omafeed_core::model::StatePatch::read(true));
        }
    }

    pub(super) fn open_original(&self) {
        if let Some(a) = self.view.borrow().open() {
            open_uri(&a.url);
        }
    }

    fn copy_link(&self) {
        if let Some(a) = self.view.borrow().open()
            && !a.url.is_empty()
        {
            self.window.clipboard().set_text(&a.url);
            self.toast("Link copied");
        }
    }
}
