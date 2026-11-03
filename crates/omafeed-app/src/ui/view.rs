//! What the window shows, apart from its widgets: the view and page, the articles on it, the
//! open article, collapsed folders, and the read/star changes and undo in flight. No GTK here,
//! so every rule about what a late database result may change is tested on its own.
//!
//! Each kind of request has its own guard. A list page, an article being opened, and a
//! reload after another program's change can each arrive after something newer happened,
//! and each is checked against the counter for its kind only.
use super::changes::{Batch, Changes, Field, Undo};
use omafeed_core::{
    model::{Article, ArticleId, FolderId, Query, Scope, StatePatch},
    store::PAGE_SIZE,
};
use std::collections::HashSet;

/// A page of the article list to ask the database for.
#[derive(Clone, Debug)]
pub(super) struct PageRequest {
    pub query: Query,
    pub page: usize,
    generation: u64,
    mark: u64,
}

/// What to show once a page has arrived.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PageShown {
    /// The article to highlight: the open one, or the one remembered from the last session.
    pub highlight: Option<ArticleId>,
    /// An article to open because nothing is open and it is on this page.
    pub reopen: Option<ArticleId>,
    pub count: String,
    pub has_next: bool,
    pub has_previous: bool,
}

/// An article being opened.
#[derive(Clone, Copy, Debug)]
pub(super) struct Opening {
    generation: u64,
    mark: u64,
}

/// A read/star change shown at once and being written.
#[derive(Debug)]
pub(super) struct Writing {
    id: ArticleId,
    started: Vec<(Field, u64)>,
}

/// A bulk read change (mark all read, or its undo) being written.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bulk(u64);

#[derive(Debug, Default)]
pub(super) struct View {
    pub query: Query,
    page: usize,
    articles: Vec<Article>,
    open: Option<Article>,
    collapsed: HashSet<FolderId>,
    undo: Undo,
    changes: Changes,
    /// Bumped per list request, so only the newest page is shown.
    list_generation: u64,
    /// Bumped per article opened or closed, so a slow load cannot replace a newer choice.
    selection_generation: u64,
    /// Counts this window's own read/star changes, so a reload that raced one keeps it.
    state_writes: u64,
    /// Article to mark read once the reader has finished loading it.
    pending_mark: Option<ArticleId>,
}

/// The list's count line for `shown` articles on `page`.
pub(super) fn count_label(shown: usize, page: usize) -> String {
    match shown {
        0 => "No articles".into(),
        1 if page == 0 => "1 article".into(),
        n if page == 0 && n < PAGE_SIZE => format!("{n} articles"),
        n => {
            let first = page * PAGE_SIZE + 1;
            format!("{first}–{}", first + n - 1)
        }
    }
}

impl View {
    pub fn new(query: Query) -> Self {
        Self {
            query,
            ..Self::default()
        }
    }

    pub fn articles(&self) -> &[Article] {
        &self.articles
    }

    pub fn open(&self) -> Option<&Article> {
        self.open.as_ref()
    }

    pub fn open_id(&self) -> Option<ArticleId> {
        self.open.as_ref().map(|a| a.id)
    }

    // Views and pages.

    /// Show `scope` from its first page.
    pub fn set_scope(&mut self, scope: Scope) {
        self.query.scope = scope;
        self.page = 0;
    }

    pub fn set_search(&mut self, search: String) {
        self.query.search = search;
        self.page = 0;
    }

    pub fn set_unread_only(&mut self, unread_only: bool) {
        self.query.unread_only = unread_only;
        self.page = 0;
    }

    pub fn next_page(&mut self) {
        self.page += 1;
    }

    /// Go back a page; false on the first page.
    pub fn previous_page(&mut self) -> bool {
        if self.page == 0 {
            return false;
        }
        self.page -= 1;
        true
    }

    /// Ask for the current page; any page asked for before it will be ignored.
    pub fn request_page(&mut self) -> PageRequest {
        self.list_generation += 1;
        let mut query = self.query.clone();
        query.offset = self.page * PAGE_SIZE;
        PageRequest {
            query,
            page: self.page,
            generation: self.list_generation,
            mark: self.changes.mark(),
        }
    }

    pub fn is_current_page(&self, request: &PageRequest) -> bool {
        request.generation == self.list_generation
    }

