//! What a sync service has to offer for the library to mirror it: [`Backend`], and the
//! categories, feeds and entries it describes in terms of its own IDs. [`super::miniflux`] and
//! [`super::freshrss`] implement it; the sync itself ([`super::sync`]) only talks to this
//! trait.
//!
//! IDs are the service's own, unique per kind (category, feed, entry) within one account.
//! Feeds and entries have numbers; a category's ID is text, as some services (FreshRSS) know
//! a category only by its name.
//!
//! Services learn of read and star changes made elsewhere in one of two ways
//! ([`Backend::state_sync`]): Miniflux lists the entries changed since a time, and FreshRSS
//! gives the complete sets of unread and starred entries instead.
use crate::model::Service;
use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;
use std::collections::{HashMap, HashSet};

/// The account an API key signs in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The user's ID on the server, or 0 for a service that has none (FreshRSS).
    pub id: i64,
    /// The name the user signs in with.
    pub username: String,
}

/// A category: the service's folder. Categories do not nest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    /// Its ID on the server: a number written as text (Miniflux), or its name (FreshRSS).
    pub id: String,
    /// Its name, unique among the account's categories.
    pub title: String,
}

/// A subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feed {
    /// Its ID on the server.
    pub id: i64,
    /// The name shown for it.
    pub title: String,
    /// The feed's own address.
    pub feed_url: String,
    /// The website it belongs to, or empty.
    pub site_url: String,
    /// The [`Category::id`] it is in, or `None` for none.
    pub category: Option<String>,
    /// Why the service's last fetch of it failed, if it did.
    pub error: Option<String>,
}

/// An article, with its read and star state as the service has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its ID on the server.
    pub id: i64,
    /// The [`Feed::id`] it came from.
    pub feed: i64,
    /// The original on the web, possibly relative to the feed's website, or empty.
    pub url: String,
    /// Its title, possibly with HTML entities.
    pub title: String,
    /// Its authors, as the feed names them, or empty.
    pub author: String,
    /// Its HTML, which the library sanitizes before storing.
    pub html: String,
    /// When it was published, in Unix seconds, if the feed said.
    pub published: Option<i64>,
    /// Addresses of attached files, such as a podcast episode's audio.
    pub enclosures: Vec<String>,
    /// Whether it has been read.
    pub read: bool,
    /// Whether it is starred.
    pub starred: bool,
}

/// Which entries [`Backend::entries`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection<'a> {
    /// Every unread entry, or those of the given feeds.
    Unread(Option<&'a [i64]>),
    /// Every starred entry.
    Starred,
    /// Entries published after this time (Unix seconds).
    PublishedAfter(i64),
    /// Entries added or whose read or star state changed after this time on the server's
    /// clock (Unix seconds).
    ChangedAfter(i64),
}

/// Entries a [`Backend::entries`] call returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entries {
    /// The entries, each once.
    pub entries: Vec<Entry>,
    /// The server's clock when it started answering (Unix seconds), if it said: every change
    /// after it is left for the next [`Selection::ChangedAfter`].
    pub server_time: Option<i64>,
}

/// What [`Backend::update_feed`] changes; `None` leaves that part as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedChange {
    /// The feed's new title.
    pub title: Option<String>,
    /// Its new address.
    pub feed_url: Option<String>,
    /// The [`Category::id`] to move it to, or `Some(None)` to take it out of any category
    /// (for a service that has [`Backend::top_level_category`] `None`).
    pub category: Option<Option<String>>,
}

/// How a sync learns of read and star changes made elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateSync {
    /// The service lists the entries changed since a time ([`Selection::ChangedAfter`]), and
    /// counts unread entries per feed and starred ones ([`Backend::unread_counts`],
    /// [`Backend::starred_count`]) to find what such a list missed.
    Changes,
    /// The service cannot say what changed, but gives the IDs of every unread and every
    /// starred entry cheaply ([`Backend::unread_ids`], [`Backend::starred_ids`]), the IDs of
    /// new entries ([`Backend::new_ids`]), and entries by ID ([`Backend::entries_by_id`]).
    IdSets,
}

/// Entry IDs a [`Backend::new_ids`] call returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ids {
    /// The IDs, each once.
    pub ids: Vec<i64>,
    /// The server's clock when it started answering (Unix seconds), if it said, as
    /// [`Entries::server_time`].
    pub server_time: Option<i64>,
}

/// The failure of a method that `backend` does not offer, as its [`Backend::state_sync`] says.
fn unsupported<T>(backend: &(impl Backend + ?Sized), what: &str) -> Result<T> {
    Err(anyhow!("{} cannot {what}", backend.name()))
}

/// A sync service the library can mirror, signed in to one account. Every call talks to the
/// server; errors carry the service's own message.
///
/// The futures are boxed so that callers can hold any service as `&dyn Backend`.
pub trait Backend: Send + Sync {
    /// The service it is.
    fn service(&self) -> Service;

