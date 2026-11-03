//! Feeds: the library listing, subscriptions, refresh results, and OPML import and export.
use super::Store;
use crate::{
    model::{Download, Feed, Folder, ImportReport, Library},
    opml,
    util::validate_url,
};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

const LIBRARY_FEEDS: &str = "
SELECT f.id, f.folder, f.title, f.url, f.site_url,
       (SELECT count(*) FROM articles a JOIN article_state s ON s.article_id = a.id
        WHERE a.feed_id = f.id AND s.read = 0),
       f.error, f.etag, f.modified, f.next_fetch, f.failures, f.revision
FROM feeds f
ORDER BY f.title COLLATE NOCASE";

const UPSERT_ARTICLE: &str = "
INSERT INTO articles(feed_id, identity, title, url, author, published, first_seen, html, text)
VALUES(?1, ?2, ?3, ?4, ?5, COALESCE(?6, ?7), ?7, ?8, ?9)
ON CONFLICT(feed_id, identity) DO UPDATE SET
    title = excluded.title,
    url = excluded.url,
    author = excluded.author,
    published = COALESCE(?6, articles.published),
    html = excluded.html,
    text = excluded.text";

/// Changing a feed URL clears everything learned from the old URL and gives the feed revision
/// `?5`.
const UPDATE_FEED: &str = "
UPDATE feeds SET
    title = ?1,
    folder = ?2,
    revision = CASE WHEN url <> ?3 THEN ?5 ELSE revision END,
    etag = CASE WHEN url <> ?3 THEN NULL ELSE etag END,
    modified = CASE WHEN url <> ?3 THEN NULL ELSE modified END,
    site_url = CASE WHEN url <> ?3 THEN '' ELSE site_url END,
    next_fetch = CASE WHEN url <> ?3 THEN 0 ELSE next_fetch END,
    error = CASE WHEN url <> ?3 THEN NULL ELSE error END,
    failures = CASE WHEN url <> ?3 THEN 0 ELSE failures END,
    url = ?3
WHERE id = ?4";

impl Store {
    /// Folders, feeds and totals, all read from one snapshot so they agree with each other.
    pub fn library(&self) -> Result<Library> {
        self.snapshot(Self::read_library)
    }

