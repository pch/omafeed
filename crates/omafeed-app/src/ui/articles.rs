//! Article list: paging, selection, read/star state, and the context menu of articles.
use super::{
    Ui, context_menu, reader,
    view::{self, PageRequest},
    widgets::label,
};
use chrono::{DateTime, Datelike, Local};
use gtk::{glib, prelude::*};
use omafeed_core::{
    content::one_line,
    model::{Article, ArticleId, FeedId, Query, Scope, StatePatch},
    reading,
};
use std::rc::Rc;

/// Widgets that change when an article's read or star state changes.
pub(super) struct ArticleRow {
    row: gtk::ListBoxRow,
    star: gtk::Image,
}

impl ArticleRow {
    fn new(a: &Article, show_feed: bool, now: DateTime<Local>) -> Self {
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(3)
            .build();
        // Feed name on the left and date on the right; without a feed name the
        // date becomes a left-aligned dateline.
        let meta = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        meta.set_hexpand(true);
        let date = label(&format_date(a.published, now), "article-date");
        if show_feed {
            let feed = label(&a.feed_title, "article-feed");
            feed.set_hexpand(true);
            feed.set_ellipsize(gtk::pango::EllipsizeMode::End);
            meta.append(&feed);
        } else {
            date.set_hexpand(true);
        }
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.set_pixel_size(12);
        star.add_css_class("article-star");
        if show_feed {
            meta.append(&star);
            meta.append(&date);
        } else {
            meta.append(&date);
            meta.append(&star);
        }
        grid.attach(&meta, 1, 0, 1, 1);
        // Unread marker in a left gutter, aligned with the title's first line.
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("unread-dot");
        dot.set_valign(gtk::Align::Start);
        dot.set_halign(gtk::Align::Center);
        dot.set_margin_top(6);
        grid.attach(&dot, 0, 1, 1, 1);
        let title = label(&a.title, "article-title");
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_lines(3);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        grid.attach(&title, 1, 1, 1, 1);
        let text = one_line(&a.preview);
        if !text.is_empty() {
            let preview = label(&text, "preview");
            preview.set_wrap(true);
            preview.set_lines(2);
            preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
            grid.attach(&preview, 1, 2, 1, 1);
        }
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&grid));
        let this = Self { row, star };
        this.show_state(a);
        this
    }

    fn show_state(&self, a: &Article) {
        if a.read {
            self.row.add_css_class("read");
        } else {
            self.row.remove_css_class("read");
        }
        self.star.set_visible(a.starred);
    }
}

/// Compact list date: time today, then "Yesterday", weekday, month/day, and year.
fn format_date(timestamp: i64, now: DateTime<Local>) -> String {
    let Some(date) = DateTime::from_timestamp(timestamp, 0).map(|d| d.with_timezone(&Local)) else {
        return String::new();
    };
    let days = (now.date_naive() - date.date_naive()).num_days();
    let format = match days {
        0 => "%H:%M",
        1 => return "Yesterday".into(),
        2..=6 => "%A",
        _ if date.year() == now.year() => "%b %-d",
        _ => "%b %-d, %Y",
    };
    date.format(format).to_string()
}

/// What the context menu of an article can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArticleCommand {
    ToggleRead,
    ToggleStar,
    MarkAboveRead,
    MarkBelowRead,
    OpenOriginal,
    CopyLink,
    GoToFeed,
}

impl ArticleCommand {
    /// The action's name, under the `article.` prefix.
    pub fn name(self) -> &'static str {
        match self {
            Self::ToggleRead => "toggle-read",
            Self::ToggleStar => "toggle-star",
            Self::MarkAboveRead => "mark-above-read",
            Self::MarkBelowRead => "mark-below-read",
            Self::OpenOriginal => "open-original",
            Self::CopyLink => "copy-link",
            Self::GoToFeed => "go-to-feed",
        }
    }

    fn label(self, a: &Article) -> &'static str {
        match self {
            Self::ToggleRead if a.read => "Mark as unread",
            Self::ToggleRead => "Mark as read",
            Self::ToggleStar if a.starred => "Unstar",
            Self::ToggleStar => "Star",
            Self::MarkAboveRead => "Mark above as read",
            Self::MarkBelowRead => "Mark below as read",
            Self::OpenOriginal => "Open in browser",
            Self::CopyLink => "Copy link",
            Self::GoToFeed => "Go to feed",
        }
    }
}

