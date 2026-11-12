//! The Miniflux account: connecting and disconnecting it, keeping the library from being
//! edited while it decides the feeds, and storing what a sync brings back.
//!
//! See [`crate::sync`] for the order a sync works in; this file only reads and writes the
//! library.
use super::Store;
use crate::{
    Error, content,
    model::{Account, ArticleId, FeedId, FolderId},
    sync::miniflux,
    util::{escape, validate_url},
};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};

/// A read or star change waiting to be sent to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unsent {
    pub article: ArticleId,
    /// The entry's ID on the server.
    pub entry: i64,
    /// `read` or `starred`.
    pub field: String,
    pub value: bool,
}

/// A feed Miniflux does not have, to subscribe it to.
#[derive(Clone, Debug)]
pub(crate) struct LocalFeed {
    pub id: FeedId,
    pub url: String,
    pub title: String,
    /// The name of the folder it is in, if any: the category to put it in.
    pub folder: Option<String>,
}

/// What the server has, as one sync read it.
pub(crate) struct Remote {
    pub categories: Vec<miniflux::Category>,
    pub feeds: Vec<miniflux::Feed>,
    /// Entries to store if the library does not have them yet.
    pub entries: Vec<miniflux::Entry>,
    /// Every unread entry's ID.
    pub unread: HashSet<i64>,
    /// Every starred entry's ID.
    pub starred: HashSet<i64>,
    pub now: i64,
}

/// What storing a [`Remote`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    pub new_articles: usize,
    pub state_changes: usize,
}

/// URLs compare as [`validate_url`] writes them, so the same feed matches however it was
/// typed; one it refuses (such as an address with credentials) compares as it is.
pub(crate) fn same_url(url: &str) -> String {
    validate_url(url).unwrap_or_else(|_| url.to_owned())
}

