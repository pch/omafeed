//! Library sidebar: smart views, folders, and feeds with unread counts, their context menu,
//! and moving feeds and folders by drag and drop.
use super::{Ui, context_menu, reader, widgets::icon, widgets::label};
use gtk::{gdk, gio, glib, prelude::*};
use omafeed_core::{
    ingest::Which,
    model::{Feed, FeedId, FolderId, Library, Query, Scope, TreeItem},
};
use std::rc::Rc;

/// One sidebar entry before it becomes a widget.
struct Item {
    scope: Scope,
    title: String,
    count: i64,
    depth: i32,
    feed: Option<Feed>,
}

/// Widgets that change when counts or feed errors change.
pub(super) struct Row {
    pub scope: Scope,
    pub(super) row: gtk::ListBoxRow,
    count: gtk::Label,
    warning: gtk::Image,
}

fn items(lib: &Library) -> Vec<Item> {
    let smart = |scope: Scope, count| Item {
        title: scope.label(lib),
        scope,
        count,
        depth: 0,
        feed: None,
    };
    let mut items = vec![
        smart(Scope::Unread, lib.unread),
        smart(Scope::Today, 0),
        smart(Scope::Starred, lib.starred),
        smart(Scope::All, 0),
    ];
    // Open folders, by the index of their item; a folder counts the unread feeds inside it.
    let mut open = Vec::new();
    for item in lib.tree() {
        match item {
            TreeItem::Folder { folder, depth } => {
                open.push(items.len());
                items.push(Item {
                    scope: Scope::Folder(folder.id),
                    title: folder.name.clone(),
                    count: 0,
                    depth: depth as i32,
                    feed: None,
                });
            }
            TreeItem::FolderEnd(_) => {
                let index = open.pop().expect("folders end after they start");
                items[index].count = items[index + 1..]
                    .iter()
                    .filter(|i| i.feed.is_some())
                    .map(|i| i.count)
                    .sum();
            }
            TreeItem::Feed { feed, depth } => items.push(Item {
                scope: Scope::Feed(feed.id),
                title: feed.title.clone(),
                count: feed.unread,
                depth: depth as i32,
                feed: Some(feed.clone()),
            }),
        }
    }
    items
}

/// Drop items inside collapsed folders.
fn visible(items: Vec<Item>, collapsed: &std::collections::HashSet<FolderId>) -> Vec<Item> {
    let mut hidden_below = None;
    items
        .into_iter()
        .filter(|item| {
            if hidden_below.is_some_and(|depth| item.depth > depth) {
                return false;
            }
            hidden_below = match item.scope {
                Scope::Folder(id) if collapsed.contains(&id) => Some(item.depth),
                _ => None,
            };
            true
        })
        .collect()
}

fn tooltip(feed: &Feed) -> String {
    match &feed.error {
        Some(e) => format!("{}\n{e}", feed.url),
        None => feed.url.clone(),
    }
}

/// What the context menu of a sidebar entry can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    OpenHomePage,
    CopyFeedUrl,
    CopyHomePageUrl,
    MarkRead,
    Refresh,
    AddFeed,
    NewFolder,
    Edit,
    Remove,
}

impl Command {
    /// The action's name, under the `sidebar.` prefix.
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenHomePage => "open-home-page",
            Self::CopyFeedUrl => "copy-feed-url",
            Self::CopyHomePageUrl => "copy-home-page-url",
            Self::MarkRead => "mark-read",
            Self::Refresh => "refresh",
            Self::AddFeed => "add-feed",
            Self::NewFolder => "new-folder",
            Self::Edit => "edit",
            Self::Remove => "remove",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenHomePage => "Open home page",
            Self::CopyFeedUrl => "Copy feed URL",
            Self::CopyHomePageUrl => "Copy home page URL",
            Self::MarkRead => "Mark all as read",
            Self::Refresh => "Refresh",
            Self::AddFeed => "Add feed here…",
            Self::NewFolder => "New folder inside…",
            Self::Edit => "Edit…",
            Self::Remove => "Remove…",
        }
    }
}