    /// The service's name, for messages: "Miniflux" or "FreshRSS".
    fn name(&self) -> &'static str {
        self.service().name()
    }

    /// The server's address, as the library keeps it.
    fn server(&self) -> &str;

    /// The category feeds at the library's top level go into, made if need be, for a service
    /// that keeps every feed in a category; `None` if feeds may have none.
    fn top_level_category(&self) -> Option<&'static str>;

    /// Whether deleting a category deletes its feeds too, so the library must refuse to
    /// remove a category that still has any.
    fn deleting_a_category_deletes_its_feeds(&self) -> bool;

    /// Whether [`Backend::update_feed`] can give a feed a new address.
    fn can_change_feed_url(&self) -> bool {
        true
    }

    /// How the service tells of read and star changes made elsewhere.
    fn state_sync(&self) -> StateSync {
        StateSync::Changes
    }

    /// Check that the address, the key and the server's version will do, and say whose
    /// account it is.
    fn check(&self) -> BoxFuture<'_, Result<User>>;

    /// Every category.
    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>>;

    /// Every subscription.
    fn feeds(&self) -> BoxFuture<'_, Result<Vec<Feed>>>;

    /// Every entry `which` selects. For [`StateSync::Changes`].
    fn entries<'a>(&'a self, which: Selection<'a>) -> BoxFuture<'a, Result<Entries>> {
        let _ = which;
        Box::pin(async { unsupported(self, "list entries by their state") })
    }

    /// How many unread entries each feed has, by [`Feed::id`]; feeds with none may be left
    /// out. Cheap: the sync compares it with the library's counts after every pass. For
    /// [`StateSync::Changes`].
    fn unread_counts(&self) -> BoxFuture<'_, Result<HashMap<i64, i64>>> {
        Box::pin(async { unsupported(self, "count unread entries") })
    }

    /// How many entries are starred. Cheap, like [`Backend::unread_counts`]. For
    /// [`StateSync::Changes`].
    fn starred_count(&self) -> BoxFuture<'_, Result<i64>> {
        Box::pin(async { unsupported(self, "count starred entries") })
    }

    /// The IDs of entries added to the server, or whose content changed, at `since` (Unix
    /// seconds on the server's clock) or later. For [`StateSync::IdSets`].
    fn new_ids(&self, since: i64) -> BoxFuture<'_, Result<Ids>> {
        let _ = since;
        Box::pin(async { unsupported(self, "list new entries") })
    }

    /// The IDs of every unread entry. For [`StateSync::IdSets`].
    fn unread_ids(&self) -> BoxFuture<'_, Result<HashSet<i64>>> {
        Box::pin(async { unsupported(self, "list unread entries") })
    }

    /// The IDs of every starred entry. For [`StateSync::IdSets`].
    fn starred_ids(&self) -> BoxFuture<'_, Result<HashSet<i64>>> {
        Box::pin(async { unsupported(self, "list starred entries") })
    }

    /// The entries `ids`, leaving out those the server no longer has. Their read and star
    /// state is the caller's to set, from [`Backend::unread_ids`] and
    /// [`Backend::starred_ids`]. For [`StateSync::IdSets`].
    fn entries_by_id<'a>(&'a self, ids: &'a [i64]) -> BoxFuture<'a, Result<Vec<Entry>>> {
        let _ = ids;
        Box::pin(async { unsupported(self, "read entries by ID") })
    }

    /// Mark the entries `ids` read, or unread. IDs the server no longer has are ignored.
    fn set_read<'a>(&'a self, ids: &'a [i64], read: bool) -> BoxFuture<'a, Result<()>>;

    /// Star the entries `ids`, or remove their stars. IDs the server no longer has are
    /// ignored.
    fn set_starred<'a>(&'a self, ids: &'a [i64], starred: bool) -> BoxFuture<'a, Result<()>>;

    /// Subscribe to the feed at `url` in `category`, returning its [`Feed::id`]. The service
    /// may refuse a feed it cannot fetch.
    fn subscribe<'a>(
        &'a self,
        url: &'a str,
        category: Option<&'a str>,
    ) -> BoxFuture<'a, Result<i64>>;

    /// Rename the feed `id`, change its address, or move it to another category.
    fn update_feed<'a>(&'a self, id: i64, change: &'a FeedChange) -> BoxFuture<'a, Result<()>>;

    /// Unsubscribe from the feed `id`, deleting its entries on the server.
    fn unsubscribe(&self, id: i64) -> BoxFuture<'_, Result<()>>;

    /// Make a category named `title`, returning it. A service that makes a category only
    /// when a feed goes in may return one it has not made yet.
    fn create_category<'a>(&'a self, title: &'a str) -> BoxFuture<'a, Result<Category>>;

    /// Rename the category `id` to `title`, returning its ID now (which changes where the ID
    /// is the name).
    fn rename_category<'a>(&'a self, id: &'a str, title: &'a str) -> BoxFuture<'a, Result<String>>;

    /// Delete the category `id`; see [`Backend::deleting_a_category_deletes_its_feeds`].
    fn delete_category<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;

    /// The full article of the entry `id`, fetched from its web page by the server, which may
    /// know better than the library how to read the site; the entry itself stays as it was.
    /// `None` if the service cannot, and the library fetches the page itself
    /// ([`crate::full_article`]).
    fn fetch_content(&self, id: i64) -> BoxFuture<'_, Result<Option<String>>> {
        let _ = id;
        Box::pin(async { Ok(None) })
    }
}