    fn read_library(&self) -> Result<Library> {
        let folders = self
            .conn
            .prepare("SELECT id, parent, name FROM folders ORDER BY name COLLATE NOCASE")?
            .query_map([], |r| {
                Ok(Folder {
                    id: r.get(0)?,
                    parent: r.get(1)?,
                    name: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let feeds = self
            .conn
            .prepare(LIBRARY_FEEDS)?
            .query_map([], |r| {
                Ok(Feed {
                    id: r.get(0)?,
                    folder: r.get(1)?,
                    title: r.get(2)?,
                    url: r.get(3)?,
                    site_url: r.get(4)?,
                    unread: r.get(5)?,
                    error: r.get(6)?,
                    etag: r.get(7)?,
                    modified: r.get(8)?,
                    next_fetch: r.get(9)?,
                    failures: r.get(10)?,
                    revision: r.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let (unread, starred) = self.conn.query_row(
            "SELECT COALESCE(sum(read = 0), 0), COALESCE(sum(starred), 0) FROM article_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(Library {
            folders,
            feeds,
            unread,
            starred,
        })
    }

    pub fn add_feed(&self, title: &str, url: &str, folder: Option<i64>) -> Result<i64> {
        let url = validate_url(url)?;
        let title = if title.trim().is_empty() {
            &url
        } else {
            Self::name(title)?
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO feeds(title, url, folder, revision) VALUES(?1, ?2, ?3, ?4)",
            params![title, url, folder, Self::next_revision(&tx)?],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn update_feed(&self, id: i64, title: &str, url: &str, folder: Option<i64>) -> Result<()> {
        let url = validate_url(url)?;
        let tx = self.conn.unchecked_transaction()?;
        let revision = Self::next_revision(&tx)?;
        tx.execute(
            UPDATE_FEED,
            params![Self::name(title)?, folder, url, id, revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Stored articles for a feed, and how many of them are starred.
    pub fn feed_article_counts(&self, id: i64) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT count(*), COALESCE(SUM(s.starred), 0)
             FROM articles a JOIN article_state s ON s.article_id = a.id
             WHERE a.feed_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    pub fn delete_feed(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM feeds WHERE id = ?1", [id])?;
        self.articles_changed();
        Ok(())
    }

    /// Store a successful download of the feed `feed_id` at `revision`; validators are only
    /// saved with committed content. Nothing is stored if the feed was deleted or is no
    /// longer at that revision.
    pub fn commit_download(
        &mut self,
        feed_id: i64,
        revision: i64,
        download: Download,
        minutes: u32,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM feeds WHERE id = ?1 AND revision = ?2)",
            [feed_id, revision],
            |r| r.get(0),
        )?;
        if !current {
            return Ok(());
        }
        let changes_articles = matches!(download, Download::Updated(_));
        if let Download::Updated(update) = download {
            let mut upsert = tx.prepare(UPSERT_ARTICLE)?;
            for e in update.entries {
                upsert.execute(params![
                    feed_id,
                    e.identity,
                    e.title,
                    e.url,
                    e.author,
                    e.published,
                    now,
                    e.html,
                    e.text
                ])?;
            }
            drop(upsert);
            tx.execute(
                "UPDATE feeds SET etag = ?1, modified = ?2, site_url = COALESCE(?3, site_url) WHERE id = ?4",
                params![update.etag, update.modified, update.site_url, feed_id],
            )?;
        }
        tx.execute(
            "UPDATE feeds SET last_attempt = ?1, last_success = ?1, next_fetch = ?2, error = NULL, failures = 0
             WHERE id = ?3",
            params![now, now + i64::from(minutes) * 60, feed_id],
        )?;
        tx.commit()?;
        if changes_articles {
            self.articles_changed();
        }
        Ok(())
    }

    /// Record a failed refresh of the feed `feed_id` at `revision` and schedule the next
    /// attempt `delay` seconds from `now`. Nothing is recorded if the feed was deleted or is
    /// no longer at that revision.
    pub fn record_failure(
        &self,
        feed_id: i64,
        revision: i64,
        error: &str,
        now: i64,
        delay: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET error = ?1, failures = failures + 1, last_attempt = ?2, next_fetch = ?3
             WHERE id = ?4 AND revision = ?5",
            params![error, now, now + delay, feed_id, revision],
        )?;
        Ok(())
    }

    pub fn import(&mut self, text: &str) -> Result<ImportReport> {
        let doc = opml::parse(text)?;
        let tx = self.conn.transaction()?;
        let mut report = ImportReport::default();
        Self::import_outlines(&tx, &doc.body.outlines, None, &mut report, 0)?;
        tx.commit()?;
        Ok(report)
    }

    fn import_outlines(
        conn: &Connection,
        outlines: &[opml::Outline],
        parent: Option<i64>,
        report: &mut ImportReport,
        depth: usize,
    ) -> Result<()> {
        if depth > 32 {
            bail!("OPML folders exceed 32 levels");
        }
        for o in outlines {
            let Some(url) = &o.url else {
                // Look the folder up by the name it is stored under, so " Work " finds "Work".
                let name = Self::name(if o.name().trim().is_empty() {
                    "Untitled"
                } else {
                    o.name()
                })?;
                let existing: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM folders WHERE parent IS ?1 AND name = ?2",
                        params![parent, name],
                        |r| r.get(0),
                    )
                    .optional()?;
                let id = match existing {
                    Some(id) => id,
                    None => {
                        conn.execute(
                            "INSERT INTO folders(name, parent) VALUES(?1, ?2)",
                            params![name, parent],
                        )?;
                        conn.last_insert_rowid()
                    }
                };
                Self::import_outlines(conn, &o.children, Some(id), report, depth + 1)?;
                continue;
            };
            let url = match validate_url(url) {
                Ok(u) => u,
                Err(e) => {
                    report.invalid.push(format!("{}: {e}", o.name()));
                    continue;
                }
            };
            if conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM feeds WHERE url = ?1)",
                [&url],
                |r| r.get::<_, bool>(0),
            )? {
                report.skipped += 1;
                continue;
            }
            let title = if o.name().trim().is_empty() {
                url.as_str()
            } else {
                match Self::name(o.name()) {
                    Ok(t) => t,
                    Err(e) => {
                        report.invalid.push(format!("{}: {e}", o.name()));
                        continue;
                    }
                }
            };
            let site = o
                .site_url
                .as_deref()
                .and_then(|s| validate_url(s).ok())
                .unwrap_or_default();
            conn.execute(
                "INSERT INTO feeds(title, url, folder, site_url, revision) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![title, url, parent, site, Self::next_revision(conn)?],
            )?;
            report.added += 1;
        }
        Ok(())
    }

    pub fn export(&self) -> Result<String> {
        Ok(opml::export(&self.library()?))
    }
}
