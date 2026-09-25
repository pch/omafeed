use crate::opml;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize)]
pub struct Folder {
    pub id: i64,
    pub parent: Option<i64>,
    pub name: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Feed {
    pub site_url: String,
    pub id: i64,
    pub folder: Option<i64>,
    pub title: String,
    pub url: String,
    pub unread: i64,
    pub error: Option<String>,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub next_fetch: i64,
    pub failures: i64,
}
#[derive(Clone, Debug, Default, Serialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn decode(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
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
pub struct Store {
    pub conn: Connection,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            bail!("Database was created by a newer Omafeed version");
        }
        if version == 0 {
            conn.execute_batch("BEGIN;
CREATE TABLE folders(id INTEGER PRIMARY KEY, parent INTEGER REFERENCES folders(id) ON DELETE SET NULL, name TEXT NOT NULL);
CREATE UNIQUE INDEX folder_siblings ON folders(COALESCE(parent,0), name);
CREATE TABLE feeds(id INTEGER PRIMARY KEY, folder INTEGER REFERENCES folders(id) ON DELETE SET NULL, title TEXT NOT NULL, url TEXT NOT NULL UNIQUE, site_url TEXT NOT NULL DEFAULT '', etag TEXT, modified TEXT, last_attempt INTEGER, last_success INTEGER, next_fetch INTEGER NOT NULL DEFAULT 0, failures INTEGER NOT NULL DEFAULT 0, error TEXT);
CREATE TABLE articles(id INTEGER PRIMARY KEY, feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE, identity TEXT NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL, author TEXT NOT NULL, published INTEGER NOT NULL, first_seen INTEGER NOT NULL, html TEXT NOT NULL, text TEXT NOT NULL, UNIQUE(feed_id,identity));
CREATE INDEX article_order ON articles(published DESC,id DESC);
CREATE TABLE article_state(article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE, read INTEGER NOT NULL DEFAULT 0, starred INTEGER NOT NULL DEFAULT 0);
CREATE INDEX unread_state ON article_state(read,article_id);
CREATE VIRTUAL TABLE article_fts USING fts5(title,author,text,content='articles',content_rowid='id',tokenize='unicode61');
CREATE TRIGGER article_insert AFTER INSERT ON articles BEGIN
 INSERT INTO article_state(article_id) VALUES(new.id);
 INSERT INTO article_fts(rowid,title,author,text) VALUES(new.id,new.title,new.author,new.text); END;
CREATE TRIGGER article_delete AFTER DELETE ON articles BEGIN
 INSERT INTO article_fts(article_fts,rowid,title,author,text) VALUES('delete',old.id,old.title,old.author,old.text); END;
CREATE TRIGGER article_update AFTER UPDATE ON articles BEGIN
 INSERT INTO article_fts(article_fts,rowid,title,author,text) VALUES('delete',old.id,old.title,old.author,old.text);
 INSERT INTO article_fts(rowid,title,author,text) VALUES(new.id,new.title,new.author,new.text); END;
PRAGMA user_version=1; COMMIT;")?;
        }
        Ok(Self { conn })
    }
    pub fn library(&self) -> Result<Library> {
        let folders = self
            .conn
            .prepare("SELECT id,parent,name FROM folders ORDER BY name COLLATE NOCASE")?
            .query_map([], |r| {
                Ok(Folder {
                    id: r.get(0)?,
                    parent: r.get(1)?,
                    name: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let feeds = self.conn.prepare("SELECT f.id,f.folder,f.title,f.url,(SELECT count(*) FROM articles a JOIN article_state s ON s.article_id=a.id WHERE a.feed_id=f.id AND s.read=0),f.error,f.etag,f.modified,f.next_fetch,f.failures,f.site_url FROM feeds f ORDER BY f.title COLLATE NOCASE")?.query_map([], |r|Ok(Feed{id:r.get(0)?,folder:r.get(1)?,title:r.get(2)?,url:r.get(3)?,unread:r.get(4)?,error:r.get(5)?,etag:r.get(6)?,modified:r.get(7)?,next_fetch:r.get(8)?,failures:r.get(9)?,site_url:r.get(10)?}))?.collect::<Result<Vec<_>,_>>()?;
        let (unread, starred) = self.conn.query_row(
            "SELECT COALESCE(sum(read=0),0),COALESCE(sum(starred),0) FROM article_state",
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
            "INSERT INTO folders(name,parent) VALUES(?1,?2)",
            params![Self::name(name)?, parent],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn rename_folder(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET name=?1 WHERE id=?2",
            params![Self::name(name)?, id],
        )?;
        Ok(())
    }
    pub fn move_folder(&self, id: i64, parent: Option<i64>) -> Result<()> {
        if let Some(p) = parent {
            let cycle: bool=self.conn.query_row("WITH RECURSIVE descendants(id) AS (SELECT ?1 UNION ALL SELECT f.id FROM folders f JOIN descendants d ON f.parent=d.id) SELECT EXISTS(SELECT 1 FROM descendants WHERE id=?2)",params![id,p],|r|r.get(0))?;
            if cycle {
                bail!("A folder cannot be moved inside itself or its children");
            }
        }
        self.conn.execute(
            "UPDATE folders SET parent=?1 WHERE id=?2",
            params![parent, id],
        )?;
        Ok(())
    }
    /// Removing a folder keeps all feeds and promotes child folders to its parent.
    pub fn delete_folder(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let parent: Option<i64> =
            tx.query_row("SELECT parent FROM folders WHERE id=?1", [id], |r| r.get(0))?;
        // Disambiguate names when promoting children into an existing sibling group.
        let children = tx
            .prepare("SELECT id,name FROM folders WHERE parent=?1")?
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        for (child, name) in children {
            let mut candidate = name.clone();
            let mut suffix = 2;
            while tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM folders WHERE parent IS ?1 AND name=?2 AND id<>?3)",
                params![parent, candidate, id],
                |r| r.get::<_, bool>(0),
            )? {
                candidate = format!("{name} ({suffix})");
                suffix += 1;
            }
            tx.execute(
                "UPDATE folders SET parent=?1,name=?2 WHERE id=?3",
                params![parent, candidate, child],
            )?;
        }
        tx.execute(
            "UPDATE feeds SET folder=?1 WHERE folder=?2",
            params![parent, id],
        )?;
        tx.execute("DELETE FROM folders WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn add_feed(&self, title: &str, url: &str, folder: Option<i64>) -> Result<i64> {
        let url = opml::validate_url(url)?;
        let title = if title.trim().is_empty() {
            &url
        } else {
            Self::name(title)?
        };
        self.conn.execute(
            "INSERT INTO feeds(title,url,folder) VALUES(?1,?2,?3)",
            params![title, url, folder],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn edit_feed(&self, id: i64, title: &str, folder: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET title=?1,folder=?2 WHERE id=?3",
            params![Self::name(title)?, folder, id],
        )?;
        Ok(())
    }
    pub fn update_feed(&self, id: i64, title: &str, url: &str, folder: Option<i64>) -> Result<()> {
        let url = opml::validate_url(url)?;
        self.conn.execute("UPDATE feeds SET title=?1,folder=?2,etag=CASE WHEN url<>?3 THEN NULL ELSE etag END,modified=CASE WHEN url<>?3 THEN NULL ELSE modified END,site_url=CASE WHEN url<>?3 THEN '' ELSE site_url END,next_fetch=CASE WHEN url<>?3 THEN 0 ELSE next_fetch END,error=CASE WHEN url<>?3 THEN NULL ELSE error END,failures=CASE WHEN url<>?3 THEN 0 ELSE failures END,url=?3 WHERE id=?4",params![Self::name(title)?,folder,url,id])?;
        Ok(())
    }
    pub fn delete_feed(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM feeds WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn import(&mut self, text: &str) -> Result<ImportReport> {
        let doc = opml::parse(text)?;
        self.conn.execute_batch("SAVEPOINT import")?;
        let mut report = ImportReport::default();
        let result = self.import_outlines(&doc.body.outlines, None, &mut report, 0);
        if let Err(e) = result {
            self.conn
                .execute_batch("ROLLBACK TO import; RELEASE import")?;
            return Err(e);
        }
        self.conn.execute_batch("RELEASE import")?;
        Ok(report)
    }
    fn import_outlines(
        &self,
        outlines: &[opml::Outline],
        parent: Option<i64>,
        report: &mut ImportReport,
        depth: usize,
    ) -> Result<()> {
        if depth > 32 {
            bail!("OPML folders exceed 32 levels");
        }
        for o in outlines {
            if let Some(url) = &o.url {
                let url = match opml::validate_url(url) {
                    Ok(u) => u,
                    Err(e) => {
                        report.invalid.push(format!("{}: {e}", o.name()));
                        continue;
                    }
                };
                if self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM feeds WHERE url=?1)",
                    [&url],
                    |r| r.get::<_, bool>(0),
                )? {
                    report.skipped += 1;
                    continue;
                }
                match self.add_feed(o.name(), &url, parent) {
                    Ok(id) => {
                        report.added += 1;
                        if let Some(site) = o
                            .site_url
                            .as_deref()
                            .and_then(|s| opml::validate_url(s).ok())
                        {
                            self.conn.execute(
                                "UPDATE feeds SET site_url=?1 WHERE id=?2",
                                params![site, id],
                            )?;
                        }
                    }
                    Err(e) => report.invalid.push(format!("{}: {e}", o.name())),
                }
            } else {
                let name = if o.name().trim().is_empty() {
                    "Untitled"
                } else {
                    o.name()
                };
                let id: Option<i64> = self
                    .conn
                    .query_row(
                        "SELECT id FROM folders WHERE parent IS ?1 AND name=?2",
                        params![parent, name],
                        |r| r.get(0),
                    )
                    .optional()?;
                let id = match id {
                    Some(id) => id,
                    None => self.add_folder(name, parent)?,
                };
                self.import_outlines(&o.children, Some(id), report, depth + 1)?;
            }
        }
        Ok(())
    }
    pub fn export(&self) -> Result<String> {
        let lib = self.library()?;
        fn outlines(lib: &Library, parent: Option<i64>, out: &mut String) {
            for f in lib.folders.iter().filter(|f| f.parent == parent) {
                out.push_str(&format!("<outline text=\"{}\">\n", opml::escape(&f.name)));
                outlines(lib, Some(f.id), out);
                out.push_str("</outline>\n");
            }
            for f in lib.feeds.iter().filter(|f| f.folder == parent) {
                out.push_str(&format!(
                    "<outline type=\"rss\" text=\"{}\" xmlUrl=\"{}\" htmlUrl=\"{}\"/>\n",
                    opml::escape(&f.title),
                    opml::escape(&f.url),
                    opml::escape(&f.site_url)
                ));
            }
        }
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\"><head><title>Omafeed subscriptions</title></head><body>\n",
        );
        outlines(&lib, None, &mut out);
        out.push_str("</body></opml>\n");
        Ok(out)
    }
    fn predicate(q: &Query) -> (String, Vec<rusqlite::types::Value>) {
        let mut terms: Vec<String> = Vec::new();
        let mut values = Vec::new();
        match q.scope {
            Scope::Unread => terms.push("s.read=0".into()),
            Scope::Starred => terms.push("s.starred=1".into()),
            Scope::Today => {
                terms.push("a.published>=?".into());
                values.push(
                    chrono::Local::now()
                        .date_naive()
                        .and_hms_opt(0, 0, 0)
                        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
                        .map(|t| t.timestamp())
                        .unwrap_or(0)
                        .into(),
                );
            }
            Scope::Feed(id) => {
                terms.push("a.feed_id=?".into());
                values.push(id.into());
            }
            Scope::Folder(id) => {
                terms.push("f.folder IN (WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT folders.id FROM folders JOIN tree ON folders.parent=tree.id) SELECT id FROM tree)".into());
                values.push(id.into());
            }
            Scope::All => {}
        }
        if q.unread_only {
            terms.push("s.read=0".into());
        }
        let tokens: Vec<_> = q
            .search
            .split_whitespace()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect();
        if !tokens.is_empty() {
            terms.push("a.id IN (SELECT rowid FROM article_fts WHERE article_fts MATCH ?)".into());
            values.push(tokens.join(" AND ").into());
        }
        (
            if terms.is_empty() {
                "1".into()
            } else {
                terms.join(" AND ")
            },
            values,
        )
    }
    pub fn articles(&self, q: &Query) -> Result<Vec<Article>> {
        let (predicate, mut values) = Self::predicate(q);
        values.push((q.offset as i64).into());
        let sql = format!(
            "SELECT a.id,a.feed_id,f.title,a.title,a.url,a.author,a.published,'',substr(a.text,1,220),s.read,s.starred FROM articles a JOIN feeds f ON f.id=a.feed_id JOIN article_state s ON s.article_id=a.id WHERE {predicate} ORDER BY a.published DESC,a.id DESC LIMIT 200 OFFSET ?"
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
        Ok(self.conn.query_row("SELECT a.id,a.feed_id,f.title,a.title,a.url,a.author,a.published,a.html,substr(a.text,1,220),s.read,s.starred FROM articles a JOIN feeds f ON f.id=a.feed_id JOIN article_state s ON s.article_id=a.id WHERE a.id=?1",[id],Self::article_row)?)
    }
    pub fn set_read(&self, id: i64, read: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET read=?1 WHERE article_id=?2",
            params![read, id],
        )?;
        Ok(())
    }
    pub fn set_starred(&self, id: i64, value: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE article_state SET starred=?1 WHERE article_id=?2",
            params![value, id],
        )?;
        Ok(())
    }
    pub fn mark_read(&mut self, q: &Query) -> Result<Vec<i64>> {
        let (predicate, values) = Self::predicate(q);
        let tx = self.conn.transaction()?;
        let ids=tx.prepare(&format!("SELECT a.id FROM articles a JOIN feeds f ON f.id=a.feed_id JOIN article_state s ON s.article_id=a.id WHERE s.read=0 AND ({predicate})"))?.query_map(rusqlite::params_from_iter(values),|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
        for id in &ids {
            tx.execute("UPDATE article_state SET read=1 WHERE article_id=?1", [id])?;
        }
        tx.commit()?;
        Ok(ids)
    }
    pub fn undo_read(&mut self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for id in ids {
            tx.execute("UPDATE article_state SET read=0 WHERE article_id=?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }
}

type Job = Box<dyn FnOnce(&mut Store) + Send>;
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
                    job(&mut store);
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
        rx.recv().await?
    }
}
