//! Article list: paging, selection, and read/star state.
use super::{Ui, changes::Field, widgets::label};
use chrono::{DateTime, Datelike, Local};
use gtk::{glib, prelude::*};
use omafeed_core::{
    model::{Article, Scope},
    store::PAGE_SIZE,
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
        let text = a.preview.split_whitespace().collect::<Vec<_>>().join(" ");
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

impl Ui {
    pub(super) fn reload_articles(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let page = self.page.get();
        let mut q = self.query.borrow().clone();
        q.offset = page * PAGE_SIZE;
        let mark = self.changes.borrow().mark();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.articles(&q)).await {
                Ok(articles) if u.generation.get() == generation => {
                    u.show_articles(articles, page, mark);
                }
                Ok(_) => {}
                Err(e) => u.error(e),
            }
        });
    }

    /// Show a page of articles read from the database at `mark`.
    fn show_articles(self: &Rc<Self>, mut articles: Vec<Article>, page: usize, mark: u64) {
        // Changes made since the read was sent, or still being written, are newer.
        for a in &mut articles {
            self.changes
                .borrow()
                .overlay(a.id, mark, &mut a.read, &mut a.starred);
        }
        self.rebuilding.set(true);
        let list = &self.list.list;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        self.list.next.set_sensitive(articles.len() == PAGE_SIZE);
        self.list.previous.set_sensitive(page > 0);
        let selected = self
            .selected
            .borrow()
            .as_ref()
            .map(|a| a.id)
            .or(self.settings.borrow().selected_article);
        // Inside a single feed, repeating its name on every row is noise.
        let show_feed = !matches!(self.query.borrow().scope, Scope::Feed(_));
        let now = Local::now();
        let rows: Vec<ArticleRow> = articles
            .iter()
            .map(|a| ArticleRow::new(a, show_feed, now))
            .collect();
        for (a, r) in articles.iter().zip(&rows) {
            list.append(&r.row);
            if Some(a.id) == selected {
                list.select_row(Some(&r.row));
            }
        }
        *self.article_rows.borrow_mut() = rows;
        self.list.count.set_text(&match articles.len() {
            0 => "No articles".into(),
            1 if page == 0 => "1 article".into(),
            n if page == 0 && n < PAGE_SIZE => format!("{n} articles"),
            n => {
                let first = page * PAGE_SIZE + 1;
                format!("{first}–{}", first + n - 1)
            }
        });
        let heading = self.query.borrow().scope.label(&self.library.borrow());
        self.list.heading.set_text(&heading);
        let restore = self.selected.borrow().is_none()
            && selected.is_some_and(|id| articles.iter().any(|a| a.id == id));
        *self.articles.borrow_mut() = articles;
        self.rebuilding.set(false);
        if restore && let Some(id) = selected {
            self.select(id);
        }
    }

    pub(super) fn select(self: &Rc<Self>, id: i64) {
        let generation = self.selection_generation.get() + 1;
        self.selection_generation.set(generation);
        let mark = self.changes.borrow().mark();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.article(id)).await {
                Ok(_) if u.selection_generation.get() != generation => {}
                Ok(mut a) => {
                    u.changes
                        .borrow()
                        .overlay(a.id, mark, &mut a.read, &mut a.starred);
                    u.settings.borrow_mut().selected_article = Some(id);
                    *u.selected.borrow_mut() = Some(a);
                    u.pending_mark.set(Some(id));
                    u.render();
                    u.save();
                    u.show_reader(true);
                }
                Err(e) => u.error(e),
            }
        });
    }

    /// Show an article's read and/or star state in the list and, if it is open, the reader.
    fn show_state(&self, id: i64, read: Option<bool>, starred: Option<bool>) {
        if let Some(a) = self.selected.borrow_mut().as_mut().filter(|a| a.id == id) {
            a.read = read.unwrap_or(a.read);
            a.starred = starred.unwrap_or(a.starred);
            self.update_buttons(a);
        }
        let mut articles = self.articles.borrow_mut();
        let Some((index, a)) = articles.iter_mut().enumerate().find(|(_, a)| a.id == id) else {
            return;
        };
        a.read = read.unwrap_or(a.read);
        a.starred = starred.unwrap_or(a.starred);
        if let Some(row) = self.article_rows.borrow().get(index) {
            row.show_state(a);
        }
    }

    /// Show a read and/or star change at once, then write it. Results are applied only while
    /// no newer change of the same field is pending; an article deleted meanwhile is ignored.
    pub(super) fn set_state(self: &Rc<Self>, id: i64, read: Option<bool>, starred: Option<bool>) {
        self.state_writes.set(self.state_writes.get() + 1);
        let started: Vec<(Field, u64)> = {
            let mut changes = self.changes.borrow_mut();
            [(Field::Read, read), (Field::Starred, starred)]
                .into_iter()
                .filter_map(|(field, value)| Some((field, changes.start(id, field, value?))))
                .collect()
        };
        self.show_state(id, read, starred);
        let u = self.clone();
        glib::spawn_future_local(async move {
            let result =
                u.db.call(move |s| {
                    if let Some(r) = read {
                        s.set_read(id, r)?;
                    }
                    if let Some(v) = starred {
                        s.set_starred(id, v)?;
                    }
                    Ok(())
                })
                .await;
            let mut newest = false;
            for (field, sequence) in started {
                newest |= u.changes.borrow_mut().finish(id, field, sequence);
            }
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
    fn reconcile(self: &Rc<Self>, id: i64) {
        let mark = self.changes.borrow().mark();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.article_if_exists(id)).await {
                Ok(Some(mut a)) => {
                    u.changes
                        .borrow()
                        .overlay(id, mark, &mut a.read, &mut a.starred);
                    u.show_state(id, Some(a.read), Some(a.starred));
                    u.refresh_counts();
                }
                // Deleted in the meantime: nothing left to show.
                Ok(None) => {}
                Err(e) => u.error(e),
            }
        });
    }

    pub(super) fn toggle_read(self: &Rc<Self>) {
        let change = self.selected.borrow().as_ref().map(|a| (a.id, !a.read));
        if let Some((id, read)) = change {
            self.set_state(id, Some(read), None);
        }
    }

    pub(super) fn toggle_star(self: &Rc<Self>) {
        let change = self.selected.borrow().as_ref().map(|a| (a.id, !a.starred));
        if let Some((id, starred)) = change {
            self.set_state(id, None, Some(starred));
        }
    }

    /// Record that bulk change `sequence` set `articles` to `read`, except those changed
    /// since it was sent, and show it on the open article if it is one of them.
    fn bulk_read_done(&self, articles: &[i64], read: bool, sequence: u64) {
        let open = self.selected.borrow().as_ref().map(|a| a.id);
        let mut show = false;
        let mut changes = self.changes.borrow_mut();
        for &id in articles {
            let applied = changes.finish_bulk(id, Field::Read, read, sequence);
            show |= applied && Some(id) == open;
        }
        drop(changes);
        if let Some(id) = open.filter(|_| show) {
            self.show_state(id, Some(read), None);
        }
    }

    pub(super) fn mark_all(self: &Rc<Self>) {
        self.state_writes.set(self.state_writes.get() + 1);
        let q = self.query.borrow().clone();
        let sequence = self.changes.borrow_mut().start_bulk();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.mark_read(&q)).await {
                Ok(ids) => {
                    u.toast(&format!(
                        "Marked {} articles read · Ctrl+Z to undo",
                        ids.len()
                    ));
                    u.bulk_read_done(&ids, true, sequence);
                    u.undo.borrow_mut().push(ids);
                    u.reload();
                }
                Err(e) => u.error(e),
            }
        });
    }

    pub(super) fn undo(self: &Rc<Self>) {
        let Some(batch) = self.undo.borrow_mut().take() else {
            return;
        };
        self.state_writes.set(self.state_writes.get() + 1);
        let sequence = self.changes.borrow_mut().start_bulk();
        let u = self.clone();
        glib::spawn_future_local(async move {
            let ids = batch.articles.clone();
            match u.db.call(move |s| s.undo_read(&ids)).await {
                Ok(()) => {
                    // Whatever is open now, not what was open when undo was pressed.
                    u.bulk_read_done(&batch.articles, false, sequence);
                    u.reload();
                }
                Err(e) => {
                    u.error(e);
                    // Ctrl+Z can try again, unless a newer batch took its place.
                    u.undo.borrow_mut().give_back(batch);
                }
            }
        });
    }

    /// Move the list selection by `delta`, optionally skipping read articles.
    pub(super) fn navigate(self: &Rc<Self>, delta: i32, unread: bool) {
        let list = &self.list.list;
        let current = list
            .selected_row()
            .map_or(if delta > 0 { -1 } else { 0 }, |r| r.index());
        let target = {
            let articles = self.articles.borrow();
            std::iter::successors(Some(current + delta), |i| Some(i + delta))
                .take_while(|i| *i >= 0 && (*i as usize) < articles.len())
                .find(|i| !unread || !articles[*i as usize].read)
        };
        match target.and_then(|i| list.row_at_index(i)) {
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