    /// Take a page read for `request`. `remembered` is the article open when the window was
    /// last closed.
    pub fn receive_page(
        &mut self,
        request: &PageRequest,
        mut articles: Vec<Article>,
        remembered: Option<ArticleId>,
    ) -> PageShown {
        // Changes made since the read was sent, or still being written, are newer.
        for a in &mut articles {
            self.changes
                .overlay(a.id, request.mark, &mut a.read, &mut a.starred);
        }
        let highlight = self.open_id().or(remembered);
        let reopen =
            highlight.filter(|id| self.open.is_none() && articles.iter().any(|a| a.id == *id));
        let shown = PageShown {
            highlight,
            reopen,
            count: count_label(articles.len(), request.page),
            has_next: articles.len() == PAGE_SIZE,
            has_previous: request.page > 0,
        };
        self.articles = articles;
        shown
    }

    /// The list position `delta` rows from `current` (none selected: before the first row
    /// going down, the first row going up), skipping read articles with `unread_only`.
    pub fn step(&self, current: Option<usize>, delta: i32, unread_only: bool) -> Option<usize> {
        let start = current.map_or(if delta > 0 { -1 } else { 0 }, |i| i as i64);
        std::iter::successors(Some(start + i64::from(delta)), |i| {
            Some(i + i64::from(delta))
        })
        .take_while(|i| *i >= 0 && (*i as usize) < self.articles.len())
        .map(|i| i as usize)
        .find(|i| !unread_only || !self.articles[*i].read)
    }

    // The open article.

    /// Start opening an article; anything opened or closed before is superseded.
    pub fn start_opening(&mut self) -> Opening {
        self.selection_generation += 1;
        Opening {
            generation: self.selection_generation,
            mark: self.changes.mark(),
        }
    }

    /// Show `article`, read for `opening`, unless something else was opened or closed since.
    /// Returns whether it is now open; it is then marked read once the reader has loaded it.
    pub fn finish_opening(&mut self, opening: Opening, mut article: Article) -> bool {
        if opening.generation != self.selection_generation {
            return false;
        }
        self.changes.overlay(
            article.id,
            opening.mark,
            &mut article.read,
            &mut article.starred,
        );
        self.pending_mark = Some(article.id);
        self.open = Some(article);
        true
    }

    /// Close the open article, and drop any article still being opened.
    pub fn close(&mut self) {
        self.selection_generation += 1;
        self.pending_mark = None;
        self.open = None;
    }

    /// The article to mark read now that the reader has loaded: the one just opened, if it is
    /// still open and unread. A re-render of the same article does not count.
    pub fn take_pending_mark(&mut self) -> Option<ArticleId> {
        let loaded = self.pending_mark.take()?;
        self.open
            .as_ref()
            .filter(|a| a.id == loaded && !a.read)
            .map(|a| a.id)
    }

    // Reloads after another program changed the library.

    /// What a reload of the open article needs to know, taken when it is sent.
    pub fn start_outside_reload(&self) -> OutsideReload {
        OutsideReload {
            open: self.open_id(),
            generation: self.selection_generation,
            writes: self.state_writes,
            mark: self.changes.mark(),
        }
    }

    /// Take the open article as another program left it. `None` means it is gone. Returns
    /// what to do, if the reload still concerns the open article.
    pub fn finish_outside_reload(
        &mut self,
        reload: &OutsideReload,
        article: Option<Article>,
    ) -> Option<Reloaded> {
        reload.open?;
        if reload.generation != self.selection_generation {
            return None;
        }
        let Some(mut a) = article else {
            self.close();
            return Some(Reloaded::Gone);
        };
        // A star or read change made here since the read is newer.
        if self.state_writes != reload.writes
            && let Some(old) = &self.open
        {
            a.read = old.read;
            a.starred = old.starred;
        }
        self.changes
            .overlay(a.id, reload.mark, &mut a.read, &mut a.starred);
        // State-only updates keep the reader's scroll position.
        let content_changed = self.open.as_ref().is_some_and(|old| {
            old.html != a.html
                || old.title != a.title
                || old.author != a.author
                || old.url != a.url
                || old.feed_title != a.feed_title
                || old.published != a.published
        });
        self.open = Some(a);
        Some(Reloaded::Updated { content_changed })
    }

