//! Schema migrations, applied in order when a library is opened.
use super::Store;
use anyhow::Result;
use rusqlite::{Connection, params};

enum Migration {
    Sql(&'static str),
    Rust(fn(&Connection) -> Result<()>),
}

/// Schema migrations; entry `n` upgrades `user_version` from `n` to `n + 1`.
const MIGRATIONS: &[Migration] = &[
    Migration::Sql(SCHEMA_V1),
    // Plain-text extraction no longer inserts spaces before punctuation after links.
    Migration::Rust(reextract_text),
    // Blocks also separate words where they end, and `details`/`summary` count as blocks.
    Migration::Rust(reextract_text),
    // Feed revisions, so a refresh that raced an edit or a deletion stores nothing.
    Migration::Sql(FEED_REVISIONS),
    // A Miniflux account: what each feed, folder and article is on the server, and read and
    // star changes waiting to be sent there.
    Migration::Sql(ACCOUNT),
    // Incremental syncs: where the next one reads the server's changes from, and when the
    // last one was tried, so a failing sync waits for its interval too.
    Migration::Sql(SYNC_CURSOR),
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

/// Every existing feed gets its ID as revision; new revisions start above all of them, so a
/// recreated feed never matches a revision handed out before.
const FEED_REVISIONS: &str = "
ALTER TABLE feeds ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
UPDATE feeds SET revision = id;
CREATE TABLE meta(
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_revision INTEGER NOT NULL
);
INSERT INTO meta(id, next_revision) VALUES(1, (SELECT COALESCE(max(id), 0) + 1 FROM feeds));
";

/// While an `account` row exists, Miniflux owns the library: `remote_id` is what a feed
/// (Miniflux feed), folder (category) or article (entry) is on the server, or NULL for one it
/// does not have. Changing the read or star state of an article with a `remote_id` queues the
/// new value in `outbox`, in the same statement, whichever code made the change; the sync
/// sends it and clears it. One row per article and field, so the newest value replaces an
/// unsent older one.
const ACCOUNT: &str = "
ALTER TABLE feeds ADD COLUMN remote_id INTEGER;
CREATE UNIQUE INDEX feed_remote ON feeds(remote_id) WHERE remote_id IS NOT NULL;
ALTER TABLE folders ADD COLUMN remote_id INTEGER;
CREATE UNIQUE INDEX folder_remote ON folders(remote_id) WHERE remote_id IS NOT NULL;
ALTER TABLE articles ADD COLUMN remote_id INTEGER;
CREATE UNIQUE INDEX article_remote ON articles(remote_id) WHERE remote_id IS NOT NULL;
CREATE TABLE account(
    id INTEGER PRIMARY KEY CHECK (id = 1),
    server TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    username TEXT NOT NULL,
    last_sync INTEGER,
    error TEXT
);
CREATE TABLE outbox(
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    field TEXT NOT NULL CHECK (field IN ('read', 'starred')),
    value INTEGER NOT NULL,
    PRIMARY KEY(article_id, field)
) WITHOUT ROWID;
CREATE TRIGGER outbox_read AFTER UPDATE OF read ON article_state
WHEN new.read IS NOT old.read
 AND (SELECT remote_id FROM articles WHERE id = new.article_id) IS NOT NULL
BEGIN
    INSERT INTO outbox(article_id, field, value) VALUES(new.article_id, 'read', new.read)
    ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value;
END;
CREATE TRIGGER outbox_starred AFTER UPDATE OF starred ON article_state
WHEN new.starred IS NOT old.starred
 AND (SELECT remote_id FROM articles WHERE id = new.article_id) IS NOT NULL
BEGIN
    INSERT INTO outbox(article_id, field, value) VALUES(new.article_id, 'starred', new.starred)
    ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value;
END;
";

/// `cursor` is the server's clock when the last sync started reading its changes; `NULL`
/// makes the next sync read everything, as the first one does.
const SYNC_CURSOR: &str = "
ALTER TABLE account ADD COLUMN cursor INTEGER;
ALTER TABLE account ADD COLUMN last_attempt INTEGER;
UPDATE account SET last_attempt = last_sync;
";

/// Recompute search/preview text from stored HTML (the FTS index follows via trigger).
fn reextract_text(conn: &Connection) -> Result<()> {
    let articles = conn
        .prepare("SELECT id, html FROM articles")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut update = conn.prepare("UPDATE articles SET text = ?1 WHERE id = ?2")?;
    for (id, html) in articles {
        update.execute(params![crate::content::plain(&html), id])?;
    }
    Ok(())
}

impl Store {
    pub(super) fn migrate(conn: &mut Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(usize::MAX);
        if version > MIGRATIONS.len() {
            return Err(crate::Error::NewerDatabase.into());
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
}