/// The unread articles listed above (`below` false) or below the one at `index`.
fn unread_beside(articles: &[Article], index: usize, below: bool) -> Vec<ArticleId> {
    let (above, rest) = articles.split_at(index.min(articles.len()));
    let side = if below {
        rest.get(1..).unwrap_or_default()
    } else {
        above
    };
    side.iter().filter(|a| !a.read).map(|a| a.id).collect()
}

/// The context menu of the article at `index` in `articles`, listed in `scope`: sections of
/// commands, each with whether it can be used now. Empty if there is no such article.
fn article_menu(
    articles: &[Article],
    index: usize,
    scope: &Scope,
) -> Vec<Vec<(ArticleCommand, bool)>> {
    use ArticleCommand::*;
    let Some(a) = articles.get(index) else {
        return Vec::new();
    };
    vec![
        vec![(ToggleRead, true), (ToggleStar, true)],
        vec![
            (
                MarkAboveRead,
                !unread_beside(articles, index, false).is_empty(),
            ),
            (
                MarkBelowRead,
                !unread_beside(articles, index, true).is_empty(),
            ),
        ],
        vec![
            (OpenOriginal, reader::opens(&a.url)),
            (CopyLink, !a.url.is_empty()),
        ],
        vec![(GoToFeed, *scope != Scope::Feed(a.feed_id))],
    ]
}

impl Ui {
    /// Open an article's context menu on right click, and with the Menu key or Shift+F10.
    pub(super) fn connect_article_menu(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        context_menu::connect(&self.list.list, move |row, at| {
            if let Some(u) = weak.upgrade() {
                u.open_article_menu(row, at);
            }
        });
    }

    /// Show the context menu of an article row, at `at` in the list's coordinates or, without
    /// it, below the row. The article is not opened.
    pub(super) fn open_article_menu(
        self: &Rc<Self>,
        row: &gtk::ListBoxRow,
        at: Option<(f64, f64)>,
    ) {
        let Ok(index) = usize::try_from(row.index()) else {
            return;
        };
        let view = self.view.borrow();
        let Some(article) = view.articles().get(index) else {
            return;
        };
        let id = article.id;
        let sections = article_menu(view.articles(), index, &view.query.scope)
            .into_iter()
            .map(|section| {
                section
                    .into_iter()
                    .map(|(command, enabled)| {
                        let weak = Rc::downgrade(self);
                        context_menu::Item {
                            label: command.label(article).into(),
                            name: command.name(),
                            enabled,
                            run: Box::new(move || {
                                if let Some(u) = weak.upgrade() {
                                    u.run_article_command(command, id);
                                }
                            }),
                        }
                    })
                    .collect()
            })
            .collect();
        drop(view);
        context_menu::show(
            &self.list.menu,
            "article",
            sections,
            self.list.root.upcast_ref(),
            &self.list.list,
            row,
            at,
        );
    }

    /// Run a command of an article's context menu, if the article is still listed.
    fn run_article_command(self: &Rc<Self>, command: ArticleCommand, id: ArticleId) {
        let (index, article) = {
            let view = self.view.borrow();
            let Some(index) = view.articles().iter().position(|a| a.id == id) else {
                return;
            };
            (index, view.articles()[index].clone())
        };
        match command {
            ArticleCommand::ToggleRead => self.set_state(id, StatePatch::read(!article.read)),
            ArticleCommand::ToggleStar => {
                self.set_state(id, StatePatch::starred(!article.starred));
            }
            ArticleCommand::MarkAboveRead | ArticleCommand::MarkBelowRead => {
                let below = command == ArticleCommand::MarkBelowRead;
                let ids = unread_beside(self.view.borrow().articles(), index, below);
                self.mark_articles_read(ids);
            }
            ArticleCommand::OpenOriginal => reader::open_uri(&article.url),
            ArticleCommand::CopyLink if !article.url.is_empty() => {
                self.window.clipboard().set_text(&article.url);
                self.toast("Link copied");
            }
            ArticleCommand::GoToFeed => self.go_to_feed(article.feed_id),
            ArticleCommand::CopyLink => {}
        }
    }