    // Read and star changes.

    /// Show `patch` on article `id` at once, wherever it is shown. Returns its list position,
    /// so its row can be redrawn.
    pub fn apply(&mut self, id: ArticleId, patch: StatePatch) -> Option<usize> {
        if let Some(a) = self.open.as_mut().filter(|a| a.id == id) {
            patch.apply(a);
        }
        let index = self.articles.iter().position(|a| a.id == id)?;
        patch.apply(&mut self.articles[index]);
        Some(index)
    }

    /// Start writing `patch` to article `id`; show it with [`apply`](Self::apply).
    pub fn start_write(&mut self, id: ArticleId, patch: StatePatch) -> Writing {
        self.state_writes += 1;
        let started = [(Field::Read, patch.read), (Field::Starred, patch.starred)]
            .into_iter()
            .filter_map(|(field, value)| Some((field, self.changes.start(id, field, value?))))
            .collect();
        Writing { id, started }
    }

    /// The write finished, successfully or not. Returns whether any of its fields is still
    /// the newest change, so that a failure must show the saved state again.
    pub fn finish_write(&mut self, writing: &Writing) -> bool {
        let mut newest = false;
        for &(field, sequence) in &writing.started {
            newest |= self.changes.finish(writing.id, field, sequence);
        }
        newest
    }

    /// A point to read the saved state from after a failed write.
    pub fn start_reconcile(&self) -> u64 {
        self.changes.mark()
    }

    /// The state to show for `article` as saved, read at `mark`: fields changed since keep
    /// the window's value.
    pub fn reconciled(&self, mark: u64, article: &Article) -> StatePatch {
        let (mut read, mut starred) = (article.read, article.starred);
        self.changes
            .overlay(article.id, mark, &mut read, &mut starred);
        StatePatch {
            read: Some(read),
            starred: Some(starred),
        }
    }

    /// The change that toggles the open article's read state.
    pub fn toggle_read(&self) -> Option<(ArticleId, StatePatch)> {
        self.open
            .as_ref()
            .map(|a| (a.id, StatePatch::read(!a.read)))
    }

    /// The change that toggles the open article's star.
    pub fn toggle_star(&self) -> Option<(ArticleId, StatePatch)> {
        self.open
            .as_ref()
            .map(|a| (a.id, StatePatch::starred(!a.starred)))
    }

    /// Start a bulk read change.
    pub fn start_bulk(&mut self) -> Bulk {
        self.state_writes += 1;
        Bulk(self.changes.start_bulk())
    }

    /// The bulk change set `articles` to `read`. Records it for those not changed since it
    /// was sent and returns the open article if it is one of them, to show the change on.
    pub fn finish_bulk(
        &mut self,
        bulk: Bulk,
        articles: &[ArticleId],
        read: bool,
    ) -> Option<ArticleId> {
        let open = self.open_id();
        let mut show = false;
        for &id in articles {
            let applied = self.changes.finish_bulk(id, Field::Read, read, bulk.0);
            show |= applied && Some(id) == open;
        }
        open.filter(|_| show)
    }

    /// Remember a "mark all read" for undo.
    pub fn push_undo(&mut self, articles: Vec<ArticleId>) {
        self.undo.push(articles);
    }

    /// The batch to undo, if any; its undo is then a bulk change of its own.
    pub fn take_undo(&mut self) -> Option<Batch> {
        self.undo.take()
    }

    /// Put back a batch whose undo failed, unless a newer one replaced it.
    pub fn give_back_undo(&mut self, batch: Batch) {
        self.undo.give_back(batch);
    }

    /// The value an unfinished change of `field` is writing, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn unfinished(&self, id: ArticleId, field: Field) -> Option<bool> {
        self.changes.unfinished(id, field)
    }

    // Folders.

    pub fn collapsed(&self) -> &HashSet<FolderId> {
        &self.collapsed
    }

    pub fn toggle_folder(&mut self, id: FolderId) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
    }
}

/// A reload of the open article after another program's change, as sent.
#[derive(Clone, Copy, Debug)]
pub(super) struct OutsideReload {
    pub open: Option<ArticleId>,
    generation: u64,
    writes: u64,
    mark: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reloaded {
    /// The open article is gone; it was closed.
    Gone,
    /// The open article was updated; re-render it if its content changed.
    Updated { content_changed: bool },
}

#[cfg(test)]
mod tests {
    use super::*;
    use omafeed_core::model::FeedId;

