//! Article list: paging, selection, and read/star state.
use super::{Ui, widgets::label};
use chrono::{DateTime, Datelike, Local};
use gtk::{glib, prelude::*};
use omafeed_core::db::{Article, PAGE_SIZE, Scope};
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
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.articles(&q)).await {
                Ok(articles) if u.generation.get() == generation => u.show_articles(articles, page),
                Ok(_) => {}
                Err(e) => u.error(e),
            }
        });
    }

    fn show_articles(self: &Rc<Self>, articles: Vec<Article>, page: usize) {
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
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.article(id)).await {
                Ok(_) if u.selection_generation.get() != generation => {}
                Ok(a) => {
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

    /// Update an article's title row after its state changed.
    fn update_local_state(&self, id: i64, read: Option<bool>, starred: Option<bool>) {
        let mut articles = self.articles.borrow_mut();
        let Some((index, a)) = articles.iter_mut().enumerate().find(|(_, a)| a.id == id) else {
            return;
        };
        if let Some(r) = read {
            a.read = r;
        }
        if let Some(s) = starred {
            a.starred = s;
        }
        if let Some(row) = self.article_rows.borrow().get(index) {
            row.show_state(a);
        }
    }

    /// Persist a read and/or star change, then update every view of the article.
    pub(super) fn set_state(self: &Rc<Self>, id: i64, read: Option<bool>, starred: Option<bool>) {
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
            if let Err(e) = result {
                u.error(e);
                return;
            }
            if let Some(a) = u.selected.borrow_mut().as_mut().filter(|a| a.id == id) {
                a.read = read.unwrap_or(a.read);
                a.starred = starred.unwrap_or(a.starred);
                u.update_buttons(a);
            }
            u.update_local_state(id, read, starred);
            u.refresh_counts();
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

    pub(super) fn mark_all(self: &Rc<Self>) {
        let q = self.query.borrow().clone();
        let u = self.clone();
        glib::spawn_future_local(async move {
            match u.db.call(move |s| s.mark_read(&q)).await {
                Ok(ids) => {
                    u.toast(&format!(
                        "Marked {} articles read · Ctrl+Z to undo",
                        ids.len()
                    ));
                    if let Some(a) = u.selected.borrow_mut().as_mut()
                        && ids.contains(&a.id)
                    {
                        a.read = true;
                        u.update_buttons(a);
                    }
                    *u.undo.borrow_mut() = ids;
                    u.reload();
                }
                Err(e) => u.error(e),
            }
        });
    }

    pub(super) fn undo(self: &Rc<Self>) {
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

    /// Move the list selection by `delta`, optionally skipping read articles.
    pub(super) fn navigate(self: &Rc<Self>, delta: i32, unread: bool) {
        let list = &self.list.list;
        let current = list
            .selected_row()
            .map(|r| r.index())
            .unwrap_or(if delta > 0 { -1 } else { 0 });
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
    use super::format_date;
    use chrono::{Local, TimeZone};

    #[test]
    fn dates_get_coarser_with_age() {
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