/// The feeds a refresh of `scope` downloads: the feed, or every feed in the folder and the
/// folders inside it.
fn feeds_of(lib: &Library, scope: &Scope) -> Vec<FeedId> {
    match scope {
        Scope::Feed(id) => vec![*id],
        Scope::Folder(id) => lib
            .feeds
            .iter()
            .filter(|f| lib.folder_path(f.folder).iter().any(|p| p.id == *id))
            .map(|f| f.id)
            .collect(),
        _ => Vec::new(),
    }
}

/// The context menu of the sidebar entry for `scope`: sections of commands, each with
/// whether it can be used now. Empty for a feed or folder no longer in the library.
/// `refreshing` tells whether a refresh is under way, which leaves no room for another.
fn menu(scope: &Scope, lib: &Library, refreshing: bool) -> Vec<Vec<(Command, bool)>> {
    // Smart views other than All Unread show no count, so they may always have unread ones.
    let unread = match scope {
        Scope::Unread => Some(lib.unread),
        Scope::Feed(_) | Scope::Folder(_) => {
            let Some(item) = items(lib).into_iter().find(|i| i.scope == *scope) else {
                return Vec::new();
            };
            Some(item.count)
        }
        _ => None,
    };
    let mark = (Command::MarkRead, unread.is_none_or(|n| n > 0));
    let refresh = (
        Command::Refresh,
        !refreshing && !feeds_of(lib, scope).is_empty(),
    );
    let manage = vec![(Command::Edit, true), (Command::Remove, true)];
    match scope {
        Scope::Feed(id) => {
            let Some(feed) = lib.feed(*id) else {
                return Vec::new();
            };
            // A feed's website is unknown until it is refreshed or imported with one.
            let home = !feed.site_url.is_empty();
            vec![
                vec![(Command::OpenHomePage, reader::opens(&feed.site_url))],
                vec![
                    (Command::CopyFeedUrl, true),
                    (Command::CopyHomePageUrl, home),
                ],
                vec![refresh, mark],
                manage,
            ]
        }
        Scope::Folder(_) => vec![
            vec![refresh, mark],
            vec![(Command::AddFeed, true), (Command::NewFolder, true)],
            manage,
        ],
        _ => vec![vec![mark]],
    }
}

/// Where dropping `dragged` on the sidebar entry `onto` moves it: into the folder dropped on,
/// into the folder of the feed dropped on, or to the top level when dropped on a smart view
/// or below the last entry. `None` if it would stay where it is, or if a folder would go
/// inside itself.
fn destination(lib: &Library, dragged: &Scope, onto: Option<&Scope>) -> Option<Option<FolderId>> {
    let target = match onto {
        Some(Scope::Folder(id)) => Some(*id),
        Some(Scope::Feed(id)) => lib.feed(*id)?.folder,
        _ => None,
    };
    let current = match dragged {
        Scope::Feed(id) => lib.feed(*id)?.folder,
        Scope::Folder(id) => {
            let inside = lib.folder_path(target).iter().any(|f| f.id == *id);
            if inside {
                return None;
            }
            lib.folder(*id)?.parent
        }
        _ => return None,
    };
    (current != target).then_some(target)
}

/// Distance (px) from the sidebar's top or bottom edge within which a drag scrolls it.
const DRAG_SCROLL_EDGE: f64 = 32.0;
const DRAG_SCROLL_STEP: f64 = 12.0;

impl Ui {
    /// Let a feed or folder row be dragged, carrying its scope as text and showing the row.
    fn drag_source(self: &Rc<Self>, scope: &Scope, content: &gtk::Box) -> gtk::DragSource {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.set_content(Some(&gdk::ContentProvider::for_value(
            &scope.to_string().to_value(),
        )));
        let content = content.downgrade();
        let scope = scope.clone();
        source.connect_drag_begin(glib::clone!(
            #[weak(rename_to = u)]
            self,
            move |source, _| {
                u.dragging.replace(Some(scope.clone()));
                if let Some(content) = content.upgrade() {
                    let icon = gtk::WidgetPaintable::new(Some(&content));
                    source.set_icon(Some(&icon), 0, 0);
                }
            }
        ));
        source.connect_drag_end(glib::clone!(
            #[weak(rename_to = u)]
            self,
            move |_, _, _| {
                u.dragging.replace(None);
            }
        ));
        source
    }