    fn article(id: i64, read: bool) -> Article {
        Article {
            id: ArticleId(id),
            feed_id: FeedId(1),
            title: format!("Article {id}"),
            read,
            ..Default::default()
        }
    }

    fn page(view: &mut View, articles: Vec<Article>) -> PageShown {
        let request = view.request_page();
        view.receive_page(&request, articles, None)
    }

    fn opened(view: &mut View, a: Article) {
        let opening = view.start_opening();
        assert!(view.finish_opening(opening, a));
    }

    #[test]
    fn only_the_newest_page_is_shown_and_views_start_on_their_first_page() {
        let mut v = View::default();
        let old = v.request_page();
        v.next_page();
        let new = v.request_page();
        assert!(!v.is_current_page(&old));
        assert!(v.is_current_page(&new));
        assert_eq!(new.query.offset, PAGE_SIZE);
        v.set_search("rust".into());
        assert_eq!(v.request_page().query.offset, 0);
        assert!(!v.previous_page());
        v.next_page();
        v.set_scope(Scope::Starred);
        assert_eq!(v.request_page().page, 0);
        v.next_page();
        v.set_unread_only(true);
        assert!(!v.previous_page());
    }

    #[test]
    fn pages_count_their_articles_and_reopen_the_remembered_one() {
        let mut v = View::default();
        let request = v.request_page();
        let shown = v.receive_page(
            &request,
            vec![article(1, false), article(2, true)],
            Some(ArticleId(2)),
        );
        assert_eq!(shown.count, "2 articles");
        assert_eq!(shown.highlight, Some(ArticleId(2)));
        assert_eq!(shown.reopen, Some(ArticleId(2)));
        assert!(!shown.has_next && !shown.has_previous);
        // With an article open, it is highlighted but not opened again.
        opened(&mut v, article(1, false));
        let shown = page(&mut v, vec![article(1, false)]);
        assert_eq!((shown.highlight, shown.reopen), (Some(ArticleId(1)), None));
        assert_eq!(count_label(1, 0), "1 article");
        assert_eq!(count_label(0, 3), "No articles");
        assert_eq!(count_label(PAGE_SIZE, 0), format!("1–{PAGE_SIZE}"));
        assert_eq!(
            count_label(5, 2),
            format!("{}–{}", 2 * PAGE_SIZE + 1, 2 * PAGE_SIZE + 5)
        );
    }

    #[test]
    fn a_slow_article_load_cannot_replace_a_newer_choice_or_a_closed_reader() {
        let mut v = View::default();
        let first = v.start_opening();
        let second = v.start_opening();
        assert!(!v.finish_opening(first, article(1, false)));
        assert!(v.finish_opening(second, article(2, false)));
        let late = v.start_opening();
        v.close();
        assert!(!v.finish_opening(late, article(3, false)));
        assert!(v.open().is_none());
    }

    #[test]
    fn opening_marks_read_once_and_only_while_unread_and_open() {
        let mut v = View::default();
        opened(&mut v, article(1, false));
        assert_eq!(v.take_pending_mark(), Some(ArticleId(1)));
        assert_eq!(
            v.take_pending_mark(),
            None,
            "a re-render does not mark again"
        );
        opened(&mut v, article(2, true));
        assert_eq!(v.take_pending_mark(), None);
        opened(&mut v, article(3, false));
        v.close();
        assert_eq!(v.take_pending_mark(), None);
    }

    #[test]
    fn quick_toggles_flip_what_the_last_one_showed() {
        let mut v = View::default();
        page(&mut v, vec![article(1, false)]);
        opened(&mut v, article(1, false));
        let (id, star) = v.toggle_star().unwrap();
        let first = v.start_write(id, star);
        assert_eq!(v.apply(id, star), Some(0));
        let (_, unstar) = v.toggle_star().unwrap();
        assert_eq!(unstar, StatePatch::starred(false));
        let second = v.start_write(id, unstar);
        v.apply(id, unstar);
        assert!(!v.finish_write(&first), "superseded by the second");
        assert!(v.finish_write(&second));
        assert!(!v.open().unwrap().starred);
        assert!(!v.articles()[0].starred);
    }

