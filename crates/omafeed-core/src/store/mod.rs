//! The library database: one SQLite connection ([`Store`]) and the worker thread that owns it
//! ([`Db`]). Each file adds one concern's methods to `Store`.
mod articles;
mod feeds;
mod folders;
mod schema;
mod search;
mod worker;

use crate::fuzzy::Vocabulary;
use anyhow::{Result, bail};
use rusqlite::Connection;
use std::{
    cell::{Cell, RefCell},
    path::Path,
};

pub use worker::Db;

/// Number of articles per list page.
pub const PAGE_SIZE: usize = 200;

pub struct Store {
    conn: Connection,
    /// The library's words for forgiving search, and the state they were read at.
    vocabulary: RefCell<Option<((i64, u64), Vocabulary)>>,
    /// Counts writes through this connection that add, change or remove article text, which
    /// is what makes `vocabulary` stale. Anything new that does so must call
    /// `articles_changed`; starring, reading and moving feeds do not change any words.
    article_writes: Cell<u64>,
    /// How many times the word list was read from the index, for tests and benchmarks.
    vocabulary_loads: Cell<u64>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        Self::migrate(&mut conn)?;
        // A view of the words in the search index, private to this connection: nothing is
        // written to the library file.
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.article_vocab
             USING fts5vocab(main, article_fts, 'row')",
        )?;
        Ok(Self {
            conn,
            vocabulary: RefCell::default(),
            article_writes: Cell::default(),
            vocabulary_loads: Cell::default(),
        })
    }

    /// How many times forgiving search has read the library's word list. It is kept between
    /// searches and read again only after articles change.
    #[doc(hidden)]
    pub fn vocabulary_loads(&self) -> u64 {
        self.vocabulary_loads.get()
    }

    pub(super) fn articles_changed(&self) {
        self.article_writes.set(self.article_writes.get() + 1);
    }

    /// SQLite's change counter for this connection: it differs between two calls only if
    /// another connection (another program) committed a write in between. Writes made through
    /// this connection never change it.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// Hand out a feed revision no feed has had before. Call it inside the transaction that
    /// uses the revision.
    pub(super) fn next_revision(conn: &Connection) -> Result<i64> {
        Ok(conn.query_row(
            "UPDATE meta SET next_revision = next_revision + 1 WHERE id = 1
             RETURNING next_revision - 1",
            [],
            |r| r.get(0),
        )?)
    }

    pub(super) fn name(name: &str) -> Result<&str> {
        let n = name.trim();
        if n.is_empty() || n.len() > 300 {
            bail!("Name must contain 1–300 bytes");
        }
        Ok(n)
    }

    /// Run several reads against one snapshot of the library, so pages read one after another
    /// neither repeat nor skip articles when another program writes in between. Inside another
    /// snapshot or transaction, the reads simply join it.
    pub fn snapshot<T>(&self, reads: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return reads(self);
        }
        let tx = self.conn.unchecked_transaction()?;
        let result = reads(self)?;
        tx.commit()?;
        Ok(result)
    }

    /// Run reads and writes in one transaction that takes the write lock at once, so nothing
    /// another program writes can come between them. Nothing is kept if `work` fails.
    pub fn immediate<T>(&self, work: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let result = work(self)?;
        tx.commit()?;
        Ok(result)
    }
}
