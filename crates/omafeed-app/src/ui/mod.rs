//! Main window: a library sidebar, an article list, and a WebKit reader.
mod articles;
mod keys;
mod reader;
mod sidebar;
#[cfg(test)]
mod tests;
mod theme;
pub mod widgets;

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
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use widgets::{icon, label, margins, scroll};

/// Widths (px) at which panes collapse; see [`Layout`].
const NARROW_MAX_WIDTH: f64 = 849.0;
const MEDIUM_MAX_WIDTH: f64 = 999.0;
/// How often to look for feeds whose refresh interval has elapsed.
const REFRESH_CHECK_SECONDS: u32 = 30;
/// How often to check whether another program (such as the command line) changed the library.
const OUTSIDE_CHANGE_SECONDS: u32 = 2;
/// Delay before writing settings, so rapid changes (e.g. holding J) coalesce.
const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// One pane at a time: the list, or the reader with a back button.
    Narrow,
    /// List and reader; the sidebar is hidden until toggled.
    Medium,
    /// All three panes.
    Wide,
}

struct Header {
    bar: adw::HeaderBar,
    brand: gtk::Box,
    refresh: gtk::Button,
    sidebar_toggle: gtk::Button,
    library: gtk::Button,
    search: gtk::SearchEntry,
}

impl Header {
    fn new() -> Self {
        let bar = adw::HeaderBar::new();
        let brand = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        brand.append(&gtk::Image::from_icon_name("application-rss+xml-symbolic"));
        brand.append(&label("Omafeed", "title-3"));
        bar.set_title_widget(Some(&brand));
        let refresh = icon("view-refresh-symbolic", "Refresh feeds (Ctrl+R)");
        let sidebar_toggle = icon("sidebar-show-symbolic", "Show or hide subscriptions");
        let library = icon("folder-new-symbolic", "Manage feeds and folders");
        for b in [&refresh, &sidebar_toggle, &library] {
            bar.pack_start(b);
        }
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
        bar.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .build(),
        );
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search articles…")
            .width_request(240)
            .build();
        bar.pack_end(&search);
        Self {
            bar,
            brand,
            refresh,
            sidebar_toggle,
            library,
            search,
        }
    }
}

struct SidebarPane {
    root: gtk::Box,
    list: gtk::ListBox,
    manage: gtk::Button,
}

impl SidebarPane {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_width_request(180);
        root.add_css_class("sidebar");
        let title = label("LIBRARY", "section-title");
        title.set_margin_start(18);
        title.set_margin_top(18);
        title.set_margin_bottom(12);
        root.append(&title);
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        root.append(&scroll(&list));
        let manage = gtk::Button::with_label("Manage library");
        margins(&manage, 12);
        root.append(&manage);
        Self { root, list, manage }
    }
}

struct ListPane {
    root: gtk::Box,
    heading: gtk::Label,
    count: gtk::Label,
    unread: gtk::ToggleButton,
    mark: gtk::Button,
    list: gtk::ListBox,
    previous: gtk::Button,
    next: gtk::Button,
}

impl ListPane {
    fn new(unread_only: bool) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_width_request(270);
        let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
        margins(&header, 16);
        let heading = label("All Unread", "list-heading");
        heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&heading);
        let options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let count = label("", "dim-label");
        count.set_hexpand(true);
        options.append(&count);
        let unread = gtk::ToggleButton::with_label("Unread");
        unread.set_active(unread_only);
        options.append(&unread);
        let mark = icon("object-select-symbolic", "Mark this view read");
        options.append(&mark);
        header.append(&options);
        root.append(&header);
        let list = gtk::ListBox::new();
        list.add_css_class("articles");
        list.set_activate_on_single_click(true);
        root.append(&scroll(&list));
        let previous = gtk::Button::with_label("Previous");
        previous.set_tooltip_text(Some("Previous page"));
        let next = gtk::Button::with_label("Next");
        next.set_tooltip_text(Some("Next page"));
        let pages = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&pages, 12);
        pages.set_homogeneous(true);
        pages.add_css_class("pagination");
        for b in [&previous, &next] {
            b.set_sensitive(false);
            b.add_css_class("flat");
            pages.append(b);
        }
        root.append(&pages);
        Self {
            root,
            heading,
            count,
            unread,
            mark,
            list,
            previous,
            next,
        }
    }
}

fn paned(position: i32, start: &impl IsA<gtk::Widget>, end: &impl IsA<gtk::Widget>) -> gtk::Paned {
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_position(position);
    paned.set_shrink_start_child(false);
    paned.set_resize_start_child(false);
    paned.set_start_child(Some(start));
    paned.set_end_child(Some(end));
    paned
}

fn breakpoint(max_width: f64) -> adw::Breakpoint {
    adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        max_width,
        adw::LengthUnit::Px,
    ))
}