    #[test]
    fn a_failed_write_shows_the_saved_state_but_keeps_newer_changes() {
        let mut v = View::default();
        opened(&mut v, article(1, false));
        let mark = v.start_reconcile();
        let read = v.start_write(ArticleId(1), StatePatch::read(true));
        v.apply(ArticleId(1), StatePatch::read(true));
        let saved = article(1, false);
        assert_eq!(
            v.reconciled(mark, &saved).read,
            Some(true),
            "the newer change wins"
        );
        v.finish_write(&read);
        assert_eq!(v.reconciled(v.start_reconcile(), &saved).read, Some(false));
    }

    #[test]
    fn pages_and_reloads_read_before_a_change_keep_it() {
        let mut v = View::default();
        let request = v.request_page();
        let early = v.start_outside_reload();
        opened(&mut v, article(1, false));
        let reload = v.start_outside_reload();
        let star = v.start_write(ArticleId(1), StatePatch::starred(true));
        v.apply(ArticleId(1), StatePatch::starred(true));
        v.finish_write(&star);
        let shown = v.receive_page(&request, vec![article(1, false)], None);
        assert_eq!(shown.highlight, Some(ArticleId(1)));
        assert!(v.articles()[0].starred, "the page was read before the star");
        // A reload sent with nothing open does not concern this article.
        assert_eq!(
            v.finish_outside_reload(&early, Some(article(1, false))),
            None
        );
        // One sent before this window's own change keeps the window's state.
        let mut renamed = article(1, true);
        renamed.title = "Renamed".into();
        assert_eq!(
            v.finish_outside_reload(&reload, Some(renamed)),
            Some(Reloaded::Updated {
                content_changed: true
            })
        );
        let open = v.open().unwrap();
        assert!(!open.read && open.starred);
        // One sent afterwards takes what the other program saved.
        let reload = v.start_outside_reload();
        let mut saved = article(1, true);
        saved.title = "Renamed".into();
        saved.starred = true;
        assert_eq!(
            v.finish_outside_reload(&reload, Some(saved)),
            Some(Reloaded::Updated {
                content_changed: false
            })
        );
        assert!(v.open().unwrap().read);
        let reload = v.start_outside_reload();
        assert_eq!(v.finish_outside_reload(&reload, None), Some(Reloaded::Gone));
        assert!(v.open().is_none());
    }

    #[test]
    fn bulk_changes_update_the_open_article_unless_it_changed_since() {
        let mut v = View::default();
        opened(&mut v, article(1, false));
        let bulk = v.start_bulk();
        assert_eq!(
            v.finish_bulk(bulk, &[ArticleId(1), ArticleId(2)], true),
            Some(ArticleId(1))
        );
        v.push_undo(vec![ArticleId(1), ArticleId(2)]);
        let batch = v.take_undo().unwrap();
        let undo = v.start_bulk();
        let toggle = v.start_write(ArticleId(1), StatePatch::read(true));
        assert_eq!(
            v.finish_bulk(undo, &batch.articles, false),
            None,
            "the toggle is newer"
        );
        v.finish_write(&toggle);
        v.give_back_undo(batch.clone());
        assert_eq!(v.take_undo(), Some(batch));
    }

    #[test]
    fn navigation_steps_within_the_page_and_can_skip_read_articles() {
        let mut v = View::default();
        page(
            &mut v,
            vec![
                article(1, true),
                article(2, false),
                article(3, true),
                article(4, false),
            ],
        );
        assert_eq!(v.step(None, 1, false), Some(0));
        assert_eq!(v.step(None, -1, false), None);
        assert_eq!(v.step(Some(0), 1, true), Some(1));
        assert_eq!(v.step(Some(1), 1, true), Some(3));
        assert_eq!(v.step(Some(3), 1, false), None);
        assert_eq!(v.step(Some(3), -1, false), Some(2));
    }

    #[test]
    fn folders_collapse_and_expand() {
        let mut v = View::default();
        v.toggle_folder(FolderId(4));
        assert!(v.collapsed().contains(&FolderId(4)));
        v.toggle_folder(FolderId(4));
        assert!(v.collapsed().is_empty());
    }
}
