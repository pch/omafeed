//! Article list: paging, selection, and read/star state.
use super::{Ui, widgets::label};
use gtk::{glib, prelude::*};
use omafeed_core::db::{Article, PAGE_SIZE};
use std::rc::Rc;

fn title_text(a: &Article) -> String {
    format!(
        "{}{}{}",
        if a.read { "" } else { "● " },
        if a.starred { "★ " } else { "" },
        a.title
    )
}

fn style_title(title: &gtk::Label, a: &Article) {
    title.set_text(&title_text(a));
    if a.read {
        title.add_css_class("article-read");
    } else {
        title.remove_css_class("article-read");
    }
}

fn article_row(a: &Article) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let title = label("", "article-title");
    style_title(&title, a);
    title.set_wrap(true);
    title.set_lines(3);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&title);
    let date = chrono::DateTime::from_timestamp(a.published, 0)
        .map(|d| d.format("%b %-d").to_string())
        .unwrap_or_default();
    let meta = label(&format!("{} · {date}", a.feed_title), "dim-label");
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&meta);
    let preview = label(&a.preview.replace('\n', " "), "preview");
    preview.set_wrap(true);
    preview.set_lines(2);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&preview);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&content));
    row
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
        for a in &articles {
            let row = article_row(a);
            list.append(&row);
            if Some(a.id) == selected {
                list.select_row(Some(&row));
            }
        }
        self.list.count.set_text(&if articles.is_empty() {
            "No articles".into()
        } else {
            let first = page * PAGE_SIZE + 1;
            format!("{first}–{}", first + articles.len() - 1)
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
        if let Some(row) = self.list.list.row_at_index(index as i32)
            && let Some(content) = row.child().and_downcast::<gtk::Box>()
            && let Some(title) = content.first_child().and_downcast::<gtk::Label>()
        {
            style_title(&title, a);
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