    /// The feed or folder a drop carries, if it is the one this window's drag started with
    /// and the library has not been read again since: its ID may now be another's.
    fn dragged(&self, value: Option<glib::Value>) -> Option<Scope> {
        let scope: Scope = value?.get::<String>().ok()?.parse().ok()?;
        (self.dragging.borrow().as_ref() == Some(&scope)).then_some(scope)
    }

    /// The folder new feeds and folders go into: the one shown, or the shown feed's.
    pub(super) fn selected_folder(&self) -> Option<FolderId> {
        match &self.view.borrow().query.scope {
            Scope::Folder(id) => Some(*id),
            Scope::Feed(id) => self.library.borrow().feed(*id).and_then(|f| f.folder),
            _ => None,
        }
    }

    fn sidebar_items(&self) -> Vec<Item> {
        visible(
            items(&self.library.borrow()),
            self.view.borrow().collapsed(),
        )
    }

    pub(super) fn rebuild_sidebar(self: &Rc<Self>) {
        self.rebuilding.set(true);
        // Rows are replaced, so keyboard focus on one moves to its replacement.
        let focused = self
            .sidebar_rows
            .borrow()
            .iter()
            .find(|r| r.row.is_focus())
            .map(|r| r.scope.clone());
        self.side.list.remove_all();
        let selected = self.view.borrow().query.scope.clone();
        let rows: Vec<Row> = self
            .sidebar_items()
            .into_iter()
            .map(|item| self.sidebar_row(item))
            .collect();
        // Feeds and folders start a little below the smart views.
        if let Some(r) = rows
            .iter()
            .find(|r| matches!(r.scope, Scope::Feed(_) | Scope::Folder(_)))
        {
            r.row.add_css_class("library-start");
        }
        for r in &rows {
            self.side.list.append(&r.row);
            if r.scope == selected {
                self.side.list.select_row(Some(&r.row));
            }
        }
        *self.sidebar_rows.borrow_mut() = rows;
        if let Some(scope) = focused {
            self.focus_sidebar_row(&scope);
        }
        self.rebuilding.set(false);
    }

    /// Give keyboard focus to `scope`'s sidebar row, which scrolls it into view. Not while
    /// the layout hides the sidebar: GTK would move focus on from a row nobody can see.
    pub(super) fn focus_sidebar_row(&self, scope: &Scope) {
        if !self.side.root.is_visible() {
            return;
        }
        let row = self
            .sidebar_rows
            .borrow()
            .iter()
            .find(|r| r.scope == *scope)
            .map(|r| r.row.clone());
        if let Some(row) = row {
            row.grab_focus();
        }
    }

    /// Refresh counts and warnings in place; rebuild only if the rows changed.
    pub(super) fn update_sidebar(self: &Rc<Self>) {
        let items = self.sidebar_items();
        let same = {
            let rows = self.sidebar_rows.borrow();
            rows.len() == items.len() && rows.iter().zip(&items).all(|(r, i)| r.scope == i.scope)
        };
        if !same {
            self.rebuild_sidebar();
            return;
        }
        for (row, item) in self.sidebar_rows.borrow().iter().zip(items) {
            row.count.set_text(&item.count.to_string());
            row.count.set_visible(item.count > 0);
            if let Some(feed) = &item.feed {
                row.warning.set_visible(feed.error.is_some());
                row.row.set_tooltip_text(Some(&tooltip(feed)));
            }
        }
    }

    fn favicon(&self, feed: &Feed) -> gtk::Image {
        let path = omafeed_core::ingest::icons::path(&self.paths.cache, feed.website());
        let texture = self
            .favicons
            .borrow_mut()
            .entry(path)
            .or_insert_with_key(|path| gdk::Texture::from_file(&gio::File::for_path(path)).ok())
            .clone();
        let image = match texture {
            Some(texture) => gtk::Image::from_paintable(Some(&texture)),
            None => gtk::Image::from_icon_name("application-rss+xml-symbolic"),
        };
        image.set_pixel_size(16);
        image
    }