    /// Show `feed` in the sidebar and its articles in the list, keeping the open article.
    fn go_to_feed(self: &Rc<Self>, feed: FeedId) {
        let folder = self.library.borrow().feed(feed).and_then(|f| f.folder);
        self.expand_to(folder);
        let scope = Scope::Feed(feed);
        self.view.borrow_mut().set_scope(scope.clone());
        self.settings.borrow_mut().scope = scope;
        self.rebuild_sidebar();
        self.reload_articles();
        self.save();
    }

    pub(super) fn reload_articles(self: &Rc<Self>) {
        let request = self.view.borrow_mut().request_page();
        let u = self.clone();
        glib::spawn_future_local(async move {
            let query = request.query.clone();
            match u.db.call(move |s| s.articles(&query)).await {
                Ok(articles) if u.view.borrow().is_current_page(&request) => {
                    u.show_articles(&request, articles);
                }
                Ok(_) => {}
                Err(e) => u.error(e),
            }
        });
    }

    fn show_articles(self: &Rc<Self>, request: &PageRequest, articles: Vec<Article>) {
        let remembered = self.settings.borrow().selected_article;
        let shown = self
            .view
            .borrow_mut()
            .receive_page(request, articles, remembered);
        self.rebuilding.set(true);
        // The menu's article may no longer be listed.
        context_menu::close(&self.list.menu, "article");
        let list = &self.list.list;
        list.remove_all();
        self.list.next.set_sensitive(shown.has_next);
        self.list.previous.set_sensitive(shown.has_previous);
        self.list
            .pages
            .set_visible(shown.has_next || shown.has_previous);
        let view = self.view.borrow();
        // Inside a single feed, repeating its name on every row is noise.
        let show_feed = !matches!(view.query.scope, Scope::Feed(_));
        let now = Local::now();
        let rows: Vec<ArticleRow> = view
            .articles()
            .iter()
            .map(|a| ArticleRow::new(a, show_feed, now))
            .collect();
        for (a, r) in view.articles().iter().zip(&rows) {
            list.append(&r.row);
            if Some(a.id) == shown.highlight {
                list.select_row(Some(&r.row));
            }
        }
        *self.article_rows.borrow_mut() = rows;
        self.list.count.set_text(&shown.count);
        let heading = view.query.scope.label(&self.library.borrow());
        drop(view);
        self.list.heading.set_text(&heading);
        self.rebuilding.set(false);
        if let Some(id) = shown.reopen {
            self.select(id);
        }
    }

