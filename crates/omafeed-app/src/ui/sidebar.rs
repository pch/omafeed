//! Library sidebar: smart views, folders, and feeds with unread counts.
use super::{Ui, widgets::icon, widgets::label};
use gtk::{gdk, gio, glib, prelude::*};
use omafeed_core::db::{Feed, Library, Scope};
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
    row: gtk::ListBoxRow,
    count: gtk::Label,
    warning: gtk::Image,
}

fn items(lib: &Library) -> Vec<Item> {
    fn tree(lib: &Library, parent: Option<i64>, depth: i32, items: &mut Vec<Item>) {
        for f in lib.folders.iter().filter(|f| f.parent == parent) {
            let index = items.len();
            items.push(Item {
                scope: Scope::Folder(f.id),
                title: f.name.clone(),
                count: 0,
                depth,
                feed: None,
            });
            tree(lib, Some(f.id), depth + 1, items);
            items[index].count = items[index + 1..]
                .iter()
                .filter(|i| i.feed.is_some())
                .map(|i| i.count)
                .sum();
        }
        for f in lib.feeds.iter().filter(|f| f.folder == parent) {
            items.push(Item {
                scope: Scope::Feed(f.id),
                title: f.title.clone(),
                count: f.unread,
                depth,
                feed: Some(f.clone()),
            });
        }
    }
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
    tree(lib, None, 0, &mut items);
    items
}

/// Drop items inside collapsed folders.
fn visible(items: Vec<Item>, collapsed: &std::collections::HashSet<i64>) -> Vec<Item> {
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

impl Ui {
    fn sidebar_items(&self) -> Vec<Item> {
        visible(items(&self.library.borrow()), &self.collapsed.borrow())
    }

    pub(super) fn rebuild_sidebar(self: &Rc<Self>) {
        self.rebuilding.set(true);
        while let Some(child) = self.side.list.first_child() {
            self.side.list.remove(&child);
        }
        let selected = self.query.borrow().scope.clone();
        let rows: Vec<Row> = self
            .sidebar_items()
            .into_iter()
            .map(|item| self.sidebar_row(item))
            .collect();
        for r in &rows {
            self.side.list.append(&r.row);
            if r.scope == selected {
                self.side.list.select_row(Some(&r.row));
            }
        }
        *self.sidebar_rows.borrow_mut() = rows;
        self.rebuilding.set(false);
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
        let path = omafeed_core::icons::path(&self.paths.cache, feed.website());
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

    fn folder_expander(self: &Rc<Self>, id: i64) -> gtk::Button {
        let collapsed = self.collapsed.borrow().contains(&id);
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
                let mut collapsed = u.collapsed.borrow_mut();
                if !collapsed.remove(&id) {
                    collapsed.insert(id);
                }
                drop(collapsed);
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
        match (&item.scope, &item.feed) {
            (Scope::Folder(id), _) => {
                content.append(&self.folder_expander(*id));
                text.add_css_class("folder-name");
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
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
        if let Some(feed) = &item.feed {
            row.set_tooltip_text(Some(&tooltip(feed)));
        }
        Row {
            scope: item.scope,
            row,
            count,
            warning,
        }
    }
}