    fn folder_expander(self: &Rc<Self>, id: FolderId) -> gtk::Button {
        let collapsed = self.view.borrow().collapsed().contains(&id);
        let (name, tip) = if collapsed {
            ("pan-end-symbolic", "Expand folder")
        } else {
            ("pan-down-symbolic", "Collapse folder")
        };
        let expander = icon(name, tip);
        expander.add_css_class("flat");
        expander.add_css_class("folder-expander");
        expander.connect_clicked(glib::clone!(
            #[weak(rename_to = u)]
            self,
            move |_| {
                u.view.borrow_mut().toggle_folder(id);
                u.rebuild_sidebar();
            }
        ));
        expander
    }

    fn sidebar_row(self: &Rc<Self>, item: Item) -> Row {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_start(item.depth * 14);
        content.set_height_request(28);
        let text = label(&item.title, "");
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.set_hexpand(true);
        // A folder's icon stands where the others do, so names line up; its expander goes
        // at the end of the row.
        let mut expander = None;
        match (&item.scope, &item.feed) {
            (Scope::Folder(id), _) => {
                let open = !self.view.borrow().collapsed().contains(id);
                let symbol = if open {
                    "folder-open-symbolic"
                } else {
                    "folder-symbolic"
                };
                let image = gtk::Image::from_icon_name(symbol);
                image.set_pixel_size(16);
                content.append(&image);
                text.add_css_class("folder-name");
                expander = Some(self.folder_expander(*id));
            }
            (Scope::Feed(_), Some(feed)) => content.append(&self.favicon(feed)),
            (scope, _) => {
                let symbol = match scope {
                    Scope::Unread => "mail-unread-symbolic",
                    Scope::Today => "x-office-calendar-symbolic",
                    Scope::Starred => "starred-symbolic",
                    _ => "view-list-symbolic",
                };
                let image = gtk::Image::from_icon_name(symbol);
                image.set_pixel_size(16);
                content.append(&image);
            }
        }
        content.append(&text);
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.set_visible(item.feed.as_ref().is_some_and(|f| f.error.is_some()));
        content.append(&warning);
        let count = label(&item.count.to_string(), "unread-count");
        count.set_visible(item.count > 0);
        content.append(&count);
        if let Some(expander) = &expander {
            content.append(expander);
        }
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
        if let Some(feed) = &item.feed {
            row.set_tooltip_text(Some(&tooltip(feed)));
        }
        if matches!(item.scope, Scope::Feed(_) | Scope::Folder(_)) {
            row.add_controller(self.drag_source(&item.scope, &content));
        }
        Row {
            scope: item.scope,
            row,
            count,
            warning,
        }
    }

    /// Open the context menu on right click, and with the Menu key or Shift+F10 on the
    /// focused entry.
    pub(super) fn connect_sidebar_menu(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        context_menu::connect(&self.side.list, move |row, at| {
            if let Some(u) = weak.upgrade() {
                u.open_sidebar_menu(row, at);
            }
        });
    }

    /// Show the context menu of a sidebar row, at `at` in the list's coordinates or, without
    /// it, below the row. The row is not selected, so the view shown stays.
    pub(super) fn open_sidebar_menu(
        self: &Rc<Self>,
        row: &gtk::ListBoxRow,
        at: Option<(f64, f64)>,
    ) {
        let scope = {
            let rows = self.sidebar_rows.borrow();
            let Some(r) = usize::try_from(row.index()).ok().and_then(|i| rows.get(i)) else {
                return;
            };
            r.scope.clone()
        };
        let sections = menu(&scope, &self.library.borrow(), self.busy.get());
        if sections.is_empty() {
            return;
        }
        let sections = sections
            .into_iter()
            .map(|section| {
                section
                    .into_iter()
                    .map(|(command, enabled)| {
                        let weak = Rc::downgrade(self);
                        let scope = scope.clone();
                        context_menu::Item {
                            label: command.label().into(),
                            name: command.name(),
                            enabled,
                            run: Box::new(move || {
                                if let Some(u) = weak.upgrade() {
                                    u.run_command(command, &scope);
                                }
                            }),
                        }
                    })
                    .collect()
            })
            .collect();
        context_menu::show(
            &self.side.menu,
            "sidebar",
            sections,
            self.side.root.upcast_ref(),
            &self.side.list,
            row,
            at,
        );
    }