    pub(super) fn select(self: &Rc<Self>, id: ArticleId) {
        let opening = self.view.borrow_mut().start_opening();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.article(id)).await {
                Ok(a) => {
                    if !u.view.borrow_mut().finish_opening(opening, a) {
                        return;
                    }
                    u.settings.borrow_mut().selected_article = Some(id);
                    u.render();
                    u.save();
                    u.show_reader(true);
                }
                Err(e) => u.error(e),
            }
        });
    }

    /// Show an article's read and/or star state in the list and, if it is open, the reader.
    fn show_state(&self, id: ArticleId, patch: StatePatch) {
        let index = self.view.borrow_mut().apply(id, patch);
        let view = self.view.borrow();
        if let Some(a) = view.open().filter(|a| a.id == id) {
            self.update_buttons(a);
        }
        if let Some(index) = index
            && let Some(row) = self.article_rows.borrow().get(index)
        {
            row.show_state(&view.articles()[index]);
        }
    }

    /// Show a read and/or star change at once, then write it. Results are applied only while
    /// no newer change of the same field is pending; an article deleted meanwhile is ignored.
    pub(super) fn set_state(self: &Rc<Self>, id: ArticleId, patch: StatePatch) {
        let writing = self.view.borrow_mut().start_write(id, patch);
        self.show_state(id, patch);
        let u = self.clone();
        glib::spawn_future_local(async move {
            let result = reading::change_one(&u.db, id, patch).await;
            let newest = u.view.borrow_mut().finish_write(&writing);
            match result {
                Ok(()) => u.refresh_counts(),
                Err(e) => {
                    u.error(e);
                    // Show what was saved, unless a newer change will settle it anyway.
                    if newest {
                        u.reconcile(id);
                    }
                }
            }
        });
    }

    /// Show the article's saved state again after a failed write, keeping any change made
    /// after this read was sent.
    fn reconcile(self: &Rc<Self>, id: ArticleId) {
        let mark = self.view.borrow().start_reconcile();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.article_if_exists(id)).await {
                Ok(Some(a)) => {
                    let saved = u.view.borrow().reconciled(mark, &a);
                    u.show_state(id, saved);
                    u.refresh_counts();
                }
                // Deleted in the meantime: nothing left to show.
                Ok(None) => {}
                Err(e) => u.error(e),
            }
        });
    }

    pub(super) fn toggle_read(self: &Rc<Self>) {
        let change = self.view.borrow().toggle_read();
        if let Some((id, patch)) = change {
            self.set_state(id, patch);
        }
    }

    pub(super) fn toggle_star(self: &Rc<Self>) {
        let change = self.view.borrow().toggle_star();
        if let Some((id, patch)) = change {
            self.set_state(id, patch);
        }
    }

    /// Mark the view shown read, search included.
    pub(super) fn mark_all(self: &Rc<Self>) {
        let q = self.view.borrow().query.clone();
        self.mark_read(q);
    }

    /// Mark every unread article in `q` read, shown at once and undone with Ctrl+Z.
    pub(super) fn mark_read(self: &Rc<Self>, q: Query) {
        let bulk = self.view.borrow_mut().start_bulk();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match reading::mark_view_read(&u.db, q).await {
                Ok(ids) => u.marked_read(bulk, ids),
                Err(e) => u.error(e),
            }
        });
    }

    /// Mark the articles `ids` read, as [`Self::mark_read`] does a view.
    fn mark_articles_read(self: &Rc<Self>, ids: Vec<ArticleId>) {
        if ids.is_empty() {
            return;
        }
        let bulk = self.view.borrow_mut().start_bulk();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match reading::change(&u.db, ids, StatePatch::read(true)).await {
                Ok(changed) => u.marked_read(bulk, changed.into_iter().map(|(id, _)| id).collect()),
                Err(e) => u.error(e),
            }
        });
    }

    /// Show articles a bulk change `bulk` marked read, and keep them for Ctrl+Z.
    fn marked_read(self: &Rc<Self>, bulk: view::Bulk, ids: Vec<ArticleId>) {
        self.toast(&format!(
            "Marked {} articles read · Ctrl+Z to undo",
            ids.len()
        ));
        let open = self.view.borrow_mut().finish_bulk(bulk, &ids, true);
        if let Some(id) = open {
            self.show_state(id, StatePatch::read(true));
        }
        self.view.borrow_mut().push_undo(ids);
        self.reload();
    }

    pub(super) fn undo(self: &Rc<Self>) {
        let (batch, bulk) = {
            let mut view = self.view.borrow_mut();
            let Some(batch) = view.take_undo() else {
                return;
            };
            (batch, view.start_bulk())
        };
        let u = self.clone();
        glib::spawn_future_local(async move {
            match reading::undo_read(&u.db, batch.articles.clone()).await {
                Ok(()) => {
                    // Whatever is open now, not what was open when undo was pressed.
                    let open = u
                        .view
                        .borrow_mut()
                        .finish_bulk(bulk, &batch.articles, false);
                    if let Some(id) = open {
                        u.show_state(id, StatePatch::read(false));
                    }
                    u.reload();
                }
                Err(e) => {
                    u.error(e);
                    // Ctrl+Z can try again, unless a newer batch took its place.
                    u.view.borrow_mut().give_back_undo(batch);
                }
            }
        });
    }

    /// Move the list selection by `delta`, optionally skipping read articles.
    pub(super) fn navigate(self: &Rc<Self>, delta: i32, unread: bool) {
        let list = &self.list.list;
        let current = list.selected_row().map(|r| r.index() as usize);
        let target = self.view.borrow().step(current, delta, unread);
        match target.and_then(|i| list.row_at_index(i as i32)) {
            Some(row) => {
                list.select_row(Some(&row));
                row.grab_focus();
            }
            None if unread => self.toast("No more unread articles on this page"),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(id: i64, read: bool, url: &str) -> Article {
        Article {
            id: ArticleId(id),
            feed_id: FeedId(1),
            url: url.into(),
            read,
            ..Default::default()
        }
    }

    fn commands(menu: &[Vec<(ArticleCommand, bool)>]) -> Vec<Vec<ArticleCommand>> {
        menu.iter()
            .map(|section| section.iter().map(|(c, _)| *c).collect())
            .collect()
    }

    fn enabled(menu: &[Vec<(ArticleCommand, bool)>], command: ArticleCommand) -> bool {
        menu.iter()
            .flatten()
            .find(|(c, _)| *c == command)
            .unwrap()
            .1
    }

    #[test]
    fn article_menus_offer_what_applies() {
        use ArticleCommand::*;
        let list = [
            article(1, false, "https://example.org/1"),
            article(2, true, ""),
            article(3, true, "feed://example.org/3"),
            article(4, false, "https://example.org/4"),
        ];
        let first = article_menu(&list, 0, &Scope::All);
        assert_eq!(
            commands(&first),
            [
                vec![ToggleRead, ToggleStar],
                vec![MarkAboveRead, MarkBelowRead],
                vec![OpenOriginal, CopyLink],
                vec![GoToFeed],
            ]
        );
        // Nothing above the first; an unread one below it.
        assert!(!enabled(&first, MarkAboveRead));
        assert!(enabled(&first, MarkBelowRead));
        assert!(enabled(&first, OpenOriginal));
        assert!(enabled(&first, GoToFeed));
        // No link to open or copy; a link the browser is not given can still be copied.
        let second = article_menu(&list, 1, &Scope::All);
        assert!(!enabled(&second, OpenOriginal));
        assert!(!enabled(&second, CopyLink));
        let third = article_menu(&list, 2, &Scope::Feed(FeedId(1)));
        assert!(!enabled(&third, OpenOriginal));
        assert!(enabled(&third, CopyLink));
        // Already showing its feed.
        assert!(!enabled(&third, GoToFeed));
        assert_eq!(unread_beside(&list, 2, false), [ArticleId(1)]);
        assert_eq!(unread_beside(&list, 2, true), [ArticleId(4)]);
        assert!(unread_beside(&list, 3, true).is_empty());
        assert_eq!(ToggleRead.label(&list[0]), "Mark as read");
        assert_eq!(ToggleRead.label(&list[1]), "Mark as unread");
        assert!(article_menu(&list, 9, &Scope::All).is_empty());
    }

    #[test]
    fn dates_get_coarser_with_age() {
        use super::format_date;
        use chrono::{Local, TimeZone};
        let now = Local.with_ymd_and_hms(2026, 9, 25, 15, 0, 0).unwrap();
        let at = |y, m, d, h| {
            Local
                .with_ymd_and_hms(y, m, d, h, 30, 0)
                .unwrap()
                .timestamp()
        };
        assert_eq!(format_date(at(2026, 9, 25, 9), now), "09:30");
        assert_eq!(format_date(at(2026, 9, 24, 23), now), "Yesterday");
        assert_eq!(format_date(at(2026, 9, 22, 12), now), "Tuesday");
        assert_eq!(format_date(at(2026, 6, 5, 12), now), "Jun 5");
        assert_eq!(format_date(at(2025, 12, 31, 12), now), "Dec 31, 2025");
    }
}
