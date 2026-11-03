//! Articles: listing, reading, and read/star state.
use super::{PAGE_SIZE, Store};
use crate::model::{Article, Query, StatePatch};
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
            "a.id, a.feed_id, f.title, a.title, a.url, a.author, a.published, {}, substr(a.text, 1, 220), s.read, s.starred",
            if html { "a.html" } else { "''" }
        )
    }

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
        })
    }

    pub fn article(&self, id: i64) -> Result<Article> {
        let sql = format!(
            "SELECT {} {ARTICLE_JOINS} WHERE a.id = ?1",
            Self::article_columns(true)
        );
        Ok(self.conn.query_row(&sql, [id], Self::article_row)?)
    }

    /// Look up an article without treating deletion as a database error.
    pub fn article_if_exists(&self, id: i64) -> Result<Option<Article>> {
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
    pub fn set_article_states(&self, ids: &[i64], patch: StatePatch) -> Result<Vec<(i64, String)>> {
        self.immediate(|s| s.set_article_states_now(ids, patch))
    }

    fn set_article_states_now(&self, ids: &[i64], patch: StatePatch) -> Result<Vec<(i64, String)>> {
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
    pub fn set_state(&self, id: i64, patch: StatePatch) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read = COALESCE(?1, read), starred = COALESCE(?2, starred) WHERE article_id = ?3",
            params![patch.read, patch.starred, id],
        )?;
        Ok(())
    }

    pub fn set_read(&self, id: i64, read: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read = ?1 WHERE article_id = ?2",
            params![read, id],
        )?;
        Ok(())
    }

    pub fn set_starred(&self, id: i64, value: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET starred = ?1 WHERE article_id = ?2",
            params![value, id],
        )?;
        Ok(())
    }

    /// Mark every unread article matching `q` read, returning the changed IDs for undo.
    pub fn mark_read(&self, q: &Query) -> Result<Vec<i64>> {
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
            .collect::<Result<Vec<i64>, _>>()?)
    }

    pub fn undo_read(&self, ids: &[i64]) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read = 0 WHERE article_id IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(ids)?],
        )?;
        Ok(())
    }
}
