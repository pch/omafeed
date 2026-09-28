use crate::{fetch::Download, opml, util::validate_url};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Number of articles per list page.
pub const PAGE_SIZE: usize = 200;

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: i64,
    pub parent: Option<i64>,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Feed {
    pub id: i64,
    pub folder: Option<i64>,
    pub title: String,
    pub url: String,
    pub site_url: String,
    pub unread: i64,
    pub error: Option<String>,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub next_fetch: i64,
    pub failures: i64,
}

impl Feed {
    /// The website URL, falling back to the feed URL before the first refresh.
    pub fn website(&self) -> &str {
        if self.site_url.is_empty() {
            &self.url
        } else {
            &self.site_url
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Article {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub published: i64,
    pub html: String,
    pub preview: String,
    pub read: bool,
    pub starred: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    #[default]
    Unread,
    Today,
    Starred,
    All,
    Folder(i64),
    Feed(i64),
}

impl Scope {
    pub fn label(&self, lib: &Library) -> String {
        match self {
            Scope::Unread => "All Unread".into(),
            Scope::Today => "Today".into(),
            Scope::Starred => "Starred".into(),
            Scope::All => "All Articles".into(),
            Scope::Feed(id) => lib
                .feeds
                .iter()
                .find(|f| f.id == *id)
                .map_or_else(|| "Feed".into(), |f| f.title.clone()),
            Scope::Folder(id) => lib
                .folders
                .iter()
                .find(|f| f.id == *id)
                .map_or_else(|| "Folder".into(), |f| f.name.clone()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub scope: Scope,
    pub search: String,
    pub unread_only: bool,
    pub offset: usize,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub added: usize,
    pub skipped: usize,
    pub invalid: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Library {
    pub folders: Vec<Folder>,
    pub feeds: Vec<Feed>,
    pub unread: i64,
    pub starred: i64,
}

enum Migration {
    Sql(&'static str),
    Rust(fn(&Connection) -> Result<()>),
}

/// Schema migrations; entry `n` upgrades `user_version` from `n` to `n + 1`.
const MIGRATIONS: &[Migration] = &[
    Migration::Sql(SCHEMA_V1),
    // Plain-text extraction no longer inserts spaces before punctuation after links.
    Migration::Rust(reextract_text),
];

const SCHEMA_V1: &str = r#"
CREATE TABLE folders(
    id INTEGER PRIMARY KEY,
    parent INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    name TEXT NOT NULL
);
CREATE UNIQUE INDEX folder_siblings ON folders(COALESCE(parent, 0), name);
CREATE TABLE feeds(
    id INTEGER PRIMARY KEY,
    folder INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL UNIQUE,
    site_url TEXT NOT NULL DEFAULT '',
    etag TEXT,
    modified TEXT,
    last_attempt INTEGER,
    last_success INTEGER,
    next_fetch INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    error TEXT
);
CREATE TABLE articles(
    id INTEGER PRIMARY KEY,
    feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    identity TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    author TEXT NOT NULL,
    published INTEGER NOT NULL,
    first_seen INTEGER NOT NULL,
    html TEXT NOT NULL,
    text TEXT NOT NULL,
    UNIQUE(feed_id, identity)
);
CREATE INDEX article_order ON articles(published DESC, id DESC);
CREATE TABLE article_state(
    article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    read INTEGER NOT NULL DEFAULT 0,
    starred INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX unread_state ON article_state(read, article_id);
CREATE VIRTUAL TABLE article_fts USING fts5(
    title, author, text, content='articles', content_rowid='id', tokenize='unicode61'
);
CREATE TRIGGER article_insert AFTER INSERT ON articles BEGIN
    INSERT INTO article_state(article_id) VALUES(new.id);
    INSERT INTO article_fts(rowid, title, author, text) VALUES(new.id, new.title, new.author, new.text);
END;
CREATE TRIGGER article_delete AFTER DELETE ON articles BEGIN
    INSERT INTO article_fts(article_fts, rowid, title, author, text)
    VALUES('delete', old.id, old.title, old.author, old.text);
END;
CREATE TRIGGER article_update AFTER UPDATE ON articles BEGIN
    INSERT INTO article_fts(article_fts, rowid, title, author, text)
    VALUES('delete', old.id, old.title, old.author, old.text);
    INSERT INTO article_fts(rowid, title, author, text) VALUES(new.id, new.title, new.author, new.text);
END;
"#;

/// Recompute search/preview text from stored HTML (the FTS index follows via trigger).
fn reextract_text(conn: &Connection) -> Result<()> {
    let articles = conn
        .prepare("SELECT id, html FROM articles")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut update = conn.prepare("UPDATE articles SET text = ?1 WHERE id = ?2")?;
    for (id, html) in articles {
        update.execute(params![crate::article::plain(&html), id])?;
    }
    Ok(())
}

const LIBRARY_FEEDS: &str = "
SELECT f.id, f.folder, f.title, f.url, f.site_url,
       (SELECT count(*) FROM articles a JOIN article_state s ON s.article_id = a.id
        WHERE a.feed_id = f.id AND s.read = 0),
       f.error, f.etag, f.modified, f.next_fetch, f.failures
FROM feeds f
ORDER BY f.title COLLATE NOCASE";

/// Joins shared by every article query; aliases `a`, `f`, and `s` are used by predicates.
const ARTICLE_JOINS: &str = "
FROM articles a
JOIN feeds f ON f.id = a.feed_id
JOIN article_state s ON s.article_id = a.id";

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

/// Changing a feed URL clears everything learned from the old URL.
const UPDATE_FEED: &str = "
UPDATE feeds SET
    title = ?1,
    folder = ?2,
    etag = CASE WHEN url <> ?3 THEN NULL ELSE etag END,
    modified = CASE WHEN url <> ?3 THEN NULL ELSE modified END,
    site_url = CASE WHEN url <> ?3 THEN '' ELSE site_url END,
    next_fetch = CASE WHEN url <> ?3 THEN 0 ELSE next_fetch END,
    error = CASE WHEN url <> ?3 THEN NULL ELSE error END,
    failures = CASE WHEN url <> ?3 THEN 0 ELSE failures END,
    url = ?3
WHERE id = ?4";

const FOLDER_CYCLE: &str = "
WITH RECURSIVE descendants(id) AS (
    SELECT ?1
    UNION ALL
    SELECT f.id FROM folders f JOIN descendants d ON f.parent = d.id
)
SELECT EXISTS(SELECT 1 FROM descendants WHERE id = ?2)";

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        Self::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    fn migrate(conn: &mut Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(usize::MAX);
        if version > MIGRATIONS.len() {
            bail!("Database was created by a newer Omafeed version");
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            match migration {
                Migration::Sql(sql) => tx.execute_batch(sql)?,
                Migration::Rust(run) => run(&tx)?,
            }
            tx.pragma_update(None, "user_version", (index + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// SQLite's change counter for this connection: it differs between two calls only if
    /// another connection (another program) committed a write in between. Writes made through
    /// this connection never change it.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    pub fn library(&self) -> Result<Library> {
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

    fn name(name: &str) -> Result<&str> {
        let n = name.trim();
        if n.is_empty() || n.len() > 300 {
            bail!("Name must contain 1–300 bytes");
        }
        Ok(n)
    }

    pub fn add_folder(&self, name: &str, parent: Option<i64>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO folders(name, parent) VALUES(?1, ?2)",
            params![Self::name(name)?, parent],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_folder(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET name = ?1 WHERE id = ?2",
            params![Self::name(name)?, id],
        )?;
        Ok(())
    }

    pub fn move_folder(&self, id: i64, parent: Option<i64>) -> Result<()> {
        if let Some(p) = parent
            && self
                .conn
                .query_row(FOLDER_CYCLE, params![id, p], |r| r.get::<_, bool>(0))?
        {
            bail!("A folder cannot be moved inside itself or its children");
        }
        self.conn.execute(
            "UPDATE folders SET parent = ?1 WHERE id = ?2",
            params![parent, id],
        )?;
        Ok(())
    }

    /// Rename and move a folder atomically.
    pub fn edit_folder(&self, id: i64, name: &str, parent: Option<i64>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.rename_folder(id, name)?;
        self.move_folder(id, parent)?;
        tx.commit()?;
        Ok(())
    }

    /// Removing a folder keeps all feeds and promotes child folders to its parent.
    pub fn delete_folder(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let parent: Option<i64> =
            tx.query_row("SELECT parent FROM folders WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        let children = tx
            .prepare("SELECT id, name FROM folders WHERE parent = ?1")?
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        // Disambiguate names when promoting children into an existing sibling group.
        for (child, name) in children {
            let mut candidate = name.clone();
            let mut suffix = 2;
            while tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM folders WHERE parent IS ?1 AND name = ?2 AND id <> ?3)",
                params![parent, candidate, id],
                |r| r.get::<_, bool>(0),
            )? {
                candidate = format!("{name} ({suffix})");
                suffix += 1;
            }
            tx.execute(
                "UPDATE folders SET parent = ?1, name = ?2 WHERE id = ?3",
                params![parent, candidate, child],
            )?;
        }
        tx.execute(
            "UPDATE feeds SET folder = ?1 WHERE folder = ?2",
            params![parent, id],
        )?;
        tx.execute("DELETE FROM folders WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_feed(&self, title: &str, url: &str, folder: Option<i64>) -> Result<i64> {
        let url = validate_url(url)?;
        let title = if title.trim().is_empty() {
            &url
        } else {
            Self::name(title)?
        };
        self.conn.execute(
            "INSERT INTO feeds(title, url, folder) VALUES(?1, ?2, ?3)",
            params![title, url, folder],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn edit_feed(&self, id: i64, title: &str, folder: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET title = ?1, folder = ?2 WHERE id = ?3",
            params![Self::name(title)?, folder, id],
        )?;
        Ok(())
    }

    pub fn update_feed(&self, id: i64, title: &str, url: &str, folder: Option<i64>) -> Result<()> {
        let url = validate_url(url)?;
        self.conn
            .execute(UPDATE_FEED, params![Self::name(title)?, folder, url, id])?;
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
        Ok(())
    }

    /// Store a successful download; validators are only saved with committed content.
    pub fn commit_download(
        &mut self,
        feed_id: i64,
        download: Download,
        minutes: u32,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM feeds WHERE id = ?1)",
            [feed_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(());
        }
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
        Ok(())
    }

    /// Record a failed refresh and schedule the next attempt `delay` seconds from `now`.
    pub fn record_failure(&self, feed_id: i64, error: &str, now: i64, delay: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET error = ?1, failures = failures + 1, last_attempt = ?2, next_fetch = ?3
             WHERE id = ?4",
            params![error, now, now + delay, feed_id],
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
                let name = if o.name().trim().is_empty() {
                    "Untitled"
                } else {
                    o.name()
                };
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
                            params![Self::name(name)?, parent],
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
                "INSERT INTO feeds(title, url, folder, site_url) VALUES(?1, ?2, ?3, ?4)",
                params![title, url, parent, site],
            )?;
            report.added += 1;
        }
        Ok(())
    }

    pub fn export(&self) -> Result<String> {
        Ok(opml::export(&self.library()?))
    }

    fn predicate(q: &Query) -> (String, Vec<rusqlite::types::Value>) {
        let mut terms: Vec<&str> = Vec::new();
        let mut values = Vec::new();
        match q.scope {
            Scope::Unread => terms.push("s.read = 0"),
            Scope::Starred => terms.push("s.starred = 1"),
            Scope::Today => {
                terms.push("a.published >= ?");
                values.push(
                    chrono::Local::now()
                        .date_naive()
                        .and_hms_opt(0, 0, 0)
                        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
                        .map_or(0, |t| t.timestamp())
                        .into(),
                );
            }
            Scope::Feed(id) => {
                terms.push("a.feed_id = ?");
                values.push(id.into());
            }
            Scope::Folder(id) => {
                terms.push(
                    "f.folder IN (WITH RECURSIVE tree(id) AS (
                         SELECT ? UNION ALL
                         SELECT folders.id FROM folders JOIN tree ON folders.parent = tree.id
                     ) SELECT id FROM tree)",
                );
                values.push(id.into());
            }
            Scope::All => {}
        }
        if q.unread_only {
            terms.push("s.read = 0");
        }
        // Quote every token so user input is matched literally, never as FTS syntax.
        let tokens: Vec<_> = q
            .search
            .split_whitespace()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect();
        if !tokens.is_empty() {
            terms.push("a.id IN (SELECT rowid FROM article_fts WHERE article_fts MATCH ?)");
            values.push(tokens.join(" AND ").into());
        }
        let predicate = if terms.is_empty() {
            "1".into()
        } else {
            terms.join(" AND ")
        };
        (predicate, values)
    }

    fn article_columns(html: bool) -> String {
        format!(
            "a.id, a.feed_id, f.title, a.title, a.url, a.author, a.published, {}, substr(a.text, 1, 220), s.read, s.starred",
            if html { "a.html" } else { "''" }
        )
    }

    pub fn articles(&self, q: &Query) -> Result<Vec<Article>> {
        let (predicate, mut values) = Self::predicate(q);
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
        let (predicate, values) = Self::predicate(q);
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

type Job = Box<dyn FnOnce(&mut Store) + Send>;

/// Handle to the database worker thread, which serializes all SQLite access.
#[derive(Clone)]
pub struct Db {
    sender: std::sync::mpsc::Sender<Job>,
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut store = Store::open(path)?;
        let (sender, receiver) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("omafeed-db".into())
            .spawn(move || {
                for job in receiver {
                    // A panicking job fails its own call without stopping the worker.
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut store)));
                }
            })?;
        Ok(Self { sender })
    }

    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = async_channel::bounded(1);
        self.sender
            .send(Box::new(move |s| {
                let _ = tx.send_blocking(f(s));
            }))
            .map_err(|_| anyhow::anyhow!("Database worker stopped"))?;
        rx.recv()
            .await
            .map_err(|_| anyhow::anyhow!("Database operation failed unexpectedly"))?
    }
}