    /// Close the context menu and forget its actions, and forget a drag in progress. Called
    /// whenever the library is read again: the entry the menu was opened on or the drag
    /// started from may have been removed meanwhile, and its ID reused for another feed or
    /// folder, so neither may act on it any more.
    pub(crate) fn close_sidebar_menu(&self) {
        self.dragging.replace(None);
        context_menu::close(&self.side.menu, "sidebar");
    }

    fn run_command(self: &Rc<Self>, command: Command, scope: &Scope) {
        let feed = match scope {
            Scope::Feed(id) => self.library.borrow().feed(*id).cloned(),
            _ => None,
        };
        match (command, feed) {
            (Command::OpenHomePage, Some(feed)) => reader::open_uri(&feed.site_url),
            (Command::CopyFeedUrl, Some(feed)) => self.copy(&feed.url, "Feed URL copied"),
            (Command::CopyHomePageUrl, Some(feed)) if !feed.site_url.is_empty() => {
                self.copy(&feed.site_url, "Home page URL copied");
            }
            (Command::MarkRead, _) => self.mark_read(Query {
                scope: scope.clone(),
                ..Default::default()
            }),
            (Command::Refresh, _) => {
                let feeds = feeds_of(&self.library.borrow(), scope);
                self.start_refresh(Which::Only(feeds));
            }
            (Command::AddFeed | Command::NewFolder, _) => {
                let Scope::Folder(id) = scope else { return };
                if command == Command::AddFeed {
                    crate::dialogs::add_feed(self.clone(), Some(*id));
                } else {
                    crate::dialogs::new_folder(self.clone(), Some(*id));
                }
            }
            (Command::Edit, _) => crate::dialogs::edit(self.clone(), scope.clone()),
            (Command::Remove, _) => crate::dialogs::remove(self.clone(), scope.clone()),
            _ => {}
        }
    }

    /// The sidebar entry at `y` in the list's coordinates, if any.
    fn scope_at(&self, y: f64) -> Option<Scope> {
        #[expect(clippy::cast_possible_truncation, reason = "pixel coordinates")]
        let row = self.side.list.row_at_y(y as i32)?;
        let index = usize::try_from(row.index()).ok()?;
        self.sidebar_rows
            .borrow()
            .get(index)
            .map(|r| r.scope.clone())
    }

    /// Highlight where a drop would put the dragged entry: the folder's row, or the whole
    /// list for the top level. Nothing for a drop that would not move it.
    fn highlight_drop(&self, to: Option<Option<FolderId>>) {
        let list = &self.side.list;
        list.drag_unhighlight_row();
        list.remove_css_class("drop-top-level");
        match to {
            Some(Some(folder)) => {
                let rows = self.sidebar_rows.borrow();
                if let Some(r) = rows.iter().find(|r| r.scope == Scope::Folder(folder)) {
                    list.drag_highlight_row(&r.row);
                }
            }
            Some(None) => list.add_css_class("drop-top-level"),
            None => {}
        }
    }

    /// Scroll the sidebar while a drag is near its top or bottom edge.
    fn scroll_during_drag(&self, y: f64) {
        let Some(scrolled) = self
            .side
            .list
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
        else {
            return;
        };
        let adj = scrolled.vadjustment();
        let visible = y - adj.value();
        if visible < DRAG_SCROLL_EDGE {
            adj.set_value(adj.value() - DRAG_SCROLL_STEP);
        } else if visible > adj.page_size() - DRAG_SCROLL_EDGE {
            adj.set_value(adj.value() + DRAG_SCROLL_STEP);
        }
    }

