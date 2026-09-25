use adw::prelude::*;
use gtk::{gdk, gio, glib};
use omafeed_core::{
    Db, Paths, Settings,
    db::{Article, Library, Query, Scope},
    fetch::{Progress, Refresher},
    settings::Palette,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};
use webkit6::prelude::*;

pub struct Ui {
    pub window: adw::ApplicationWindow,
    pub db: Db,
    pub paths: Paths,
    pub settings: RefCell<Settings>,
    pub library: RefCell<Library>,
    pub query: RefCell<Query>,
    pub selected: RefCell<Option<Article>>,
    pub sidebar: gtk::ListBox,
    pub list: gtk::ListBox,
    scopes: RefCell<Vec<Scope>>,
    articles: RefCell<Vec<Article>>,
    pub web: webkit6::WebView,
    pub status: gtk::Label,
    pub overlay: adw::ToastOverlay,
    search: gtk::SearchEntry,
    heading: gtk::Label,
    count: gtk::Label,
    star: gtk::Button,
    read: gtk::Button,
    refresh: gtk::Button,
    outer: gtk::Paned,
    inner: gtk::Paned,
    reader_stack: gtk::Stack,
    generation: Cell<u64>,
    selection_generation: Cell<u64>,
    pending_mark: Cell<Option<i64>>,
    rebuilding: Cell<bool>,
    runtime: Arc<tokio::runtime::Runtime>,
    refresher: Refresher,
    progress: async_channel::Sender<Progress>,
    pub palette: RefCell<Palette>,
    css: gtk::CssProvider,
    undo: RefCell<Vec<i64>>,
    more: gtk::Button,
    page: Cell<usize>,
    pub busy: Cell<bool>,
    responsive: Cell<i32>,
}
fn label(text: &str, css: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    if !css.is_empty() {
        l.add_css_class(css);
    }
    l
}
fn icon(name: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(name);
    b.set_tooltip_text(Some(tip));
    b
}
fn scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}
fn padded(b: &gtk::Box, n: i32) {
    b.set_margin_start(n);
    b.set_margin_end(n);
    b.set_margin_top(n);
    b.set_margin_bottom(n);
}
impl Ui {
    pub fn build(
        app: &adw::Application,
        db: Db,
        paths: Paths,
        settings: Settings,
        runtime: Arc<tokio::runtime::Runtime>,
        refresher: Refresher,
    ) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Omafeed")
            .default_width(settings.width)
            .default_height(settings.height)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = adw::HeaderBar::new();
        let brand = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        brand.append(&gtk::Image::from_icon_name("application-rss+xml-symbolic"));
        brand.append(&label("Omafeed", "title-3"));
        header.set_title_widget(Some(&brand));
        let refresh = icon("view-refresh-symbolic", "Refresh feeds (Ctrl+R)");
        header.pack_start(&refresh);
        let sidebar_toggle = icon("sidebar-show-symbolic", "Show or hide subscriptions");
        header.pack_start(&sidebar_toggle);
        let library_button = icon("folder-new-symbolic", "Manage feeds and folders");
        header.pack_start(&library_button);
        let menu = gio::Menu::new();
        for (title, action) in [
            ("Manage library…", "win.library"),
            ("Import OPML…", "win.import"),
            ("Export OPML…", "win.export"),
            ("Settings…", "win.settings"),
            ("Keyboard shortcuts", "win.shortcuts"),
            ("About Omafeed", "win.about"),
        ] {
            menu.append(Some(title), Some(action));
        }
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .build();
        header.pack_end(&menu_button);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search articles…")
            .width_request(240)
            .build();
        header.pack_end(&search);
        root.append(&header);
        let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
        outer.set_position(settings.sidebar_width);
        outer.set_shrink_start_child(false);
        outer.set_resize_start_child(false);
        outer.set_vexpand(true);
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.set_width_request(180);
        sidebar_box.add_css_class("sidebar");
        let sidebar_title = label("LIBRARY", "section-title");
        sidebar_title.set_margin_start(18);
        sidebar_title.set_margin_top(18);
        sidebar_title.set_margin_bottom(12);
        sidebar_box.append(&sidebar_title);
        let sidebar = gtk::ListBox::new();
        sidebar.add_css_class("navigation-sidebar");
        sidebar_box.append(&scroll(&sidebar));
        let manage = gtk::Button::with_label("Manage library");
        manage.set_margin_start(12);
        manage.set_margin_end(12);
        manage.set_margin_top(12);
        manage.set_margin_bottom(12);
        sidebar_box.append(&manage);
        outer.set_start_child(Some(&sidebar_box));
        let inner = gtk::Paned::new(gtk::Orientation::Horizontal);
        inner.set_position(settings.list_width);
        inner.set_shrink_start_child(false);
        inner.set_resize_start_child(false);
        let list_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list_box.set_width_request(270);
        let list_header = gtk::Box::new(gtk::Orientation::Vertical, 8);
        padded(&list_header, 16);
        let heading = label("All Unread", "title-2");
        list_header.append(&heading);
        let options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let count = label("", "dim-label");
        count.set_hexpand(true);
        options.append(&count);
        let unread = gtk::ToggleButton::with_label("Unread");
        unread.set_active(settings.unread_only);
        options.append(&unread);
        let mark = icon("object-select-symbolic", "Mark this view read");
        options.append(&mark);
        list_header.append(&options);
        list_box.append(&list_header);
        let list = gtk::ListBox::new();
        list.add_css_class("articles");
        list.set_activate_on_single_click(true);
        list_box.append(&scroll(&list));
        let more = gtk::Button::with_label("Next page →");
        more.set_visible(false);
        let previous = gtk::Button::with_label("← Previous page");
        let pages = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pages.append(&previous);
        pages.append(&more);
        list_box.append(&pages);
        inner.set_start_child(Some(&list_box));
        let reader = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        padded(&toolbar, 10);
        let back = icon("go-previous-symbolic", "Show article list");
        toolbar.append(&back);
        let read = gtk::Button::with_label("Mark unread");
        let star = gtk::Button::with_label("☆ Star");
        let open = icon("external-link-symbolic", "Open original (O)");
        let copy = icon("edit-copy-symbolic", "Copy article link");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        toolbar.append(&spacer);
        for b in [&read, &star, &copy, &open] {
            toolbar.append(b);
        }
        reader.append(&toolbar);
        let web = webkit6::WebView::new();
        web.set_hexpand(true);
        web.set_vexpand(true);
        if let Some(s) = webkit6::prelude::WebViewExt::settings(&web) {
            s.set_enable_javascript(true);
            s.set_enable_javascript_markup(false);
            s.set_enable_html5_database(false);
            s.set_enable_html5_local_storage(false);
            s.set_media_playback_requires_user_gesture(true);
            s.set_enable_page_cache(false);
        }
        let reader_stack = gtk::Stack::new();
        reader_stack.set_vexpand(true);
        let empty = adw::StatusPage::builder()
            .icon_name("application-rss+xml-symbolic")
            .title("A little space to read")
            .description(
                "Choose an article, or import your subscriptions\nfrom the menu to get started.",
            )
            .build();
        reader_stack.add_named(&empty, Some("empty"));
        reader_stack.add_named(&web, Some("article"));
        reader.append(&reader_stack);
        inner.set_end_child(Some(&reader));
        outer.set_end_child(Some(&inner));
        root.append(&outer);
        let status = label("Ready", "dim-label");
        status.set_margin_start(16);
        status.set_margin_top(6);
        status.set_margin_bottom(6);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        root.append(&status);
        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&root));
        window.set_content(Some(&overlay));
        let (progress, events) = async_channel::unbounded();
        let palette = Palette::load(&settings.theme);
        let query = Query {
            scope: Scope::decode(&settings.scope),
            unread_only: settings.unread_only,
            ..Default::default()
        };
        let ui = Rc::new(Self {
            window,
            db,
            paths,
            settings: RefCell::new(settings),
            library: RefCell::new(Library::default()),
            query: RefCell::new(query),
            selected: RefCell::new(None),
            sidebar,
            list,
            scopes: RefCell::new(vec![]),
            articles: RefCell::new(vec![]),
            web,
            status,
            overlay,
            search,
            heading,
            count,
            star,
            read,
            refresh,
            outer,
            inner,
            reader_stack,
            generation: Cell::new(0),
            selection_generation: Cell::new(0),
            pending_mark: Cell::new(None),
            rebuilding: Cell::new(false),
            runtime,
            refresher,
            progress,
            palette: RefCell::new(palette),
            css: gtk::CssProvider::new(),
            undo: RefCell::new(vec![]),
            more,
            page: Cell::new(0),
            busy: Cell::new(false),
            responsive: Cell::new(-1),
        });
        let u = ui.clone();
        sidebar_toggle.connect_clicked(move |_| {
            let side = u.outer.start_child().unwrap();
            let show = !side.is_visible();
            side.set_visible(show);
            if u.window.width() < 850 {
                u.outer.end_child().unwrap().set_visible(!show);
            }
        });
        ui.apply_theme();
        ui.actions(app);
        for button in [library_button, manage] {
            let u = ui.clone();
            button.connect_clicked(move |_| crate::dialogs::library(u.clone()));
        }
        let u = ui.clone();
        ui.refresh.connect_clicked(move |_| u.start_refresh(true));
        let u = ui.clone();
        ui.search.connect_search_changed(move |s| {
            u.query.borrow_mut().search = s.text().to_string();
            u.page.set(0);
            u.reload();
        });
        let u = ui.clone();
        unread.connect_toggled(move |b| {
            u.query.borrow_mut().unread_only = b.is_active();
            u.settings.borrow_mut().unread_only = b.is_active();
            u.page.set(0);
            u.reload();
            u.save();
        });
        let u = ui.clone();
        ui.sidebar.connect_row_selected(move |_, row| {
            if u.rebuilding.get() {
                return;
            }
            if let Some(row) = row
                && let Some(scope) = u.scopes.borrow().get(row.index() as usize).cloned()
            {
                u.query.borrow_mut().scope = scope.clone();
                u.settings.borrow_mut().scope = scope.encode();
                u.page.set(0);
                u.reload_articles();
                u.save();
            }
        });
        let u = ui.clone();
        ui.list.connect_row_selected(move |_, row| {
            if u.rebuilding.get() {
                return;
            }
            if let Some(row) = row
                && let Some(a) = u.articles.borrow().get(row.index() as usize)
            {
                u.select(a.id);
            }
        });
        let u = ui.clone();
        ui.more.connect_clicked(move |_| {
            u.page.set(u.page.get() + 1);
            u.reload_articles();
        });
        let u = ui.clone();
        previous.connect_clicked(move |_| {
            if u.page.get() > 0 {
                u.page.set(u.page.get() - 1);
                u.reload_articles();
            }
        });
        let u = ui.clone();
        mark.connect_clicked(move |_| u.mark_all());
        let u = ui.clone();
        ui.star.connect_clicked(move |_| u.toggle_star());
        let u = ui.clone();
        ui.read.connect_clicked(move |_| u.toggle_read());
        let u = ui.clone();
        open.connect_clicked(move |_| u.open_original());
        let u = ui.clone();
        copy.connect_clicked(move |_| {
            if let Some(a) = u.selected.borrow().as_ref() {
                u.window.clipboard().set_text(&a.url);
                u.toast("Link copied");
            }
        });
        let u = ui.clone();
        back.connect_clicked(move |_| {
            u.inner.start_child().unwrap().set_visible(true);
            u.inner
                .end_child()
                .unwrap()
                .set_visible(u.window.width() >= 850);
        });
        let u = ui.clone();
        ui.web.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                u.mark_opened();
            }
        });
        ui.web.connect_decide_policy(|_, decision, kind| {
            if matches!(
                kind,
                webkit6::PolicyDecisionType::NavigationAction
                    | webkit6::PolicyDecisionType::NewWindowAction
            ) && let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>()
                && let Some(mut action) = nav.navigation_action()
            {
                if action.navigation_type() == webkit6::NavigationType::LinkClicked {
                    if let Some(uri) = action.request().and_then(|r| r.uri()) {
                        open_uri(uri.as_str());
                    }
                    decision.ignore();
                    return true;
                }
                if kind == webkit6::PolicyDecisionType::NewWindowAction
                    || action
                        .request()
                        .and_then(|r| r.uri())
                        .is_some_and(|u| u.as_str() != "about:blank")
                {
                    decision.ignore();
                    return true;
                }
            }
            false
        });
        let weak = Rc::downgrade(&ui);
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(u) = weak.upgrade() else {
                    break;
                };
                match event {
                    Progress::Started(n) => {
                        u.busy.set(true);
                        u.refresh.set_sensitive(false);
                        if n > 0 {
                            u.status.set_text(&format!("Refreshing {n} feeds…"));
                        }
                    }
                    Progress::Feed {
                        title,
                        error,
                        done,
                        total,
                    } => u.status.set_text(&format!(
                        "{done}/{total} · {title}{}",
                        if error.is_some() {
                            " · unavailable"
                        } else {
                            ""
                        }
                    )),
                    Progress::Finished { total, failed } => {
                        u.busy.set(false);
                        u.refresh.set_sensitive(true);
                        if total > 0 {
                            u.status.set_text(&format!(
                                "Refreshed {total} feeds · {failed} unavailable"
                            ));
                            u.reload();
                        }
                    }
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_seconds_local(30, move || {
            let Some(u) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !u.window.is_visible() {
                return glib::ControlFlow::Break;
            }
            u.start_refresh(false);
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_seconds_local(2, move || {
            let Some(u) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let p = Palette::load(&u.settings.borrow().theme);
            if p != *u.palette.borrow() {
                *u.palette.borrow_mut() = p;
                u.apply_theme();
                u.render();
            }
            let mode = if u.window.width() < 850 {
                0
            } else if u.window.width() < 1000 {
                1
            } else {
                2
            };
            if u.responsive.replace(mode) != mode {
                u.outer.start_child().unwrap().set_visible(mode == 2);
                u.outer.end_child().unwrap().set_visible(true);
                u.inner
                    .start_child()
                    .unwrap()
                    .set_visible(mode > 0 || u.selected.borrow().is_none());
                u.inner
                    .end_child()
                    .unwrap()
                    .set_visible(mode > 0 || u.selected.borrow().is_some());
            }
            glib::ControlFlow::Continue
        });
        let u = ui.clone();
        ui.window.connect_close_request(move |_| {
            let mut s = u.settings.borrow_mut();
            s.width = u.window.width();
            s.height = u.window.height();
            if u.outer.start_child().is_some_and(|w| w.is_visible()) {
                s.sidebar_width = u.outer.position();
            }
            if u.inner.start_child().is_some_and(|w| w.is_visible()) {
                s.list_width = u.inner.position();
            }
            drop(s);
            u.save();
            glib::Propagation::Proceed
        });
        ui.window.present();
        ui.reload();
        ui.start_refresh(false);
        ui
    }
    pub fn toast(&self, text: &str) {
        self.overlay.add_toast(adw::Toast::new(text));
    }
    pub fn error(&self, e: impl std::fmt::Display) {
        let text = e.to_string();
        self.status.set_text(&text);
        self.toast(&text);
        eprintln!("{text}");
    }
    pub fn save(&self) {
        if let Err(e) = self.settings.borrow().save(&self.paths) {
            self.error(e);
        }
    }
    pub fn apply_theme(&self) {
        let p = self.palette.borrow();
        adw::StyleManager::default().set_color_scheme(if p.dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        });
        let css = include_str!("../../../data/style.css")
            .replace("{{background}}", &p.background)
            .replace("{{foreground}}", &p.foreground)
            .replace("{{accent}}", &p.accent);
        self.css.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &self.css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    pub fn reload(self: &Rc<Self>) {
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(|s| s.library()).await {
                Ok(lib) => {
                    *u.library.borrow_mut() = lib;
                    u.rebuild_sidebar();
                    u.reload_articles();
                }
                Err(e) => u.error(e),
            }
        });
    }
    fn rebuild_sidebar(&self) {
        self.rebuilding.set(true);
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let lib = self.library.borrow();
        let mut items = vec![
            (Scope::Unread, "All Unread".into(), lib.unread, 0, false),
            (Scope::Today, "Today".into(), 0, 0, false),
            (Scope::Starred, "Starred".into(), lib.starred, 0, false),
            (Scope::All, "All Articles".into(), 0, 0, false),
        ];
        fn tree(
            lib: &Library,
            parent: Option<i64>,
            depth: i32,
            items: &mut Vec<(Scope, String, i64, i32, bool)>,
        ) {
            for f in lib.folders.iter().filter(|f| f.parent == parent) {
                let before = items.len();
                items.push((
                    Scope::Folder(f.id),
                    format!("▾ {}", f.name),
                    0,
                    depth,
                    false,
                ));
                tree(lib, Some(f.id), depth + 1, items);
                let count = items[before + 1..]
                    .iter()
                    .filter(|(s, _, _, _, _)| matches!(s, Scope::Feed(_)))
                    .map(|(_, _, n, _, _)| *n)
                    .sum();
                items[before].2 = count;
            }
            for f in lib.feeds.iter().filter(|f| f.folder == parent) {
                items.push((
                    Scope::Feed(f.id),
                    f.title.clone(),
                    f.unread,
                    depth,
                    f.error.is_some(),
                ));
            }
        }
        tree(&lib, None, 0, &mut items);
        let selected = self.query.borrow().scope.clone();
        let mut scopes = vec![];
        for (scope, title, count, depth, error) in items {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_start(depth * 12);
            let text = label(&title, "");
            text.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.set_hexpand(true);
            if let Scope::Feed(id) = scope
                && let Some(feed) = lib.feeds.iter().find(|f| f.id == id)
            {
                let path = omafeed_core::icons::path(
                    &self.paths.cache,
                    if feed.site_url.is_empty() {
                        &feed.url
                    } else {
                        &feed.site_url
                    },
                );
                let image = if path.exists() {
                    gtk::Image::from_file(path)
                } else {
                    gtk::Image::from_icon_name("application-rss+xml-symbolic")
                };
                image.set_pixel_size(16);
                row.append(&image);
            }
            row.append(&text);
            if error {
                row.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            }
            if count > 0 {
                row.append(&label(&count.to_string(), "dim-label"));
            }
            let r = gtk::ListBoxRow::new();
            r.set_child(Some(&row));
            if let Scope::Feed(id) = scope
                && let Some(f) = lib.feeds.iter().find(|f| f.id == id)
            {
                r.set_tooltip_text(Some(
                    &f.error
                        .as_ref()
                        .map(|e| format!("{}\n{e}", f.url))
                        .unwrap_or_else(|| f.url.clone()),
                ));
            }
            self.sidebar.append(&r);
            if scope == selected {
                self.sidebar.select_row(Some(&r));
            }
            scopes.push(scope);
        }
        *self.scopes.borrow_mut() = scopes;
        self.rebuilding.set(false);
    }
    pub fn reload_articles(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let mut q = self.query.borrow().clone();
        q.offset = self.page.get() * 200;
        let page = self.page.get();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.articles(&q)).await {
                Ok(articles) => {
                    if u.generation.get() != generation {
                        return;
                    }
                    u.rebuilding.set(true);
                    while let Some(child) = u.list.first_child() {
                        u.list.remove(&child);
                    }
                    u.more.set_visible(articles.len() == 200);
                    let selected = u
                        .selected
                        .borrow()
                        .as_ref()
                        .map(|a| a.id)
                        .or(u.settings.borrow().selected_article);
                    for a in &articles {
                        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
                        let title = label(
                            &format!(
                                "{}{}{}",
                                if a.read { "" } else { "● " },
                                if a.starred { "★ " } else { "" },
                                a.title
                            ),
                            "article-title",
                        );
                        if a.read {
                            title.add_css_class("article-read");
                        }
                        title.set_wrap(true);
                        title.set_lines(3);
                        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        box_.append(&title);
                        let date = chrono::DateTime::from_timestamp(a.published, 0)
                            .map(|d| d.format("%b %-d").to_string())
                            .unwrap_or_default();
                        let meta = label(&format!("{} · {date}", a.feed_title), "dim-label");
                        meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        box_.append(&meta);
                        let preview = label(&a.preview.replace('\n', " "), "preview");
                        preview.set_wrap(true);
                        preview.set_lines(2);
                        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        box_.append(&preview);
                        let row = gtk::ListBoxRow::new();
                        row.set_child(Some(&box_));
                        u.list.append(&row);
                        if Some(a.id) == selected {
                            u.list.select_row(Some(&row));
                        }
                    }
                    u.count.set_text(&if articles.is_empty() {
                        "No articles".into()
                    } else {
                        format!("{}–{}", page * 200 + 1, page * 200 + articles.len())
                    });
                    let title = match u.query.borrow().scope {
                        Scope::Unread => "All Unread".into(),
                        Scope::Today => "Today".into(),
                        Scope::Starred => "Starred".into(),
                        Scope::All => "All Articles".into(),
                        Scope::Feed(id) => u
                            .library
                            .borrow()
                            .feeds
                            .iter()
                            .find(|f| f.id == id)
                            .map(|f| f.title.clone())
                            .unwrap_or_else(|| "Feed".into()),
                        Scope::Folder(id) => u
                            .library
                            .borrow()
                            .folders
                            .iter()
                            .find(|f| f.id == id)
                            .map(|f| f.name.clone())
                            .unwrap_or_else(|| "Folder".into()),
                    };
                    u.heading.set_text(&title);
                    u.heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    *u.articles.borrow_mut() = articles;
                    u.rebuilding.set(false);
                    if u.selected.borrow().is_none()
                        && let Some(id) = selected
                        && u.articles.borrow().iter().any(|a| a.id == id)
                    {
                        u.select(id);
                    }
                }
                Err(e) => u.error(e),
            }
        });
    }
    pub fn select(self: &Rc<Self>, id: i64) {
        let generation = self.selection_generation.get() + 1;
        self.selection_generation.set(generation);
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.article(id)).await {
                Ok(a) => {
                    if u.selection_generation.get() != generation {
                        return;
                    }
                    u.settings.borrow_mut().selected_article = Some(id);
                    *u.selected.borrow_mut() = Some(a);
                    u.pending_mark.set(Some(id));
                    u.render();
                    u.save();
                    if u.window.width() < 850 {
                        u.inner.start_child().unwrap().set_visible(false);
                        u.inner.end_child().unwrap().set_visible(true);
                    }
                }
                Err(e) => u.error(e),
            }
        });
    }
    pub fn render(&self) {
        if let Some(a) = self.selected.borrow().as_ref() {
            let s = self.settings.borrow();
            let html = omafeed_core::article::document(
                a,
                &self.palette.borrow(),
                s.font_size,
                s.remote_images,
            );
            self.reader_stack.set_visible_child_name("article");
            self.web.load_html(&html, Some("about:blank"));
            self.update_buttons(a);
        } else {
            self.reader_stack.set_visible_child_name("empty");
        }
    }
    fn update_buttons(&self, a: &Article) {
        self.star
            .set_label(if a.starred { "★ Starred" } else { "☆ Star" });
        self.read
            .set_label(if a.read { "Mark unread" } else { "Mark read" });
    }
    fn mark_opened(self: &Rc<Self>) {
        let Some(loaded) = self.pending_mark.take() else {
            return;
        };
        if self.selected.borrow().as_ref().map(|a| a.id) != Some(loaded) {
            return;
        }
        let id = {
            let mut selected = self.selected.borrow_mut();
            if let Some(a) = selected.as_mut() {
                if a.read {
                    return;
                }
                a.read = true;
                self.update_buttons(a);
                Some(a.id)
            } else {
                None
            }
        };
        if let Some(id) = id {
            let u = self.clone();
            glib::spawn_future_local(async move {
                if let Err(e) = u.db.call(move |s| s.set_read(id, true)).await {
                    u.error(e);
                } else {
                    u.update_local_state(id, Some(true), None);
                    u.refresh_counts();
                }
            });
        }
    }
    fn update_local_state(&self, id: i64, read: Option<bool>, star: Option<bool>) {
        let mut articles = self.articles.borrow_mut();
        if let Some((index, a)) = articles.iter_mut().enumerate().find(|(_, a)| a.id == id) {
            if let Some(r) = read {
                a.read = r;
            }
            if let Some(s) = star {
                a.starred = s;
            }
            if let Some(row) = self.list.row_at_index(index as i32)
                && let Some(b) = row.child().and_downcast::<gtk::Box>()
                && let Some(l) = b.first_child().and_downcast::<gtk::Label>()
            {
                l.set_text(&format!(
                    "{}{}{}",
                    if a.read { "" } else { "● " },
                    if a.starred { "★ " } else { "" },
                    a.title
                ));
                if a.read {
                    l.add_css_class("article-read");
                } else {
                    l.remove_css_class("article-read");
                }
            }
        }
    }
    fn refresh_counts(self: &Rc<Self>) {
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(|s| s.library()).await {
                Ok(lib) => {
                    *u.library.borrow_mut() = lib;
                    u.rebuild_sidebar();
                }
                Err(e) => u.error(e),
            }
        });
    }
    pub fn toggle_read(self: &Rc<Self>) {
        let change = self.selected.borrow().as_ref().map(|a| (a.id, !a.read));
        if let Some((id, value)) = change {
            let u = self.clone();
            glib::spawn_future_local(async move {
                match u.db.call(move |s| s.set_read(id, value)).await {
                    Ok(()) => {
                        if let Some(a) = u.selected.borrow_mut().as_mut().filter(|a| a.id == id) {
                            a.read = value;
                            u.update_buttons(a);
                        }
                        u.update_local_state(id, Some(value), None);
                        u.refresh_counts();
                    }
                    Err(e) => u.error(e),
                }
            });
        }
    }
    pub fn toggle_star(self: &Rc<Self>) {
        let change = self.selected.borrow().as_ref().map(|a| (a.id, !a.starred));
        if let Some((id, value)) = change {
            let u = self.clone();
            glib::spawn_future_local(async move {
                match u.db.call(move |s| s.set_starred(id, value)).await {
                    Ok(()) => {
                        if let Some(a) = u.selected.borrow_mut().as_mut().filter(|a| a.id == id) {
                            a.starred = value;
                            u.update_buttons(a);
                        }
                        u.update_local_state(id, None, Some(value));
                        u.refresh_counts();
                    }
                    Err(e) => u.error(e),
                }
            });
        }
    }
    fn mark_all(self: &Rc<Self>) {
        let q = self.query.borrow().clone();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.mark_read(&q)).await {
                Ok(ids) => {
                    u.toast(&format!(
                        "Marked {} articles read · Ctrl+Z to undo",
                        ids.len()
                    ));
                    *u.undo.borrow_mut() = ids;
                    if let Some(a) = u.selected.borrow_mut().as_mut()
                        && u.undo.borrow().contains(&a.id)
                    {
                        a.read = true;
                        u.update_buttons(a);
                    }
                    u.reload();
                }
                Err(e) => u.error(e),
            }
        });
    }
    fn undo(self: &Rc<Self>) {
        let ids = std::mem::take(&mut *self.undo.borrow_mut());
        if ids.is_empty() {
            return;
        }
        let selected = self.selected.borrow().as_ref().map(|a| a.id);
        let restore = selected.is_some_and(|id| ids.contains(&id));
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.undo_read(&ids)).await {
                Ok(()) => {
                    if restore && let Some(a) = u.selected.borrow_mut().as_mut() {
                        a.read = false;
                        u.update_buttons(a);
                    }
                    u.reload();
                }
                Err(e) => u.error(e),
            }
        });
    }
    pub fn start_refresh(self: &Rc<Self>, force: bool) {
        if self.busy.get() {
            return;
        }
        let db = self.db.clone();
        let refresh = self.refresher.clone();
        let minutes = self.settings.borrow().refresh_minutes;
        let tx = self.progress.clone();
        let (err_tx, err_rx) = async_channel::bounded(1);
        self.runtime.spawn(async move {
            if let Err(e) = refresh.refresh(db, force, minutes, tx).await {
                let _ = err_tx.send(e.to_string()).await;
            }
        });
        let u = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(e) = err_rx.recv().await {
                u.busy.set(false);
                u.refresh.set_sensitive(true);
                u.error(e);
            }
        });
    }
    pub fn open_original(&self) {
        if let Some(a) = self.selected.borrow().as_ref() {
            open_uri(&a.url);
        }
    }
    fn navigate(self: &Rc<Self>, delta: i32, unread: bool) {
        let current = self
            .list
            .selected_row()
            .map(|r| r.index())
            .unwrap_or(if delta > 0 { -1 } else { 0 });
        let articles = self.articles.borrow();
        let mut i = current + delta;
        while i >= 0 && (i as usize) < articles.len() {
            if !unread || !articles[i as usize].read {
                if let Some(row) = self.list.row_at_index(i) {
                    self.list.select_row(Some(&row));
                    row.grab_focus();
                }
                return;
            }
            i += delta;
        }
        if unread {
            self.toast("No more unread articles on this page");
        }
    }
    fn action(self: &Rc<Self>, name: &str, callback: impl Fn(Rc<Self>) + 'static) {
        let a = gio::SimpleAction::new(name, None);
        let u = self.clone();
        a.connect_activate(move |_, _| callback(u.clone()));
        self.window.add_action(&a);
    }
    fn actions(self: &Rc<Self>, app: &adw::Application) {
        self.action("library", crate::dialogs::library);
        self.action("import", crate::dialogs::import);
        self.action("export", crate::dialogs::export);
        self.action("settings", crate::dialogs::settings);
        self.action("shortcuts", crate::dialogs::shortcuts);
        self.action("about", crate::dialogs::about);
        self.action("refresh", |u| u.start_refresh(true));
        self.action("search", |u| {
            u.search.grab_focus();
        });
        self.action("undo", |u| u.undo());
        self.action("quit", |u| u.window.close());
        self.action("mark", |u| u.mark_all());
        for (a, keys) in [
            ("refresh", vec!["<Control>r"]),
            ("search", vec!["<Control>f"]),
            ("undo", vec!["<Control>z"]),
            ("quit", vec!["<Control>q"]),
            ("library", vec!["<Control>l"]),
            ("mark", vec!["<Control><Shift>a"]),
        ] {
            app.set_accels_for_action(&format!("win.{a}"), &keys);
        }
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let u = self.clone();
        key.connect_key_pressed(move|_,key,_,mods|{if mods.intersects(gdk::ModifierType::CONTROL_MASK|gdk::ModifierType::ALT_MASK|gdk::ModifierType::SUPER_MASK){return glib::Propagation::Proceed;}let focus=gtk::prelude::GtkWindowExt::focus(&u.window);if focus.as_ref().is_some_and(|w|w.is::<gtk::Text>()||w.is::<gtk::Entry>()||w.is::<gtk::SearchEntry>()){return glib::Propagation::Proceed;}match key {gdk::Key::j|gdk::Key::Down=>u.navigate(1,false),gdk::Key::k|gdk::Key::Up=>u.navigate(-1,false),gdk::Key::n=>u.navigate(1,true),gdk::Key::m=>u.toggle_read(),gdk::Key::s=>u.toggle_star(),gdk::Key::o=>u.open_original(),gdk::Key::space=>{let backwards=mods.contains(gdk::ModifierType::SHIFT_MASK);let script=if backwards{"window.scrollBy(0,-window.innerHeight*0.85); false"}else{"(() => {if(window.scrollY+window.innerHeight>=document.documentElement.scrollHeight-4)return true;window.scrollBy(0,window.innerHeight*0.85);return false;})()"};let next=u.clone();u.web.evaluate_javascript(script,Some("omafeed"),None,None::<&gio::Cancellable>,move|r|{if r.is_ok_and(|v|v.to_boolean()){next.navigate(1,true);}});},gdk::Key::question=>crate::dialogs::shortcuts(u.clone()),_=>return glib::Propagation::Proceed}glib::Propagation::Stop});
        self.window.add_controller(key);
    }
}
pub fn open_uri(uri: &str) {
    if (uri.starts_with("https://") || uri.starts_with("http://") || uri.starts_with("mailto:"))
        && let Err(e) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>)
    {
        eprintln!("Open link: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[track_caller]
    fn pump_until(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let context = glib::MainContext::default();
        while !condition() {
            assert!(Instant::now() < deadline, "GTK operation timed out");
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        while context.pending() {
            context.iteration(false);
        }
    }
    fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut all = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(c) = child {
            all.extend(widgets(&c));
            child = c.next_sibling();
        }
        all
    }
    fn button(root: &gtk::Widget, text: &str) -> gtk::Button {
        widgets(root)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some(text))
            .unwrap_or_else(|| panic!("Missing button {text}"))
    }
    fn all_widgets() -> Vec<gtk::Widget> {
        gtk::Window::list_toplevels()
            .iter()
            .flat_map(widgets)
            .collect()
    }
    fn form_button(text: &str) -> gtk::Button {
        all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some(text))
            .unwrap_or_else(|| panic!("Missing form button {text}"))
    }
    #[test]
    #[ignore = "requires a desktop session; uses an isolated temporary library"]
    fn desktop_smoke() {
        adw::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: dir.path().join("data"),
            config: dir.path().join("config"),
            cache: dir.path().join("cache"),
        };
        for p in [&paths.data, &paths.config, &paths.cache] {
            std::fs::create_dir_all(p).unwrap();
        }
        let db = Db::open(paths.data.join("test.db")).unwrap();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let id = runtime
            .block_on(db.call(|s| {
                let folder = s.add_folder("Blogs", None)?;
                let f = s.add_feed("Fixture feed", "https://example.invalid/feed", Some(folder))?;
                s.commit_download(
                    f,
                    omafeed_core::fetch::Download {
                        entries: vec![omafeed_core::fetch::Entry {
                            identity: "one".into(),
                            title: "Fixture article".into(),
                            url: "https://example.invalid/post".into(),
                            author: "Author".into(),
                            published: Some(1_700_000_000),
                            html: "<p>Readable fixture content.</p>".into(),
                            text: "Readable fixture content.".into(),
                        }],
                        etag: None,
                        modified: None,
                        not_modified: false,
                        site_url: Some("https://example.invalid/".into()),
                    },
                    1440,
                )?;
                Ok(s.articles(&Query {
                    scope: Scope::All,
                    ..Default::default()
                })?[0]
                    .id)
            }))
            .unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.pch.Omafeed.Smoke")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let u = Ui::build(
            &app,
            db.clone(),
            paths,
            Settings {
                remote_images: false,
                ..Default::default()
            },
            runtime.clone(),
            Refresher::new().unwrap(),
        );
        pump_until(|| !u.articles.borrow().is_empty());
        u.select(id);
        pump_until(|| u.selected.borrow().as_ref().is_some_and(|a| a.read));
        u.toggle_star();
        pump_until(|| u.selected.borrow().as_ref().is_some_and(|a| a.starred));
        u.toggle_read();
        pump_until(|| u.selected.borrow().as_ref().is_some_and(|a| !a.read));
        u.render();
        pump_until(|| !u.web.is_loading());
        let done = Rc::new(RefCell::new(None));
        let out = done.clone();
        u.web.evaluate_javascript(
            "document.body.textContent.includes('Readable fixture content')",
            Some("omafeed"),
            None,
            None::<&gio::Cancellable>,
            move |r| {
                *out.borrow_mut() = Some(r.map(|v| v.to_boolean()));
            },
        );
        pump_until(|| done.borrow().is_some());
        assert!(done.borrow_mut().take().unwrap().unwrap());
        assert!(
            !runtime
                .block_on(db.call(move |s| s.article(id)))
                .unwrap()
                .read,
            "Theme/reader reload must not mark an intentionally unread article read"
        );
        crate::dialogs::library(u.clone());
        let window = Rc::new(RefCell::new(None::<gtk::Window>));
        pump_until(|| {
            *window.borrow_mut() = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.title().as_deref() == Some("Manage library"));
            window.borrow().is_some()
        });
        let library = window.borrow().clone().unwrap();
        let root = library.clone().upcast::<gtk::Widget>();
        button(&root, "New folder").emit_clicked();
        pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
        let name = all_widgets()
            .into_iter()
            .find_map(|w| w.downcast::<gtk::Entry>().ok())
            .unwrap();
        name.set_text("Created through UI");
        form_button("Save").emit_clicked();
        pump_until(|| {
            runtime
                .block_on(db.call(|s| {
                    Ok(s.library()?
                        .folders
                        .iter()
                        .any(|f| f.name == "Created through UI"))
                }))
                .unwrap()
        });
        pump_until(|| !all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
        let list = widgets(&root)
            .into_iter()
            .find_map(|w| w.downcast::<gtk::ListBox>().ok())
            .unwrap();
        let feed_row = widgets(&list.clone().upcast())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::ListBoxRow>().ok())
            .find(|r| {
                widgets(&r.clone().upcast())
                    .into_iter()
                    .filter_map(|w| w.downcast::<gtk::Label>().ok())
                    .any(|l| l.text().contains("Fixture feed"))
            })
            .unwrap();
        list.select_row(Some(&feed_row));
        button(&root, "Edit / Move").emit_clicked();
        pump_until(|| all_widgets().iter().any(|w| w.is::<gtk::Entry>()));
        let fields = all_widgets()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Entry>().ok())
            .collect::<Vec<_>>();
        fields[0].set_text("Renamed through UI");
        let picker = all_widgets()
            .into_iter()
            .find_map(|w| w.downcast::<gtk::DropDown>().ok())
            .unwrap();
        picker.set_selected(2);
        form_button("Save").emit_clicked();
        pump_until(|| {
            runtime
                .block_on(db.call(|s| {
                    let lib = s.library()?;
                    let f = &lib.feeds[0];
                    Ok(f.title == "Renamed through UI"
                        && lib.folders.iter().any(|folder| {
                            Some(folder.id) == f.folder && folder.name == "Created through UI"
                        }))
                }))
                .unwrap()
        });
        library.close();
        pump_until(|| !library.is_visible());
        // Persisted state and actual WebKit rendering were verified above.
        u.window.close();
        pump_until(|| !u.window.is_visible());
    }
}