impl Store {
    /// The connected Miniflux account, or `None` if the library is local.
    pub fn account(&self) -> Result<Option<Account>> {
        Ok(self
            .conn
            .query_row(
                "SELECT server, user_id, username, last_sync, error, (SELECT count(*) FROM outbox)
                 FROM account WHERE id = 1",
                [],
                |r| {
                    Ok(Account {
                        server: r.get(0)?,
                        user_id: r.get(1)?,
                        username: r.get(2)?,
                        last_sync: r.get(3)?,
                        error: r.get(4)?,
                        unsent: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Fail with [`Error::Managed`] while an account decides the feeds.
    pub(super) fn require_local(&self) -> Result<()> {
        let connected: bool =
            self.conn
                .query_row("SELECT EXISTS(SELECT 1 FROM account)", [], |r| r.get(0))?;
        if connected {
            return Err(Error::Managed.into());
        }
        Ok(())
    }

    /// Fail with [`Error::Managed`] for a folder that is a category on the server. Folders of
    /// one's own, to arrange categories in, stay free to change.
    pub(super) fn require_local_folder(&self, id: FolderId) -> Result<()> {
        let remote: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1 AND remote_id IS NOT NULL)",
            [id],
            |r| r.get(0),
        )?;
        if remote {
            return Err(Error::Managed.into());
        }
        Ok(())
    }

    /// Start mirroring the account `user` on `server`. Fails if one is connected already.
    pub(crate) fn begin_account(&self, server: &str, user: &miniflux::User) -> Result<()> {
        self.immediate(|s| {
            if let Some(account) = s.account()? {
                return Err(Error::Validation(format!(
                    "Already connected to {}; disconnect first",
                    account.server
                ))
                .into());
            }
            s.conn.execute(
                "INSERT INTO account(id, server, user_id, username) VALUES(1, ?1, ?2, ?3)",
                params![server, user.id, user.username],
            )?;
            Ok(())
        })
    }

    /// Stop mirroring the account: every feed, folder and article stays, as a local one that
    /// omafeed fetches itself again. Unsent changes are dropped; returns how many there were.
    pub(crate) fn end_account(&self) -> Result<i64> {
        self.immediate(|s| {
            let unsent: i64 = s
                .conn
                .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
            s.conn.execute("DELETE FROM outbox", [])?;
            let feeds = s
                .conn
                .prepare("SELECT id FROM feeds")?
                .query_map([], |r| r.get::<_, FeedId>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in feeds {
                // A new revision drops anything a sync was about to store for the feed.
                s.conn.execute(
                    "UPDATE feeds SET remote_id = NULL, etag = NULL, modified = NULL,
                         next_fetch = 0, failures = 0, error = NULL, revision = ?1
                     WHERE id = ?2",
                    params![Self::next_revision(&s.conn)?, id],
                )?;
            }
            s.conn.execute("UPDATE folders SET remote_id = NULL", [])?;
            s.conn.execute("UPDATE articles SET remote_id = NULL", [])?;
            s.conn.execute("DELETE FROM account", [])?;
            Ok(unsent)
        })
    }

    /// Record a sync that finished at `now` with `error`, or without one. A failed sync keeps
    /// the time of the last one that worked.
    pub(crate) fn record_sync(&self, now: i64, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE account SET last_sync = CASE WHEN ?2 IS NULL THEN ?1 ELSE last_sync END,
                 error = ?2",
            params![now, error],
        )?;
        Ok(())
    }

    /// Changes waiting to be sent, oldest article first.
    pub(crate) fn unsent(&self) -> Result<Vec<Unsent>> {
        Ok(self
            .conn
            .prepare(
                "SELECT o.article_id, a.remote_id, o.field, o.value
                 FROM outbox o JOIN articles a ON a.id = o.article_id
                 WHERE a.remote_id IS NOT NULL
                 ORDER BY o.article_id, o.field",
            )?
            .query_map([], |r| {
                Ok(Unsent {
                    article: r.get(0)?,
                    entry: r.get(1)?,
                    field: r.get(2)?,
                    value: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Forget changes the server has accepted, unless they were changed again meanwhile.
    pub(crate) fn sent(&self, changes: &[Unsent]) -> Result<()> {
        self.immediate(|s| {
            let mut delete = s.conn.prepare(
                "DELETE FROM outbox WHERE article_id = ?1 AND field = ?2 AND value = ?3",
            )?;
            for c in changes {
                delete.execute(params![c.article, c.field, c.value])?;
            }
            Ok(())
        })
    }

    /// Feeds the server does not have yet: every feed without a `remote_id`, whose URL is
    /// not among `remote_urls` (as [`same_url`] writes them).
    pub(crate) fn local_feeds(&self, remote_urls: &HashSet<String>) -> Result<Vec<LocalFeed>> {
        let feeds = self
            .conn
            .prepare(
                "SELECT f.id, f.url, f.title, d.name FROM feeds f
                 LEFT JOIN folders d ON d.id = f.folder
                 WHERE f.remote_id IS NULL ORDER BY f.id",
            )?
            .query_map([], |r| {
                Ok(LocalFeed {
                    id: r.get(0)?,
                    url: r.get(1)?,
                    title: r.get(2)?,
                    folder: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(feeds
            .into_iter()
            .filter(|f| !remote_urls.contains(&same_url(&f.url)))
            .collect())
    }

    /// The newest entry the library has, by its ID on the server.
    pub(crate) fn newest_entry(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT max(remote_id) FROM articles", [], |r| r.get(0))?)
    }

    /// Show why the server would not take the feed `id`.
    pub(crate) fn set_feed_error(&self, id: FeedId, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET error = ?1 WHERE id = ?2",
            params![error, id],
        )?;
        Ok(())
    }

    /// Make the library match `remote` in one transaction: categories become folders, the
    /// server's feeds replace the library's (a local feed with the same URL becomes the
    /// server's, keeping its articles), new entries are added (a local article with the same
    /// link becomes the entry, keeping its state, which is queued to be sent), and every
    /// entry's read and star state follows the server's, except where a change waits to be
    /// sent.
    pub(crate) fn apply(&self, remote: &Remote) -> Result<Applied> {
        let applied = self.immediate(|s| s.apply_now(remote))?;
        self.articles_changed();
        Ok(applied)
    }

    fn apply_now(&self, remote: &Remote) -> Result<Applied> {
        let folders = self.apply_categories(&remote.categories)?;
        let feeds = self.apply_feeds(&remote.feeds, &folders)?;
        let site_urls: HashMap<i64, &str> = remote
            .feeds
            .iter()
            .map(|f| (f.id, f.site_url.as_str()))
            .collect();
        let mut applied = Applied::default();
        let mut seen = HashSet::new();
        let mut added = HashSet::new();
        for entry in &remote.entries {
            let Some(&feed) = feeds.get(&entry.feed_id) else {
                continue;
            };
            if !seen.insert(entry.id) {
                continue;
            }
            let site = site_urls.get(&entry.feed_id).copied().unwrap_or_default();
            if self.add_entry(feed, entry, site, remote)? {
                added.insert(entry.id);
            }
        }
        applied.new_articles = added.len();
        // New articles take the server's state too, but that is not a change to report.
        applied.state_changes = self
            .follow_states(&remote.unread, &remote.starred)?
            .into_iter()
            .filter(|entry| !added.contains(entry))
            .count();
        Ok(applied)
    }

    /// A folder for each category, returning each category's folder. A folder already linked
    /// to the category is renamed to match; otherwise one of the user's folders with its name
    /// becomes it, wherever it is nested, or a new top-level one is made. Folders of
    /// categories gone from the server are removed as [`Store::delete_folder`] removes them.
    fn apply_categories(
        &self,
        categories: &[miniflux::Category],
    ) -> Result<HashMap<i64, FolderId>> {
        let conn = &self.conn;
        let mut map = HashMap::new();
        for c in categories {
            let title = Self::name(&c.title).unwrap_or("Untitled");
            let linked: Option<(FolderId, Option<FolderId>, String)> = conn
                .query_row(
                    "SELECT id, parent, name FROM folders WHERE remote_id = ?1",
                    [c.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let id = if let Some((id, parent, name)) = linked {
                if name != title {
                    let name = self.free_folder_name(parent, title, Some(id))?;
                    conn.execute(
                        "UPDATE folders SET name = ?1 WHERE id = ?2",
                        params![name, id],
                    )?;
                }
                id
            } else if let Some(id) = conn
                .query_row(
                    "SELECT id FROM folders WHERE remote_id IS NULL AND name = ?1
                     ORDER BY parent IS NOT NULL, id LIMIT 1",
                    [title],
                    |r| r.get::<_, FolderId>(0),
                )
                .optional()?
            {
                conn.execute(
                    "UPDATE folders SET remote_id = ?1 WHERE id = ?2",
                    params![c.id, id],
                )?;
                id
            } else {
                let name = self.free_folder_name(None, title, None)?;
                conn.execute(
                    "INSERT INTO folders(name, parent, remote_id) VALUES(?1, NULL, ?2)",
                    params![name, c.id],
                )?;
                FolderId(conn.last_insert_rowid())
            };
            map.insert(c.id, id);
        }
        let ids: Vec<i64> = categories.iter().map(|c| c.id).collect();
        let gone = conn
            .prepare(
                "SELECT id FROM folders WHERE remote_id IS NOT NULL
                 AND remote_id NOT IN (SELECT value FROM json_each(?1))",
            )?
            .query_map([serde_json::to_string(&ids)?], |r| r.get::<_, FolderId>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in gone {
            self.delete_folder_now(id)?;
        }
        Ok(map)
    }

    /// `wanted`, or `wanted (2)`, `wanted (3)`, … if a sibling in `parent` other than `except`
    /// has the name.
    fn free_folder_name(
        &self,
        parent: Option<FolderId>,
        wanted: &str,
        except: Option<FolderId>,
    ) -> Result<String> {
        let mut candidate = wanted.to_owned();
        let mut suffix = 2;
        while self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE parent IS ?1 AND name = ?2 AND id IS NOT ?3)",
            params![parent, candidate, except],
            |r| r.get::<_, bool>(0),
        )? {
            candidate = format!("{wanted} ({suffix})");
            suffix += 1;
        }
        Ok(candidate)
    }

    /// The server's feeds, returning each one's feed in the library. Feeds the server dropped
    /// are removed with their articles; feeds it does not know and never had stay.
    fn apply_feeds(
        &self,
        feeds: &[miniflux::Feed],
        folders: &HashMap<i64, FolderId>,
    ) -> Result<HashMap<i64, FeedId>> {
        let conn = &self.conn;
        let mut map = HashMap::new();
        for f in feeds {
            let url = same_url(&f.feed_url);
            let title = Self::name(&f.title).unwrap_or(url.as_str()).to_owned();
            let folder = folders.get(&f.category.id).copied();
            let site = validate_url(&f.site_url).unwrap_or_default();
            let error = (!f.parsing_error_message.is_empty()).then_some(&f.parsing_error_message);
            let linked: Option<FeedId> = conn
                .query_row("SELECT id FROM feeds WHERE remote_id = ?1", [f.id], |r| {
                    r.get(0)
                })
                .optional()?;
            let id = match linked {
                Some(id) => Some(id),
                None => {
                    let adopted: Option<FeedId> = conn
                        .query_row(
                            "SELECT id FROM feeds WHERE remote_id IS NULL AND url = ?1",
                            [&url],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(id) = adopted {
                        // A new revision: a refresh of the local feed must not store anything.
                        conn.execute(
                            "UPDATE feeds SET remote_id = ?1, revision = ?2 WHERE id = ?3",
                            params![f.id, Self::next_revision(conn)?, id],
                        )?;
                    }
                    adopted
                }
            };
            let id = match id {
                Some(id) => {
                    // Keep the old address if another feed already has the new one.
                    conn.execute(
                        "UPDATE feeds SET title = ?1, site_url = ?2, folder = ?3, error = ?4,
                             url = CASE WHEN EXISTS(SELECT 1 FROM feeds WHERE url = ?5 AND id <> ?6)
                                        THEN url ELSE ?5 END
                         WHERE id = ?6",
                        params![title, site, folder, error, url, id],
                    )?;
                    id
                }
                None => {
                    conn.execute(
                        "INSERT INTO feeds(title, url, site_url, folder, error, remote_id, revision)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            title,
                            url,
                            site,
                            folder,
                            error,
                            f.id,
                            Self::next_revision(conn)?
                        ],
                    )
                    .map_err(Error::duplicate_feed)?;
                    FeedId(conn.last_insert_rowid())
                }
            };
            map.insert(f.id, id);
        }
        let ids: Vec<i64> = feeds.iter().map(|f| f.id).collect();
        conn.execute(
            "DELETE FROM feeds WHERE remote_id IS NOT NULL
             AND remote_id NOT IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(&ids)?],
        )?;
        Ok(map)
    }

    /// Store `entry` in `feed` unless the library has it; returns whether it was new. A local
    /// article with the same link becomes the entry, and its read and star state, where it
    /// differs from the server's, is queued to be sent.
    fn add_entry(
        &self,
        feed: FeedId,
        entry: &miniflux::Entry,
        site: &str,
        remote: &Remote,
    ) -> Result<bool> {
        let conn = &self.conn;
        let known: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM articles WHERE remote_id = ?1)",
            [entry.id],
            |r| r.get(0),
        )?;
        if known {
            return Ok(false);
        }
        let url = crate::util::resolve_http(site, &entry.url).unwrap_or_default();
        if !url.is_empty()
            && let Some((id, read, starred)) = conn
                .query_row(
                    "SELECT a.id, s.read, s.starred FROM articles a
                     JOIN article_state s ON s.article_id = a.id
                     WHERE a.feed_id = ?1 AND a.remote_id IS NULL AND a.url = ?2
                     ORDER BY a.id LIMIT 1",
                    params![feed, url],
                    |r| {
                        Ok((
                            r.get::<_, ArticleId>(0)?,
                            r.get::<_, bool>(1)?,
                            r.get::<_, bool>(2)?,
                        ))
                    },
                )
                .optional()?
        {
            conn.execute(
                "UPDATE articles SET remote_id = ?1 WHERE id = ?2",
                params![entry.id, id],
            )?;
            let server_read = !remote.unread.contains(&entry.id);
            let server_starred = remote.starred.contains(&entry.id);
            let mut queue = conn.prepare(
                "INSERT INTO outbox(article_id, field, value) VALUES(?1, ?2, ?3)
                 ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value",
            )?;
            if read != server_read {
                queue.execute(params![id, "read", read])?;
            }
            if starred != server_starred {
                queue.execute(params![id, "starred", starred])?;
            }
            return Ok(false);
        }
        let mut body = entry.content.clone();
        for enclosure in &entry.enclosures {
            if let Some(href) = crate::util::resolve_http(&url, &enclosure.url) {
                body.push_str(&format!(
                    "<p><a href=\"{}\">Download attachment</a></p>",
                    escape(&href)
                ));
            }
        }
        let html = content::sanitize(&body, if url.is_empty() { site } else { &url });
        let title = Some(content::plain(&entry.title))
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "Untitled article".into());
        // Miniflux writes year 1 for an entry without a date.
        let published = Some(entry.published_at.timestamp())
            .filter(|t| *t > 0)
            .unwrap_or(remote.now);
        conn.execute(
            "INSERT INTO articles(feed_id, identity, title, url, author, published, first_seen,
                                  html, text, remote_id)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(feed_id, identity) DO UPDATE SET remote_id = excluded.remote_id",
            params![
                feed,
                format!("miniflux:{}", entry.id),
                title,
                url,
                content::one_line(&entry.author),
                published,
                remote.now,
                html,
                content::plain(&html),
                entry.id
            ],
        )?;
        Ok(true)
    }

    /// Set every entry's read and star state to the server's, except for articles with a
    /// change waiting to be sent; returns the entries whose articles changed. The outbox rows the
    /// triggers write for these changes are removed again: they came from the server.
    fn follow_states(&self, unread: &HashSet<i64>, starred: &HashSet<i64>) -> Result<Vec<i64>> {
        let unread: Vec<i64> = unread.iter().copied().collect();
        let starred: Vec<i64> = starred.iter().copied().collect();
        let changed = self
            .conn
            .prepare(
                "WITH wanted AS (
                     SELECT a.id,
                            a.remote_id NOT IN (SELECT value FROM json_each(?1)) AS read,
                            a.remote_id IN (SELECT value FROM json_each(?2)) AS starred
                     FROM articles a
                     WHERE a.remote_id IS NOT NULL
                       AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.article_id = a.id)
                 )
                 UPDATE article_state SET read = w.read, starred = w.starred
                 FROM wanted w
                 WHERE article_state.article_id = w.id
                   AND (article_state.read IS NOT w.read OR article_state.starred IS NOT w.starred)
                 RETURNING article_id, (SELECT remote_id FROM articles WHERE id = article_id)",
            )?
            .query_map(
                params![
                    serde_json::to_string(&unread)?,
                    serde_json::to_string(&starred)?
                ],
                |r| Ok((r.get::<_, ArticleId>(0)?, r.get::<_, i64>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let articles: Vec<ArticleId> = changed.iter().map(|(id, _)| *id).collect();
        self.conn.execute(
            "DELETE FROM outbox WHERE article_id IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(&articles)?],
        )?;
        Ok(changed.into_iter().map(|(_, entry)| entry).collect())
    }
}