pub struct Ui {
    pub(crate) window: adw::ApplicationWindow,
    pub(crate) db: Db,
    pub(crate) paths: Paths,
    runtime: Arc<tokio::runtime::Runtime>,
    refresher: Refresher,
    progress: async_channel::Sender<Progress>,

    header: Header,
    side: SidebarPane,
    list: ListPane,
    reader: reader::ReaderPane,
    inner: gtk::Paned,
    outer: gtk::Paned,
    status: gtk::Label,
    overlay: adw::ToastOverlay,
    narrow: adw::Breakpoint,
    css: gtk::CssProvider,

    pub(crate) settings: RefCell<Settings>,
    pub(crate) library: RefCell<Library>,
    query: RefCell<Query>,
    selected: RefCell<Option<Article>>,
    palette: RefCell<Palette>,
    articles: RefCell<Vec<Article>>,
    article_rows: RefCell<Vec<articles::ArticleRow>>,
    sidebar_rows: RefCell<Vec<sidebar::Row>>,
    collapsed: RefCell<HashSet<i64>>,
    /// Decoded favicons by cache path; `None` records a missing icon.
    favicons: RefCell<HashMap<PathBuf, Option<gdk::Texture>>>,
    undo: RefCell<Vec<i64>>,
    page: Cell<usize>,
    /// Incremented per request so stale async results are discarded.
    generation: Cell<u64>,
    selection_generation: Cell<u64>,
    /// Article to mark read once WebKit finishes loading it.
    pending_mark: Cell<Option<i64>>,
    /// Suppresses selection signals while lists are repopulated.
    rebuilding: Cell<bool>,
    busy: Cell<bool>,
    /// The database change counter last seen, to notice writes by other programs.
    data_version: Cell<Option<i64>>,
    save_pending: Cell<bool>,
    theme_pending: Cell<bool>,
    theme_monitors: RefCell<Vec<gio::FileMonitor>>,
}

