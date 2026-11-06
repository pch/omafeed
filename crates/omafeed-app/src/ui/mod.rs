//! Main window: a library sidebar, an article list, and a WebKit reader.
mod articles;
mod changes;
mod context_menu;
mod keys;
mod reader;
mod sidebar;
#[cfg(feature = "test-support")]
pub(crate) mod tests;
mod theme;
mod view;
pub mod widgets;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use omafeed_core::{
    Db, Paths, Settings,
    config::Palette,
    ingest::{Progress, Refresher, Which},
    model::{FolderId, Library, Query, Scope},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use view::Reloaded;
use widgets::{icon, label, margins, scroll};

/// The refresh button's tooltip when nothing has been refreshed yet.
const REFRESH_TIP: &str = "Refresh feeds (Ctrl+R)";
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
    brand: gtk::Label,
    refresh: gtk::Button,
    sidebar_toggle: gtk::Button,
    search: gtk::SearchEntry,
}

impl Header {
    fn new() -> Self {
        let bar = adw::HeaderBar::new();
        let brand = label("omafeed", "brand");
        bar.set_title_widget(Some(&brand));
        let refresh = icon("view-refresh-symbolic", REFRESH_TIP);
        let sidebar_toggle = icon("sidebar-show-symbolic", "Show or hide subscriptions");
        for b in [&refresh, &sidebar_toggle] {
            bar.pack_start(b);
        }
        let add_menu = gio::Menu::new();
        add_menu.append(Some("Add feed…"), Some("win.add-feed"));
        add_menu.append(Some("New folder…"), Some("win.new-folder"));
        bar.pack_start(
            &gtk::MenuButton::builder()
                .icon_name("list-add-symbolic")
                .tooltip_text("Add a feed or folder")
                .menu_model(&add_menu)
                .build(),
        );
        let menu = gio::Menu::new();
        for (title, action) in [
            ("Import OPML…", "win.import"),
            ("Export OPML…", "win.export"),
            ("Content filters…", "win.filters"),
            ("Settings…", "win.settings"),
            ("Keyboard shortcuts", "win.shortcuts"),
            ("About omafeed", "win.about"),
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
            search,
        }
    }
}

struct SidebarPane {
    root: gtk::Box,
    list: gtk::ListBox,
    /// The context menu of feeds, folders and views, filled in each time it opens.
    menu: gtk::PopoverMenu,
}

impl SidebarPane {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_width_request(*Settings::SIDEBAR_WIDTHS.start());
        root.add_css_class("sidebar");
        let list = gtk::ListBox::new();
        list.set_margin_top(8);
        list.add_css_class("navigation-sidebar");
        root.append(&scroll(&list));
        let menu = context_menu::popover(&root);
        Self { root, list, menu }
    }
}

struct ListPane {
    root: gtk::Box,
    heading: gtk::Label,
    count: gtk::Label,
    unread: gtk::ToggleButton,
    mark: gtk::Button,
    list: gtk::ListBox,
    /// The context menu of articles, filled in each time it opens.
    menu: gtk::PopoverMenu,
    /// The article the menu was last opened on.
    menu_article: Cell<Option<omafeed_core::model::ArticleId>>,
    /// The page buttons, shown only when there is more than one page.
    pages: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
}

