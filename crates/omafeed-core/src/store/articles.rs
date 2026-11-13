//! Articles: listing, reading, and read/star state.
use super::{PAGE_SIZE, Store};
use crate::model::{Article, ArticleId, Query, StatePatch};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

/// Joins shared by every article query; aliases `a`, `f`, and `s` are used by predicates.
const ARTICLE_JOINS: &str = "
FROM articles a
JOIN feeds f ON f.id = a.feed_id
JOIN article_state s ON s.article_id = a.id";

impl Store {
    fn article_columns(html: bool) -> String {
        format!(
            "a.id, a.feed_id, f.title, a.title, a.url, a.author, a.published, {}, substr(a.text, 1, 220), s.read, s.starred, a.full_html IS NOT NULL",
            if html {
                "COALESCE(a.full_html, a.html)"
            } else {
                "''"
            }
        )
    }

    /// A page of up to [`PAGE_SIZE`] articles matching `q`, newest first, without their HTML.
    pub fn articles(&self, q: &Query) -> Result<Vec<Article>> {
        let (predicate, mut values) = self.predicate(q)?;
        values.push((q.offset as i64).into());
        let sql = format!(
            "SELECT {} {ARTICLE_JOINS} WHERE {predicate}
             ORDER BY a.published DESC, a.id DESC LIMIT {PAGE_SIZE} OFFSET ?",
            Self::article_columns(false)
        );
        Ok(self
            .conn
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(values), Self::article_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn article_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Article> {
        Ok(Article {
            id: r.get(0)?,
            feed_id: r.get(1)?,
            feed_title: r.get(2)?,
            title: r.get(3)?,
            url: r.get(4)?,
            author: r.get(5)?,
            published: r.get(6)?,
            html: r.get(7)?,
            preview: r.get(8)?,
            read: r.get(9)?,
            starred: r.get(10)?,
            full: r.get(11)?,
        })
    }

    /// Where to fetch the article `id` in full from: its web page, its feed's address (which
    /// decides whether the page may be on the local network), its entry on the Miniflux
    /// server, if it is one, and its identity in its feed, for [`Store::keep_full_article`].
    pub(crate) fn full_article_source(&self, id: ArticleId) -> Result<FullArticleSource> {
        self.conn
            .query_row(
                "SELECT a.url, f.url, a.remote_id, a.feed_id, a.identity
                 FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE a.id = ?1",
                [id],
                |r| {
                    Ok(FullArticleSource {
                        url: r.get(0)?,
                        feed_url: r.get(1)?,
                        entry: r.get(2)?,
                        feed: r.get(3)?,
                        identity: r.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| crate::Error::ArticleNotFound(id).into())
    }

    /// Keep `html` (sanitized) as the full article `id`, if it is still the article `source`
    /// was read from: one deleted meanwhile is left alone, and so is a new article that was
    /// given its ID since.
    pub(crate) fn keep_full_article(
        &self,
        id: ArticleId,
        source: &FullArticleSource,
        html: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE articles SET full_html = ?1
             WHERE id = ?2 AND feed_id = ?3 AND identity = ?4 AND url = ?5",
            params![html, id, source.feed, source.identity, source.url],
        )?;
        Ok(())
    }

    /// Go back to the feed's version of the article `id`.
    pub(crate) fn forget_full_article(&self, id: ArticleId) -> Result<()> {
        self.conn
            .execute("UPDATE articles SET full_html = NULL WHERE id = ?1", [id])?;
        Ok(())
    }

    /// One article with its HTML; an error if it does not exist.
    pub fn article(&self, id: ArticleId) -> Result<Article> {
        let sql = format!(
            "SELECT {} {ARTICLE_JOINS} WHERE a.id = ?1",
            Self::article_columns(true)
        );
        Ok(self.conn.query_row(&sql, [id], Self::article_row)?)
    }

    /// Look up an article without treating deletion as a database error.
    pub fn article_if_exists(&self, id: ArticleId) -> Result<Option<Article>> {
        let sql = format!(
            "SELECT {} {ARTICLE_JOINS} WHERE a.id = ?1",
            Self::article_columns(true)
        );
        Ok(self
            .conn
            .query_row(&sql, [id], Self::article_row)
            .optional()?)
    }

    /// Change every article in `ids` as `patch` says, or none of them if one does not exist
    /// or a write fails. Returns each article changed with its title, in ID order.
    pub fn set_article_states(
        &self,
        ids: &[ArticleId],
        patch: StatePatch,
    ) -> Result<Vec<(ArticleId, String)>> {
        self.immediate(|s| s.set_article_states_now(ids, patch))
    }

    fn set_article_states_now(
        &self,
        ids: &[ArticleId],
        patch: StatePatch,
    ) -> Result<Vec<(ArticleId, String)>> {
        let tx = &self.conn;
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let mut changed = Vec::new();
        for id in ids {
            let title: Option<String> = tx
                .query_row("SELECT title FROM articles WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(title) = title else {
                return Err(crate::Error::ArticleNotFound(id).into());
            };
            changed.push((id, title));
        }
        for (id, _) in &changed {
            tx.execute(
                "UPDATE article_state SET read = COALESCE(?1, read), starred = COALESCE(?2, starred) WHERE article_id = ?3",
                params![patch.read, patch.starred, id],
            )?;
        }
        Ok(changed)
    }

    /// Change one article as `patch` says. An article that no longer exists is left alone.
    pub fn set_state(&self, id: ArticleId, patch: StatePatch) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read = COALESCE(?1, read), starred = COALESCE(?2, starred) WHERE article_id = ?3",
            params![patch.read, patch.starred, id],
        )?;
        Ok(())
    }

    /// Mark one article read or unread; an article that does not exist is left alone.
    pub fn set_read(&self, id: ArticleId, read: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read = ?1 WHERE article_id = ?2",
            params![read, id],
        )?;
        Ok(())
    }

    /// Star or unstar one article; an article that does not exist is left alone.
    pub fn set_starred(&self, id: ArticleId, value: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET starred = ?1 WHERE article_id = ?2",
            params![value, id],
        )?;
        Ok(())
    }

    /// Mark every unread article matching `q` read, returning the changed IDs for undo.
    pub fn mark_read(&self, q: &Query) -> Result<Vec<ArticleId>> {
        let (predicate, values) = self.predicate(q)?;
        let sql = format!(
            "UPDATE article_state SET read = 1
             WHERE read = 0 AND article_id IN (SELECT a.id {ARTICLE_JOINS} WHERE {predicate})
             RETURNING article_id"
        );
        Ok(self
            .conn
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(values), |r| r.get(0))?
            .collect::<Result<Vec<ArticleId>, _>>()?)
    }

    /// Mark unread again the articles a [`Store::mark_read`] returned.
    pub fn undo_read(&self, ids: &[ArticleId]) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read = 0 WHERE article_id IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(ids)?],
        )?;
        Ok(())
    }
}

/// Where an article's full text comes from; see [`Store::full_article_source`].
pub(crate) struct FullArticleSource {
    pub url: String,
    pub feed_url: String,
    pub entry: Option<i64>,
    feed: i64,
    identity: String,
}
