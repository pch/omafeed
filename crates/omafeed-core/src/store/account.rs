//! The synced account: connecting and disconnecting it, keeping the library from being
//! edited while it decides the feeds, and storing what a sync brings back.
//!
//! See [`crate::sync`] for the order a sync works in; this file only reads and writes the
//! library.
use super::Store;
use crate::{
    Error, content,
    model::{Account, ArticleId, FeedId, FolderId},
    sync::backend::{Category, Entry, Feed, User},
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

/// A feed the server does not have, to subscribe it to.
#[derive(Clone, Debug)]
pub(crate) struct LocalFeed {
    pub id: FeedId,
    pub url: String,
    pub title: String,
    /// The name of the folder it is in, if any: the category to put it in.
    pub folder: Option<String>,
}

/// What a sync read from the server, for [`Store::mirror`] to store.
#[derive(Default)]
pub(crate) struct Mirror {
    /// The server's categories and feeds, which the library's folders and feeds follow;
    /// `None` leaves them as they are.
    pub tree: Option<(Vec<Category>, Vec<Feed>)>,
    /// Entries to store if the library does not have them, and whose read and star state to
    /// take.
    pub entries: Vec<Entry>,
    /// Every unread entry, of every feed or of some: the library's other articles from those
    /// feeds are read.
    pub unread: Option<Unread>,
    /// Every starred entry: the library's other articles are not starred.
    pub starred: Option<HashSet<i64>>,
    /// Where the next sync reads changes from.
    pub cursor: Cursor,
    pub now: i64,
}

/// What [`Store::mirror`] does with the cursor the next sync reads changes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// Leave it.
    #[default]
    Keep,
    /// Read changes after this time on the server's clock.
    At(i64),
    /// Read everything again, as the first sync does: the server did not say what time it
    /// was, and this computer's clock may differ from its clock.
    Reset,
}

/// Every unread entry of `feeds` (by the server's IDs; `None` for every feed).
pub(crate) struct Unread {
    pub feeds: Option<Vec<i64>>,
    pub entries: HashSet<i64>,
}

/// What [`Store::mirror`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    pub new_articles: usize,
    pub state_changes: usize,
}

/// Where the library and the server's counts disagree, so a sync reads the whole state of
/// those feeds again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Drift {
    /// Feeds (by the server's IDs) whose unread counts differ.
    pub unread: Vec<i64>,
    /// Whether the number of starred articles differs.
    pub starred: bool,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.unread.is_empty() && !self.starred
    }
}

/// How storing an entry went.
enum Stored {
    /// It is new to the library.
    New(ArticleId),
    /// A local article with its link became it, keeping (and queueing) its own state.
    Adopted,
    /// The library had it.
    Known(ArticleId),
}

/// URLs compare as [`validate_url`] writes them, so the same feed matches however it was
/// typed; one it refuses (such as an address with credentials) compares as it is.
pub(crate) fn same_url(url: &str) -> String {
    validate_url(url).unwrap_or_else(|_| url.to_owned())
}

