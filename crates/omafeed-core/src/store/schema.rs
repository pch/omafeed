//! Schema migrations, applied in order when a library is opened.
use super::Store;
use anyhow::{Result, bail};
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
}
