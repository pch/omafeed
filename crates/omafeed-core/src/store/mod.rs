//! The library database: one SQLite connection ([`Store`]) and the worker thread that owns it
//! ([`Db`]). Each file adds one concern's methods to `Store`.
pub(crate) mod account;
mod articles;
mod feeds;
mod folders;
mod schema;
mod search;
mod worker;

use crate::{
    filters::ContentFilters,
    fuzzy::Vocabulary,
    model::{ArticleId, FeedId, FolderId},
};
use anyhow::Result;
use rusqlite::{Connection, functions::FunctionFlags};
use std::{
    cell::{Cell, RefCell},
    path::Path,
};

pub use worker::Db;

/// IDs go into and come out of SQLite as the integers they are.
macro_rules! sql_id {
    ($($name:ty),*) => {$(
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                i64::column_result(value).map(Self)
            }
        }
    )*};
}

sql_id!(ArticleId, FeedId, FolderId);

/// Number of articles per list page.
pub const PAGE_SIZE: usize = 200;

/// One connection to the library database. Not shared between threads: use [`Db`] for that.
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
    /// Whether content filters are set. Without any, queries stay exactly as they were.
    filtering: Cell<bool>,
}

/// Starred articles stay visible: a filter should not make something saved on purpose vanish.
/// The text is the same preview the article list shows.
const NOT_FILTERED: &str =
    "(s.starred = 1 OR NOT omafeed_blocked(a.title, substr(a.text, 1, 220)))";

impl Store {
    /// Open (creating if need be) the library at `path`, or `:memory:`, and bring its schema
    /// up to date. Fails with [`crate::Error::NewerDatabase`] for a newer one.
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
            filtering: Cell::default(),
        })
    }

    /// Replace the content filters applied to article lists, counts and bulk mark-read.
    /// Invalid patterns are reported and skipped.
    pub fn set_filters(&self, patterns: &[String]) -> Result<()> {
        let filters = ContentFilters::new(patterns);
        self.filtering.set(!filters.is_empty());
        self.conn.create_scalar_function(
            "omafeed_blocked",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| Ok(filters.blocks(&ctx.get::<String>(0)?, &ctx.get::<String>(1)?)),
        )?;
        Ok(())
    }

    /// The condition that leaves filtered articles out (aliases `a` and `s`), if any is set.
    pub(super) fn not_filtered(&self) -> Option<&'static str> {
        self.filtering.get().then_some(NOT_FILTERED)
    }

    /// [`Self::not_filtered`] with a leading ` AND `, or nothing.
    pub(super) fn and_not_filtered(&self) -> String {
        self.not_filtered()
            .map(|c| format!(" AND {c}"))
            .unwrap_or_default()
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
            return Err(crate::Error::Validation("Name must contain 1–300 bytes".into()).into());
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
    /// another program writes can come between them. Nothing is kept if `work` fails. Inside
    /// another transaction, the work joins it (and its failure must fail that one too). Every
    /// write that takes more than one statement goes through here.
    pub fn immediate<T>(&self, work: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return work(self);
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let result = work(self)?;
        tx.commit()?;
        Ok(result)
    }
}