impl Store {
    /// The connected account, or `None` if the library is local.
    pub fn account(&self) -> Result<Option<Account>> {
        Ok(self
            .conn
            .query_row(
                "SELECT server, user_id, username, last_sync, last_attempt, error,
                        (SELECT count(*) FROM outbox)
                 FROM account WHERE id = 1",
                [],
                |r| {
                    Ok(Account {
                        server: r.get(0)?,
                        user_id: r.get(1)?,
                        username: r.get(2)?,
                        last_sync: r.get(3)?,
                        last_attempt: r.get(4)?,
                        error: r.get(5)?,
                        unsent: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// Where the next sync reads the server's changes from, or `None` before the first sync.
    pub(crate) fn sync_cursor(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT cursor FROM account WHERE id = 1", [], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// Fail with [`Error::Managed`] while an account is connected, for adding feeds and
    /// importing them.
    pub(super) fn require_local(&self) -> Result<()> {
        let connected: bool =
            self.conn
                .query_row("SELECT EXISTS(SELECT 1 FROM account)", [], |r| r.get(0))?;
        if connected {
            return Err(Error::Managed.into());
        }
        Ok(())
    }

    /// Fail with [`Error::Managed`] for a feed that is the server's. Feeds it would not take
    /// stay the library's own, to fix or remove; the next sync offers them again.
    pub(super) fn require_local_feed(&self, id: FeedId) -> Result<()> {
        if self.feed_remote_id(id)?.is_some() {
            return Err(Error::Managed.into());
        }
        Ok(())
    }

    /// The feed `id`'s ID on the server, or `None` if it is the library's own. Fails with
    /// [`Error::FeedNotFound`] if there is no such feed.
    pub(crate) fn feed_remote_id(&self, id: FeedId) -> Result<Option<i64>> {
        self.conn
            .query_row("SELECT remote_id FROM feeds WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| Error::FeedNotFound(id).into())
    }

    /// The category the folder `id` is, or `None` if it is one of the user's own (or gone).
    pub(crate) fn folder_remote_id(&self, id: FolderId) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT remote_id FROM folders WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    /// The library's feed for the server's feed `remote_id`, if a sync has brought it.
    pub(crate) fn feed_for_remote(&self, remote_id: i64) -> Result<Option<FeedId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM feeds WHERE remote_id = ?1",
                [remote_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Fail with [`Error::DuplicateFeed`], as adding it would, if a feed other than `except`
    /// has the address `url` (as [`same_url`] writes it).
    pub(crate) fn require_new_url(&self, url: &str, except: Option<FeedId>) -> Result<()> {
        let url = same_url(url);
        let taken = self
            .conn
            .prepare("SELECT id, url FROM feeds")?
            .query_map([], |r| Ok((r.get::<_, FeedId>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .any(|(id, other)| Some(id) != except && same_url(&other) == url);
        if taken {
            return Err(Error::duplicate_feed(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE),
                Some("UNIQUE constraint failed: feeds.url".into()),
            )));
        }
        Ok(())
    }

    /// How many of the server's feeds are directly in the folder `id`.
    pub(crate) fn remote_feeds_in(&self, id: FolderId) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM feeds WHERE folder = ?1 AND remote_id IS NOT NULL",
            [id],
            |r| r.get(0),
        )?)
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
    pub(crate) fn begin_account(&self, server: &str, user: &User) -> Result<()> {
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
    /// the time of the last one that worked, but counts as an attempt: the next scheduled sync
    /// waits a whole interval either way.
    pub(crate) fn record_sync(&self, now: i64, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE account SET last_sync = CASE WHEN ?2 IS NULL THEN ?1 ELSE last_sync END,
                 last_attempt = ?1, error = ?2",
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
    /// not among `remote_urls` (as [`same_url`] writes them). Feeds showing an error (most
    /// likely the server's refusal) only `with_errors`.
    pub(crate) fn local_feeds(
        &self,
        remote_urls: &HashSet<String>,
        with_errors: bool,
    ) -> Result<Vec<LocalFeed>> {
        let feeds = self
            .conn
            .prepare(
                "SELECT f.id, f.url, f.title, d.name FROM feeds f
                 LEFT JOIN folders d ON d.id = f.folder
                 WHERE f.remote_id IS NULL AND (?1 OR f.error IS NULL) ORDER BY f.id",
            )?
            .query_map([with_errors], |r| {
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

    /// Show why the server would not take the feed `id`.
    pub(crate) fn set_feed_error(&self, id: FeedId, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET error = ?1 WHERE id = ?2",
            params![error, id],
        )?;
        Ok(())
    }

    /// Store what a sync read, in one transaction: categories become folders and the
    /// server's feeds replace the library's (a local feed with the same URL becomes the
    /// server's, keeping its articles); new entries are added (a local article with the same
    /// link becomes the entry, keeping its state, which is queued to be sent); and each entry
    /// read, and each article outside a complete unread or starred set, takes the server's
    /// state, except where a change waits to be sent.
    pub(crate) fn mirror(&self, mirror: &Mirror) -> Result<Applied> {
        let applied = self.immediate(|s| s.mirror_now(mirror))?;
        self.articles_changed();
        Ok(applied)
    }

    fn mirror_now(&self, mirror: &Mirror) -> Result<Applied> {
        if let Some((categories, feeds)) = &mirror.tree {
            let folders = self.apply_categories(categories)?;
            self.apply_feeds(feeds, &folders)?;
        }
        let feeds: HashMap<i64, (FeedId, String)> = self
            .conn
            .prepare("SELECT remote_id, id, site_url FROM feeds WHERE remote_id IS NOT NULL")?
            .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?;
        let mut added = HashSet::new();
        let mut changed = HashSet::new();
        let mut seen = HashSet::new();
        for entry in &mirror.entries {
            let Some((feed, site)) = feeds.get(&entry.feed) else {
                continue;
            };
            if !seen.insert(entry.id) {
                continue;
            }
            match self.store_entry(*feed, entry, site, mirror.now)? {
                Stored::Adopted => {}
                Stored::New(id) => {
                    added.insert(id);
                    self.follow_entry(id, entry)?;
                }
                Stored::Known(id) => {
                    if self.follow_entry(id, entry)? {
                        changed.insert(id);
                    }
                }
            }
        }
        if let Some(unread) = &mirror.unread {
            changed.extend(self.follow_unread(unread)?);
        }
        if let Some(starred) = &mirror.starred {
            changed.extend(self.follow_starred(starred)?);
        }
        let cursor = match mirror.cursor {
            Cursor::Keep => None,
            Cursor::At(time) => Some(Some(time)),
            Cursor::Reset => Some(None),
        };
        if let Some(cursor) = cursor {
            self.conn
                .execute("UPDATE account SET cursor = ?1", [cursor])?;
        }
        Ok(Applied {
            new_articles: added.len(),
            // New articles take the server's state too, but that is not a change to report.
            state_changes: changed.difference(&added).count(),
        })
    }

    /// Where the library's unread counts per feed, and its number of starred articles,
    /// differ from the server's `unread` counts (by its feed IDs; feeds left out have none)
    /// and `starred` count.
    pub(crate) fn drift(&self, unread: &HashMap<i64, i64>, starred: i64) -> Result<Drift> {
        let local: Vec<(i64, i64)> = self
            .conn
            .prepare(
                "SELECT f.remote_id, count(s.article_id) FROM feeds f
                 LEFT JOIN articles a ON a.feed_id = f.id AND a.remote_id IS NOT NULL
                 LEFT JOIN article_state s ON s.article_id = a.id AND s.read = 0
                 WHERE f.remote_id IS NOT NULL
                 GROUP BY f.remote_id ORDER BY f.remote_id",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let local_starred: i64 = self.conn.query_row(
            "SELECT count(*) FROM articles a JOIN article_state s ON s.article_id = a.id
             WHERE a.remote_id IS NOT NULL AND s.starred = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(Drift {
            unread: local
                .into_iter()
                .filter(|(feed, count)| unread.get(feed).copied().unwrap_or(0) != *count)
                .map(|(feed, _)| feed)
                .collect(),
            starred: local_starred != starred,
        })
    }

    /// A folder for each category, returning each category's folder. A folder already linked
    /// to the category is renamed to match; otherwise one of the user's folders with its name
    /// becomes it, wherever it is nested, or a new top-level one is made. Folders of
    /// categories gone from the server are removed as [`Store::delete_folder`] removes them.
    fn apply_categories(&self, categories: &[Category]) -> Result<HashMap<i64, FolderId>> {
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

    /// The server's feeds, in the folders of their categories. Feeds the server dropped are
    /// removed with their articles; feeds it does not know and never had stay.
    fn apply_feeds(&self, feeds: &[Feed], folders: &HashMap<i64, FolderId>) -> Result<()> {
        let conn = &self.conn;
        for f in feeds {
            let url = same_url(&f.feed_url);
            let title = Self::name(&f.title).unwrap_or(url.as_str()).to_owned();
            let folder = f.category.and_then(|c| folders.get(&c).copied());
            let site = validate_url(&f.site_url).unwrap_or_default();
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
            match id {
                Some(id) => {
                    // Keep the old address if another feed already has the new one.
                    conn.execute(
                        "UPDATE feeds SET title = ?1, site_url = ?2, folder = ?3, error = ?4,
                             url = CASE WHEN EXISTS(SELECT 1 FROM feeds WHERE url = ?5 AND id <> ?6)
                                        THEN url ELSE ?5 END
                         WHERE id = ?6",
                        params![title, site, folder, f.error, url, id],
                    )?;
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
                            f.error,
                            f.id,
                            Self::next_revision(conn)?
                        ],
                    )
                    .map_err(Error::duplicate_feed)?;
                }
            }
        }
        let ids: Vec<i64> = feeds.iter().map(|f| f.id).collect();
        conn.execute(
            "DELETE FROM feeds WHERE remote_id IS NOT NULL
             AND remote_id NOT IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(&ids)?],
        )?;
        Ok(())
    }

    /// Store `entry` in `feed` (whose website is `site`) unless the library has it. A local
    /// article with the same link becomes the entry, and its read and star state, where it
    /// differs from the server's, is queued to be sent.
    fn store_entry(&self, feed: FeedId, entry: &Entry, site: &str, now: i64) -> Result<Stored> {
        let conn = &self.conn;
        let known: Option<ArticleId> = conn
            .query_row(
                "SELECT id FROM articles WHERE remote_id = ?1",
                [entry.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = known {
            return Ok(Stored::Known(id));
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
            let mut queue = conn.prepare(
                "INSERT INTO outbox(article_id, field, value) VALUES(?1, ?2, ?3)
                 ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value",
            )?;
            if read != entry.read {
                queue.execute(params![id, "read", read])?;
            }
            if starred != entry.starred {
                queue.execute(params![id, "starred", starred])?;
            }
            return Ok(Stored::Adopted);
        }
        let mut body = entry.html.clone();
        for enclosure in &entry.enclosures {
            if let Some(href) = crate::util::resolve_http(&url, enclosure) {
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
                entry.published.unwrap_or(now),
                now,
                html,
                content::plain(&html),
                entry.id
            ],
        )?;
        let id = conn.query_row(
            "SELECT id FROM articles WHERE remote_id = ?1",
            [entry.id],
            |r| r.get(0),
        )?;
        Ok(Stored::New(id))
    }

    /// Give the article `id` the read and star state `entry` has on the server, unless a
    /// change of its own waits to be sent; returns whether it changed.
    fn follow_entry(&self, id: ArticleId, entry: &Entry) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE article_state SET read = ?2, starred = ?3
             WHERE article_id = ?1 AND (read IS NOT ?2 OR starred IS NOT ?3)
               AND NOT EXISTS (SELECT 1 FROM outbox WHERE article_id = ?1)",
            params![id, entry.read, entry.starred],
        )? > 0;
        if changed {
            self.forget_echo(&[id])?;
        }
        Ok(changed)
    }

    /// Mark the articles of `unread`'s feeds read unless they are among its entries, except
    /// those with a change waiting to be sent; returns the articles that changed.
    fn follow_unread(&self, unread: &Unread) -> Result<Vec<ArticleId>> {
        let entries: Vec<i64> = unread.entries.iter().copied().collect();
        let feeds = unread
            .feeds
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let changed = self
            .conn
            .prepare(
                "WITH wanted AS (
                     SELECT a.id, a.remote_id NOT IN (SELECT value FROM json_each(?1)) AS read
                     FROM articles a JOIN feeds f ON f.id = a.feed_id
                     WHERE a.remote_id IS NOT NULL
                       AND (?2 IS NULL OR f.remote_id IN (SELECT value FROM json_each(?2)))
                       AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.article_id = a.id)
                 )
                 UPDATE article_state SET read = w.read FROM wanted w
                 WHERE article_state.article_id = w.id AND article_state.read IS NOT w.read
                 RETURNING article_id",
            )?
            .query_map(params![serde_json::to_string(&entries)?, feeds], |r| {
                r.get(0)
            })?
            .collect::<Result<Vec<ArticleId>, _>>()?;
        self.forget_echo(&changed)?;
        Ok(changed)
    }

    /// Star exactly the articles among `starred`, except those with a change waiting to be
    /// sent; returns the articles that changed.
    fn follow_starred(&self, starred: &HashSet<i64>) -> Result<Vec<ArticleId>> {
        let entries: Vec<i64> = starred.iter().copied().collect();
        let changed = self
            .conn
            .prepare(
                "WITH wanted AS (
                     SELECT a.id, a.remote_id IN (SELECT value FROM json_each(?1)) AS starred
                     FROM articles a
                     WHERE a.remote_id IS NOT NULL
                       AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.article_id = a.id)
                 )
                 UPDATE article_state SET starred = w.starred FROM wanted w
                 WHERE article_state.article_id = w.id AND article_state.starred IS NOT w.starred
                 RETURNING article_id",
            )?
            .query_map([serde_json::to_string(&entries)?], |r| r.get(0))?
            .collect::<Result<Vec<ArticleId>, _>>()?;
        self.forget_echo(&changed)?;
        Ok(changed)
    }

    /// Remove the outbox rows the triggers wrote for `articles` when their state was set to
    /// the server's: there is nothing to send back.
    fn forget_echo(&self, articles: &[ArticleId]) -> Result<()> {
        if !articles.is_empty() {
            self.conn.execute(
                "DELETE FROM outbox WHERE article_id IN (SELECT value FROM json_each(?1))",
                [serde_json::to_string(articles)?],
            )?;
        }
        Ok(())
    }
}