impl ListPane {
    fn new(unread_only: bool) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_width_request(*Settings::LIST_WIDTHS.start());
        let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
        margins(&header, 16);
        let heading = label("All Unread", "list-heading");
        heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&heading);
        let options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let count = label("", "dim-label");
        count.set_hexpand(true);
        options.append(&count);
        // Pages hold hundreds of articles, so these rarely show.
        let previous = icon("go-previous-symbolic", "Previous page");
        let next = icon("go-next-symbolic", "Next page");
        let pages = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pages.add_css_class("linked");
        pages.add_css_class("pagination");
        for b in [&previous, &next] {
            b.set_sensitive(false);
            pages.append(b);
        }
        pages.set_visible(false);
        options.append(&pages);
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
        let menu = context_menu::popover(&root);
        Self {
            root,
            heading,
            count,
            unread,
            mark,
            list,
            menu,
            menu_article: Cell::default(),
            pages,
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
    /// The line along the top of the panes showing how far a refresh got.
    refresh_line: gtk::ProgressBar,
    overlay: adw::ToastOverlay,
    narrow: adw::Breakpoint,
    css: gtk::CssProvider,

    pub(crate) settings: RefCell<Settings>,
    pub(crate) library: RefCell<Library>,
    /// What is shown: view, page, articles, the open article, and changes in flight.
    view: RefCell<view::View>,
    palette: RefCell<Palette>,
    article_rows: RefCell<Vec<articles::ArticleRow>>,
    sidebar_rows: RefCell<Vec<sidebar::Row>>,
    /// Decoded favicons by cache path; `None` records a missing icon.
    favicons: RefCell<HashMap<PathBuf, Option<gdk::Texture>>>,
    /// The feed or folder being dragged in the sidebar, until the drag ends or the library
    /// is read again.
    dragging: RefCell<Option<Scope>>,
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
        // A thin line along the top of the panes while feeds refresh, over them rather than
        // pushing them down.
        let refresh_line = gtk::ProgressBar::builder()
            .valign(gtk::Align::Start)
            .can_target(false)
            .visible(false)
            .build();
        refresh_line.add_css_class("refresh-progress");
        let panes = gtk::Overlay::new();
        panes.set_child(Some(&outer));
        panes.add_overlay(&refresh_line);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&header.bar);
        root.append(&panes);
        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&root));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("omafeed")
            .default_width(settings.width)
            .default_height(settings.height)
            .width_request(*Settings::WIDTHS.start())
            .height_request(*Settings::HEIGHTS.start())
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
            refresh_line,
            overlay,
            narrow,
            css,
            settings: RefCell::new(settings),
            library: RefCell::default(),
            view: RefCell::new(view::View::new(query)),
            palette: RefCell::new(palette),
            article_rows: RefCell::default(),
            sidebar_rows: RefCell::default(),
            favicons: RefCell::default(),
            dragging: RefCell::default(),
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
        ui.start_refresh(Which::Due);
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
        self.header.refresh.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.start_refresh(Which::All)
        ));
        self.header.search.connect_search_changed(clone!(
            #[weak(rename_to = u)]
            self,
            move |s| {
                u.view.borrow_mut().set_search(s.text().to_string());
                u.reload_articles();
            }
        ));
        self.list.unread.connect_toggled(clone!(
            #[weak(rename_to = u)]
            self,
            move |b| {
                u.view.borrow_mut().set_unread_only(b.is_active());
                u.settings.borrow_mut().unread_only = b.is_active();
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
                    u.view.borrow_mut().set_scope(scope.clone());
                    u.settings.borrow_mut().scope = scope;
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
                let id = row.and_then(|row| {
                    u.view
                        .borrow()
                        .articles()
                        .get(row.index() as usize)
                        .map(|a| a.id)
                });
                if let Some(id) = id {
                    u.select(id);
                }
            }
        ));
        self.list.next.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| {
                u.view.borrow_mut().next_page();
                u.reload_articles();
            }
        ));
        self.list.previous.connect_clicked(clone!(
            #[weak(rename_to = u)]
            self,
            move |_| {
                let moved = u.view.borrow_mut().previous_page();
                if moved {
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
        self.connect_sidebar_menu();
        self.connect_article_menu();
        self.connect_sidebar_drop();
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
                // Most scheduled checks find nothing due; those pass without a trace.
                if n > 0 {
                    self.show_refreshing(0, n);
                }
            }
            Progress::Feed { done, total, .. } => self.show_refreshing(done, total),
            Progress::Finished { total, failed } => {
                self.refresh_stopped();
                let refresh = &self.header.refresh;
                if total > 0 {
                    let feeds = if total == 1 { "feed" } else { "feeds" };
                    let time = chrono::Local::now().format("%H:%M");
                    let mut tip = format!("Refreshed {total} {feeds} at {time}");
                    if failed > 0 {
                        tip.push_str(&format!(" · {failed} unavailable"));
                    }
                    refresh.set_tooltip_text(Some(&format!("{tip}\n{REFRESH_TIP}")));
                    // Pick up newly downloaded favicons.
                    self.favicons.borrow_mut().clear();
                    self.reload();
                }
            }
        }
    }

    /// Back to idle after a refresh, finished or failed.
    fn refresh_stopped(&self) {
        self.busy.set(false);
        self.header.refresh.set_icon_name("view-refresh-symbolic");
        self.refresh_line.set_visible(false);
    }

    /// Show a refresh under way: a spinner in the refresh button, which tells how far it
    /// got, and a progress line along the top of the panes.
    fn show_refreshing(&self, done: usize, total: usize) {
        let refresh = &self.header.refresh;
        if !refresh.child().is_some_and(|c| c.is::<adw::Spinner>()) {
            refresh.set_child(Some(&adw::Spinner::new()));
        }
        refresh.set_tooltip_text(Some(&format!("Refreshing feeds · {done} of {total}")));
        #[expect(clippy::cast_precision_loss, reason = "feed counts")]
        self.refresh_line
            .set_fraction(done as f64 / total.max(1) as f64);
        self.refresh_line.set_visible(true);
    }

    fn schedule_refreshes(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(REFRESH_CHECK_SECONDS, move || {
            let Some(u) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            u.start_refresh(Which::Due);
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
                u.reload_external_changes();
            }
        });
    }

    fn reload_external_changes(self: &Rc<Self>) {
        let u = self.clone();
        let reload = self.view.borrow().start_outside_reload();
        let selected = reload.open;
        glib::spawn_future_local(async move {
            let result =
                u.db.call(move |s| {
                    // One snapshot, so the counts and the open article agree.
                    s.snapshot(|s| {
                        Ok((
                            s.library()?,
                            selected.map(|id| s.article_if_exists(id)).transpose()?,
                        ))
                    })
                })
                .await;
            match result {
                Ok((lib, article)) => {
                    u.close_sidebar_menu();
                    *u.library.borrow_mut() = lib;
                    // A result for the old selection must not replace a newly opened article.
                    let reloaded = u
                        .view
                        .borrow_mut()
                        .finish_outside_reload(&reload, article.flatten());
                    match reloaded {
                        Some(Reloaded::Updated { content_changed }) => {
                            if let Some(a) = u.view.borrow().open() {
                                u.update_buttons(a);
                            }
                            // State-only updates preserve the reader's scroll position.
                            if content_changed {
                                u.render();
                            }
                        }
                        Some(Reloaded::Gone) => {
                            u.clear_article();
                            u.save();
                        }
                        None => {}
                    }
                    u.rebuild_sidebar();
                    u.reload_articles();
                }
                Err(e) => u.error(e),
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
        let reading = self.view.borrow().open().is_some();
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

    pub fn error(&self, e: impl std::fmt::Display) {
        let text = e.to_string();
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

    /// Apply an edited list of content filters to the sidebar and the article list.
    pub fn apply_filters(self: &Rc<Self>, filters: Vec<String>) {
        self.db.set_filters(filters.clone());
        self.settings.borrow_mut().filters = filters;
        // The page the list was on may no longer exist.
        self.view.borrow_mut().first_page();
        self.save();
        self.reload();
    }

    /// Show "All Articles" with nothing selected, e.g. after removing the current feed.
    pub fn reset_selection(self: &Rc<Self>) {
        self.view.borrow_mut().query.scope = Scope::All;
        self.settings.borrow_mut().scope = Scope::All;
        self.clear_article();
        self.save();
        self.reload();
    }

    /// Show a feed or folder just added inside `folder`: open the folders above it, select
    /// it in the sidebar, scrolled into view, and list its articles.
    pub fn show_added(self: &Rc<Self>, scope: Scope, folder: Option<FolderId>) {
        self.expand_to(folder);
        self.view.borrow_mut().set_scope(scope.clone());
        self.settings.borrow_mut().scope = scope.clone();
        self.clear_article();
        self.save();
        let u = self.clone();
        glib::spawn_future_local(async move {
            if u.load_library().await {
                u.focus_sidebar_row(&scope);
            }
        });
    }

    /// Show the library without a feed or folder just removed from it: "All Articles" if it
    /// was the view shown, and no open article if that was in the removed feed.
    pub fn removed(self: &Rc<Self>, scope: &Scope) {
        let after = self.view.borrow().after_removal(scope);
        match after {
            view::AfterRemoval::ShowAll => self.reset_selection(),
            view::AfterRemoval::CloseArticle => {
                self.clear_article();
                self.save();
                self.reload();
            }
            view::AfterRemoval::Reload => self.reload(),
        }
    }

    /// Close the current article and show the reader's empty state.
    fn clear_article(&self) {
        // Discard any article load still in flight.
        self.view.borrow_mut().close();
        self.settings.borrow_mut().selected_article = None;
        self.render();
        self.show_reader(false);
    }

    pub fn reload(self: &Rc<Self>) {
        let u = self.clone();
        glib::spawn_future_local(async move {
            u.load_library().await;
        });
    }

    /// Reload the library, sidebar, and article list. Returns whether the library loaded.
    async fn load_library(self: &Rc<Self>) -> bool {
        match self.db.call(|s| s.library()).await {
            Ok(lib) => {
                self.close_sidebar_menu();
                *self.library.borrow_mut() = lib;
                self.rebuild_sidebar();
                self.reload_articles();
                true
            }
            Err(e) => {
                self.error(e);
                false
            }
        }
    }

    /// Update unread counts without reloading the article list.
    fn refresh_counts(self: &Rc<Self>) {
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(|s| s.library()).await {
                Ok(lib) => {
                    u.close_sidebar_menu();
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
    ) -> anyhow::Result<Vec<omafeed_core::ingest::discovery::Candidate>> {
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

    pub fn start_refresh(self: &Rc<Self>, which: Which) {
        if self.busy.get() {
            return;
        }
        let db = self.db.clone();
        let refresher = self.refresher.clone();
        let minutes = self.settings.borrow().refresh_minutes;
        let tx = self.progress.clone();
        let (err_tx, err_rx) = async_channel::bounded(1);
        self.runtime.spawn(async move {
            if let Err(e) = refresher.refresh(db, which, minutes, tx).await {
                let _ = err_tx.send(e.to_string()).await;
            }
        });
        let u = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(e) = err_rx.recv().await {
                u.refresh_stopped();
                u.error(e);
            }
        });
    }
}