    /// Accept feeds and folders dragged within the sidebar, moving them where they land.
    pub(super) fn connect_sidebar_drop(self: &Rc<Self>) {
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        target.set_preload(true);
        target.connect_motion(glib::clone!(
            #[weak(rename_to = u)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target, _, y| {
                u.scroll_during_drag(y);
                let to = u.dragged(target.value()).and_then(|scope| {
                    destination(&u.library.borrow(), &scope, u.scope_at(y).as_ref())
                });
                u.highlight_drop(to);
                if to.is_some() {
                    gdk::DragAction::MOVE
                } else {
                    gdk::DragAction::empty()
                }
            }
        ));
        target.connect_leave(glib::clone!(
            #[weak(rename_to = u)]
            self,
            move |_| u.highlight_drop(None)
        ));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = u)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, y| {
                u.highlight_drop(None);
                let Some(scope) = u.dragged(Some(value.clone())) else {
                    return false;
                };
                let to = destination(&u.library.borrow(), &scope, u.scope_at(y).as_ref());
                match to {
                    Some(folder) => {
                        u.move_entry(scope, folder);
                        true
                    }
                    None => false,
                }
            }
        ));
        self.side.list.add_controller(target);
    }

    /// Expand `folder` and the folders above it, so what is in it shows.
    pub(super) fn expand_to(&self, folder: Option<FolderId>) {
        let lib = self.library.borrow();
        let mut view = self.view.borrow_mut();
        for f in lib.folder_path(folder) {
            if view.collapsed().contains(&f.id) {
                view.toggle_folder(f.id);
            }
        }
    }

    /// Move a feed or folder into `folder`, or to the top level, and show it there.
    pub(super) fn move_entry(self: &Rc<Self>, scope: Scope, folder: Option<FolderId>) {
        // Open the folders it lands in, so it does not seem to vanish.
        self.expand_to(folder);
        let u = self.clone();
        glib::spawn_future_local(async move {
            let result =
                u.db.call(move |s| match scope {
                    Scope::Feed(id) => s.move_feed(id, folder),
                    Scope::Folder(id) => s.move_folder(id, folder),
                    _ => Ok(()),
                })
                .await;
            if let Err(e) = result {
                u.error(e);
            }
            u.reload();
        });
    }

    fn copy(&self, text: &str, done: &str) {
        self.window.clipboard().set_text(text);
        self.toast(done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omafeed_core::model::{FeedId, Folder};

    fn feed(id: i64, folder: Option<i64>, site_url: &str, unread: i64) -> Feed {
        Feed {
            id: FeedId(id),
            folder: folder.map(FolderId),
            title: format!("Feed {id}"),
            url: format!("https://example.org/{id}.xml"),
            site_url: site_url.into(),
            unread,
            error: None,
            etag: None,
            modified: None,
            next_fetch: 0,
            last_attempt: None,
            failures: 0,
            revision: 0,
        }
    }

    fn commands(menu: &[Vec<(Command, bool)>]) -> Vec<Vec<Command>> {
        menu.iter()
            .map(|section| section.iter().map(|(c, _)| *c).collect())
            .collect()
    }

    fn enabled(menu: &[Vec<(Command, bool)>], command: Command) -> bool {
        menu.iter()
            .flatten()
            .find(|(c, _)| *c == command)
            .unwrap_or_else(|| panic!("{command:?} is not in the menu"))
            .1
    }

    #[test]
    fn feeds_folders_and_views_get_their_own_menus() {
        use Command::*;
        let lib = Library {
            folders: vec![
                Folder {
                    id: FolderId(1),
                    parent: None,
                    name: "Read".into(),
                },
                Folder {
                    id: FolderId(2),
                    parent: Some(FolderId(1)),
                    name: "Nested".into(),
                },
            ],
            feeds: vec![
                feed(1, Some(1), "", 0),
                feed(2, Some(2), "https://example.org/", 3),
                feed(3, None, "feed://example.org/", 0),
            ],
            unread: 3,
            starred: 0,
        };
        let new = menu(&Scope::Feed(FeedId(1)), &lib, false);
        assert_eq!(
            commands(&new),
            [
                vec![OpenHomePage],
                vec![CopyFeedUrl, CopyHomePageUrl],
                vec![Refresh, MarkRead],
                vec![Edit, Remove],
            ]
        );
        // Not refreshed yet, and nothing unread.
        assert!(!enabled(&new, OpenHomePage));
        assert!(!enabled(&new, CopyHomePageUrl));
        assert!(!enabled(&new, MarkRead));
        assert!(enabled(&new, CopyFeedUrl));
        assert!(enabled(&new, Refresh));
        // One refresh at a time.
        assert!(!enabled(
            &menu(&Scope::Feed(FeedId(1)), &lib, true),
            Refresh
        ));
        let known = menu(&Scope::Feed(FeedId(2)), &lib, false);
        assert!(enabled(&known, OpenHomePage));
        assert!(enabled(&known, CopyHomePageUrl));
        assert!(enabled(&known, MarkRead));
        // A website the browser is not given can still be copied.
        let other = menu(&Scope::Feed(FeedId(3)), &lib, false);
        assert!(!enabled(&other, OpenHomePage));
        assert!(enabled(&other, CopyHomePageUrl));
        // A folder counts the unread articles of the folders inside it.
        let folder = menu(&Scope::Folder(FolderId(1)), &lib, false);
        assert_eq!(
            commands(&folder),
            [
                vec![Refresh, MarkRead],
                vec![AddFeed, NewFolder],
                vec![Edit, Remove]
            ]
        );
        assert!(enabled(&folder, MarkRead));
        // A folder refreshes the feeds in it and in the folders inside it.
        assert!(enabled(&folder, Refresh));
        assert_eq!(
            feeds_of(&lib, &Scope::Folder(FolderId(1))),
            [FeedId(1), FeedId(2)]
        );
        assert_eq!(feeds_of(&lib, &Scope::Folder(FolderId(2))), [FeedId(2)]);
        let empty = Library {
            folders: lib.folders.clone(),
            ..Library::default()
        };
        assert!(!enabled(
            &menu(&Scope::Folder(FolderId(1)), &empty, false),
            Refresh
        ));
        assert_eq!(
            commands(&menu(&Scope::Starred, &lib, false)),
            [vec![MarkRead]]
        );
        assert!(enabled(&menu(&Scope::Today, &lib, false), MarkRead));
        let read = Library {
            unread: 0,
            ..Library::default()
        };
        assert!(!enabled(&menu(&Scope::Unread, &read, false), MarkRead));
        assert!(menu(&Scope::Feed(FeedId(9)), &lib, false).is_empty());
        assert!(menu(&Scope::Folder(FolderId(9)), &lib, false).is_empty());
    }

    #[test]
    fn drops_move_into_folders_or_to_the_top_level() {
        let lib = Library {
            folders: vec![
                Folder {
                    id: FolderId(1),
                    parent: None,
                    name: "Read".into(),
                },
                Folder {
                    id: FolderId(2),
                    parent: Some(FolderId(1)),
                    name: "Nested".into(),
                },
                Folder {
                    id: FolderId(3),
                    parent: None,
                    name: "Other".into(),
                },
            ],
            feeds: vec![
                feed(1, Some(1), "", 0),
                feed(2, None, "", 0),
                feed(3, Some(3), "", 0),
            ],
            ..Library::default()
        };
        let feed1 = Scope::Feed(FeedId(1));
        let into = |scope: &Scope, onto: Scope| destination(&lib, scope, Some(&onto));
        // Onto a folder, into the folder; onto a feed, beside it.
        assert_eq!(
            into(&feed1, Scope::Folder(FolderId(2))),
            Some(Some(FolderId(2)))
        );
        assert_eq!(
            into(&feed1, Scope::Feed(FeedId(3))),
            Some(Some(FolderId(3)))
        );
        // Onto a smart view, a top-level feed, or below everything: the top level.
        assert_eq!(into(&feed1, Scope::Starred), Some(None));
        assert_eq!(into(&feed1, Scope::Feed(FeedId(2))), Some(None));
        assert_eq!(destination(&lib, &feed1, None), Some(None));
        // Where it already is: nothing to do.
        assert_eq!(into(&feed1, Scope::Folder(FolderId(1))), None);
        assert_eq!(destination(&lib, &Scope::Feed(FeedId(2)), None), None);
        // A folder never goes inside itself or its children.
        let read = Scope::Folder(FolderId(1));
        assert_eq!(into(&read, Scope::Folder(FolderId(1))), None);
        assert_eq!(into(&read, Scope::Folder(FolderId(2))), None);
        assert_eq!(
            into(&read, Scope::Folder(FolderId(3))),
            Some(Some(FolderId(3)))
        );
        assert_eq!(into(&Scope::Folder(FolderId(2)), Scope::All), Some(None));
        assert_eq!(into(&Scope::Starred, Scope::Folder(FolderId(3))), None);
        assert_eq!(into(&Scope::Feed(FeedId(9)), Scope::All), None);
    }
}
