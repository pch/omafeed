//! What a sync service has to offer for the library to mirror it: [`Backend`], and the
//! categories, feeds and entries it describes in terms of its own IDs. [`super::miniflux`] is
//! the one implementation; the sync itself ([`super::sync`]) only talks to this trait.
//!
//! IDs are the service's own, unique per kind (category, feed, entry) within one account.
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::collections::HashMap;

/// The account an API key signs in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The user's ID on the server.
    pub id: i64,
    /// The name the user signs in with.
    pub username: String,
}

/// A category: the service's folder. Categories do not nest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    /// Its ID on the server.
    pub id: i64,
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
    pub category: Option<i64>,
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
    pub category: Option<Option<i64>>,
}

/// A sync service the library can mirror, signed in to one account. Every call talks to the
/// server; errors carry the service's own message.
///
/// The futures are boxed so that callers can hold any service as `&dyn Backend`.
pub trait Backend: Send + Sync {
    /// The service's name, for messages: "Miniflux".
    fn name(&self) -> &'static str;

    /// The server's address, as the library keeps it.
    fn server(&self) -> &str;

    /// The category feeds at the library's top level go into, made if need be, for a service
    /// that keeps every feed in a category; `None` if feeds may have none.
    fn top_level_category(&self) -> Option<&'static str>;

    /// Whether deleting a category deletes its feeds too, so the library must refuse to
    /// remove a category that still has any.
    fn deleting_a_category_deletes_its_feeds(&self) -> bool;

    /// Check that the address, the key and the server's version will do, and say whose
    /// account it is.
    fn check(&self) -> BoxFuture<'_, Result<User>>;

    /// Every category.
    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>>;

    /// Every subscription.
    fn feeds(&self) -> BoxFuture<'_, Result<Vec<Feed>>>;

    /// Every entry `which` selects.
    fn entries<'a>(&'a self, which: Selection<'a>) -> BoxFuture<'a, Result<Entries>>;

    /// How many unread entries each feed has, by [`Feed::id`]; feeds with none may be left
    /// out. Cheap: the sync compares it with the library's counts after every pass.
    fn unread_counts(&self) -> BoxFuture<'_, Result<HashMap<i64, i64>>>;

    /// How many entries are starred. Cheap, like [`Backend::unread_counts`].
    fn starred_count(&self) -> BoxFuture<'_, Result<i64>>;

    /// Mark the entries `ids` read, or unread. IDs the server no longer has are ignored.
    fn set_read<'a>(&'a self, ids: &'a [i64], read: bool) -> BoxFuture<'a, Result<()>>;

    /// Star the entries `ids`, or remove their stars. IDs the server no longer has are
    /// ignored.
    fn set_starred<'a>(&'a self, ids: &'a [i64], starred: bool) -> BoxFuture<'a, Result<()>>;

    /// Subscribe to the feed at `url` in `category`, returning its [`Feed::id`]. The service
    /// may refuse a feed it cannot fetch.
    fn subscribe<'a>(&'a self, url: &'a str, category: Option<i64>) -> BoxFuture<'a, Result<i64>>;

    /// Rename the feed `id`, change its address, or move it to another category.
    fn update_feed<'a>(&'a self, id: i64, change: &'a FeedChange) -> BoxFuture<'a, Result<()>>;

    /// Unsubscribe from the feed `id`, deleting its entries on the server.
    fn unsubscribe(&self, id: i64) -> BoxFuture<'_, Result<()>>;

    /// Make a category named `title`, returning it.
    fn create_category<'a>(&'a self, title: &'a str) -> BoxFuture<'a, Result<Category>>;

    /// Rename the category `id` to `title`.
    fn rename_category<'a>(&'a self, id: i64, title: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Delete the category `id`; see [`Backend::deleting_a_category_deletes_its_feeds`].
    fn delete_category(&self, id: i64) -> BoxFuture<'_, Result<()>>;
}