impl Ui {
    /// Build and show the main window. Signal handlers hold only weak references,
    /// so the caller must keep the returned `Rc` alive for the window's lifetime.
    pub fn build(
        app: &adw::Application,
        db: Db,
        paths: Paths,
        settings: Settings,
        runtime: Arc<tokio::runtime::Runtime>,
        refresher: Refresher,
    ) -> Rc<Self> {
        let header = Header::new();
        let side = SidebarPane::new();
        let list = ListPane::new(settings.unread_only);
        let reader = reader::ReaderPane::new();
        let inner = paned(settings.list_width, &list.root, &reader.root);
        let outer = paned(settings.sidebar_width, &side.root, &inner);
        outer.set_vexpand(true);
        let status = label("Ready", "status-text");
        status.set_margin_start(16);
        status.set_margin_top(6);
        status.set_margin_bottom(6);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&header.bar);
        root.append(&outer);
        root.append(&status);
        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&root));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Omafeed")
            .default_width(settings.width)
            .default_height(settings.height)
            .width_request(360)
            .height_request(400)
            .content(&overlay)
            .build();
        let medium = breakpoint(MEDIUM_MAX_WIDTH);
        let narrow = breakpoint(NARROW_MAX_WIDTH);
        narrow.add_setter(&header.brand, "visible", Some(&false.to_value()));
        narrow.add_setter(&header.search, "width-request", Some(&120.to_value()));
        // When several breakpoints match, the last one added wins.
        window.add_breakpoint(medium);
        window.add_breakpoint(narrow.clone());
        let css = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let (progress, events) = async_channel::unbounded();
        let palette = Palette::load(settings.theme);
        let query = Query {
            scope: settings.scope.clone(),
            unread_only: settings.unread_only,
            ..Default::default()
        };
        let ui = Rc::new(Self {
            window,
            db,
            paths,
            runtime,
            refresher,
            progress,
            header,
            side,
            list,
            reader,
            inner,
            outer,
            status,
            overlay,
            narrow,
            css,
            settings: RefCell::new(settings),
            library: RefCell::default(),
            query: RefCell::new(query),
            selected: RefCell::default(),
            palette: RefCell::new(palette),
            articles: RefCell::default(),
            article_rows: RefCell::default(),
            sidebar_rows: RefCell::default(),
            collapsed: RefCell::default(),
            favicons: RefCell::default(),
            undo: RefCell::default(),
            page: Cell::new(0),
            generation: Cell::new(0),
            selection_generation: Cell::new(0),
            pending_mark: Cell::new(None),
            rebuilding: Cell::new(false),
            busy: Cell::new(false),
            data_version: Cell::new(None),
            save_pending: Cell::new(false),
            theme_pending: Cell::new(false),
            theme_monitors: RefCell::default(),
        });
        ui.apply_theme();
        ui.watch_theme();
        ui.actions(app);
        ui.connect_signals();
        ui.listen_for_progress(events);
        ui.schedule_refreshes();
        ui.watch_for_outside_changes();
        ui.apply_layout();
        ui.window.present();
        ui.reload();
        ui.start_refresh(false);
        ui
    }

    fn connect_signals(self: &Rc<Self>) {
        use glib::clone;
        self.window.connect_current_breakpoint_notify(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.apply_layout()
        ));
        self.window.connect_close_request(clone!(
            #[weak(rename_to = u)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                u.remember_geometry();
                u.save_now();
                glib::Propagation::Proceed
            }
        ));
        self.header.sidebar_toggle.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| {
                let show = !u.side.root.is_visible();
                u.side.root.set_visible(show);
                if u.layout() == Layout::Narrow {
                    u.inner.set_visible(!show);
                }
            }
        ));
        for button in [&self.header.library, &self.side.manage] {
            button.connect_clicked(clone!(
                #[weak(rename_to = u)]
                self,
                move |_| crate::dialogs::library(u)
            ));
        }
        self.header.refresh.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.start_refresh(true)
        ));
        self.header.search.connect_search_changed(clone!(
            #[weak(rename_to = u)]
            self,
            move |s| {
                u.query.borrow_mut().search = s.text().to_string();
                u.page.set(0);
                u.reload_articles();
            }
        ));
        self.list.unread.connect_toggled(clone!(
            #[weak(rename_to = u)]
            self,
            move |b| {
                u.query.borrow_mut().unread_only = b.is_active();
                u.settings.borrow_mut().unread_only = b.is_active();
                u.page.set(0);
                u.reload_articles();
                u.save();
            }
        ));
        self.side.list.connect_row_selected(clone!(
            #[weak(rename_to = u)]
            self,
            move |_, row| {
                if u.rebuilding.get() {
                    return;
                }
                let scope = row.and_then(|row| {
                    let rows = u.sidebar_rows.borrow();
                    rows.get(row.index() as usize).map(|r| r.scope.clone())
                });
                if let Some(scope) = scope {
                    u.query.borrow_mut().scope = scope.clone();
                    u.settings.borrow_mut().scope = scope;
                    u.page.set(0);
                    // A different view starts with an empty reader, as in NetNewsWire.
                    u.clear_article();
                    u.reload_articles();
                    u.save();
                }
            }
        ));
        self.list.list.connect_row_selected(clone!(
            #[weak(rename_to = u)]
            self,
            move |_, row| {
                if u.rebuilding.get() {
                    return;
                }
                let id =
                    row.and_then(|row| u.articles.borrow().get(row.index() as usize).map(|a| a.id));
                if let Some(id) = id {
                    u.select(id);
                }
            }
        ));
        self.list.next.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| {
                u.page.set(u.page.get() + 1);
                u.reload_articles();
            }
        ));
        self.list.previous.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| {
                if u.page.get() > 0 {
                    u.page.set(u.page.get() - 1);
                    u.reload_articles();
                }
            }
        ));
        self.list.mark.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.mark_all()
        ));
        self.connect_reader_signals();
    }

    fn listen_for_progress(self: &Rc<Self>, events: async_channel::Receiver<Progress>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(u) = weak.upgrade() else {
                    break;
                };
                u.on_progress(event);
            }
        });
    }

    fn on_progress(self: &Rc<Self>, event: Progress) {
        match event {
            Progress::Started(n) => {
                self.busy.set(true);
                self.header.refresh.set_sensitive(false);
                if n > 0 {
                    self.set_status(&format!("Refreshing {n} feeds…"));
                }
            }
            Progress::Feed {
                title,
                error,
                done,
                total,
            } => {
                let suffix = if error.is_some() {
                    " · unavailable"
                } else {
                    ""
                };
                self.set_status(&format!("{done}/{total} · {title}{suffix}"));
            }
            Progress::Finished { total, failed } => {
                self.busy.set(false);
                self.header.refresh.set_sensitive(true);
                if total > 0 {
                    self.set_status(&format!("Refreshed {total} feeds · {failed} unavailable"));
                    // Pick up newly downloaded favicons.
                    self.favicons.borrow_mut().clear();
                    self.reload();
                }
            }
        }
    }

    fn schedule_refreshes(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(REFRESH_CHECK_SECONDS, move || {
            let Some(u) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            u.start_refresh(false);
            glib::ControlFlow::Continue
        });
    }

    /// Reload when another program changes the library, so `omafeed star 2` in a terminal
    /// shows up in an open window. The window's own writes never trigger this.
    fn watch_for_outside_changes(self: &Rc<Self>) {
        self.check_for_outside_changes();
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(OUTSIDE_CHANGE_SECONDS, move || {
            let Some(u) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            u.check_for_outside_changes();
            glib::ControlFlow::Continue
        });
    }

    fn check_for_outside_changes(self: &Rc<Self>) {
        let u = self.clone();
        glib::spawn_future_local(async move {
            let Ok(version) = u.db.call(|s| s.data_version()).await else {
                return;
            };
            // The first reading is only a baseline.
            let previous = u.data_version.replace(Some(version));
            if previous.is_some_and(|p| p != version) {
                u.reload();
            }
        });
    }

    fn layout(&self) -> Layout {
        match self.window.current_breakpoint() {
            Some(b) if b == self.narrow => Layout::Narrow,
            Some(_) => Layout::Medium,
            None => Layout::Wide,
        }
    }

    fn apply_layout(&self) {
        let layout = self.layout();
        let reading = self.selected.borrow().is_some();
        self.side.root.set_visible(layout == Layout::Wide);
        self.inner.set_visible(true);
        self.list
            .root
            .set_visible(layout != Layout::Narrow || !reading);
        self.reader
            .root
            .set_visible(layout != Layout::Narrow || reading);
        self.reader.back.set_visible(layout == Layout::Narrow);
    }

    /// In the narrow layout, switch between the list and the reader.
    fn show_reader(&self, reading: bool) {
        if self.layout() == Layout::Narrow {
            self.list.root.set_visible(!reading);
            self.reader.root.set_visible(reading);
        }
    }

    fn remember_geometry(&self) {
        let mut s = self.settings.borrow_mut();
        s.width = self.window.width();
        s.height = self.window.height();
        if self.side.root.is_visible() {
            s.sidebar_width = self.outer.position();
        }
        if self.list.root.is_visible() && self.reader.root.is_visible() {
            s.list_width = self.inner.position();
        }
    }

    pub fn toast(&self, text: &str) {
        self.overlay.add_toast(adw::Toast::new(text));
    }

    pub fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    pub fn error(&self, e: impl std::fmt::Display) {
        let text = e.to_string();
        self.set_status(&text);
        self.toast(&text);
        eprintln!("{text}");
    }

    /// Save settings shortly, coalescing bursts of changes into one write.
    pub fn save(self: &Rc<Self>) {
        if self.save_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(SAVE_DELAY, move || {
            if let Some(u) = weak.upgrade()
                && u.save_pending.get()
            {
                u.save_now();
            }
        });
    }

    fn save_now(&self) {
        self.save_pending.set(false);
        if let Err(e) = self.settings.borrow().save(&self.paths) {
            self.error(e);
        }
    }

    /// Apply edited reader and appearance settings.
    pub fn apply_settings(self: &Rc<Self>, settings: Settings) {
        *self.settings.borrow_mut() = settings;
        self.save();
        self.reload_palette(true);
    }

    /// Show "All Articles" with nothing selected, e.g. after removing the current feed.
    pub fn reset_selection(self: &Rc<Self>) {
        self.query.borrow_mut().scope = Scope::All;
        self.settings.borrow_mut().scope = Scope::All;
        self.clear_article();
        self.save();
        self.reload();
    }

    /// Close the current article and show the reader's empty state.
    fn clear_article(&self) {
        // Discard any article load still in flight.
        self.selection_generation
            .set(self.selection_generation.get() + 1);
        self.pending_mark.set(None);
        *self.selected.borrow_mut() = None;
        self.settings.borrow_mut().selected_article = None;
        self.render();
        self.show_reader(false);
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

    /// Update unread counts without reloading the article list.
    fn refresh_counts(self: &Rc<Self>) {
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(|s| s.library()).await {
                Ok(lib) => {
                    *u.library.borrow_mut() = lib;
                    u.update_sidebar();
                }
                Err(e) => u.error(e),
            }
        });
    }

    pub async fn discover(
        &self,
        url: String,
    ) -> anyhow::Result<Vec<omafeed_core::discovery::Candidate>> {
        let refresher = self.refresher.clone();
        let task = self
            .runtime
            .spawn(async move { refresher.discover(&url).await });
        struct AbortOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _cancel = AbortOnDrop(task.abort_handle());
        task.await?
    }

    pub fn start_refresh(self: &Rc<Self>, force: bool) {
        if self.busy.get() {
            return;
        }
        let db = self.db.clone();
        let refresher = self.refresher.clone();
        let minutes = self.settings.borrow().refresh_minutes;
        let tx = self.progress.clone();
        let (err_tx, err_rx) = async_channel::bounded(1);
        self.runtime.spawn(async move {
            if let Err(e) = refresher.refresh(db, force, minutes, tx).await {
                let _ = err_tx.send(e.to_string()).await;
            }
        });
        let u = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(e) = err_rx.recv().await {
                u.busy.set(false);
                u.header.refresh.set_sensitive(true);
                u.error(e);
            }
        });
    }
}
