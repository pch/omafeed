//! Mirroring a sync service's account, such as Miniflux. While one is connected, the service
//! fetches the feeds and decides which feeds and folders the library has; omafeed reads them
//! from it and keeps read and star state the same on both sides. [`backend`] says what a
//! service offers, and [`miniflux`] implements it; nothing else here knows which service it is.
//!
//! A sync ([`sync`]) works in this order:
//!
//! 1. Send the read and star changes waiting in the outbox. Changes made here win over the
//!    server's state until the server has them.
//! 2. Subscribe the server to feeds it does not have (the library's own, when connecting, and
//!    feeds imported or given a new address since). A feed it refuses stays, showing why, and
//!    is offered again only by a sync someone asked for ([`Trigger::Manual`]).
//! 3. Read the categories and the feeds, and the entries that changed:
//!    - on the first sync, every unread and every starred entry, and those published in the
//!      last [`RECENT_DAYS`] days;
//!    - after that, only the entries added, read, unread, starred or unstarred since the last
//!      sync started (by the server's clock), which on most syncs is a handful.
//! 4. Store all of it in one transaction ([`crate::Store`]), with each entry's read and star
//!    state set to the server's, except where a change still waits to be sent.
//! 5. Compare the library's unread count of each feed, and its number of starred articles,
//!    with the server's (two small requests). Where they differ, say because the server
//!    deleted old entries, read that feed's whole unread state, or every starred entry, and
//!    store it as in step 4. This keeps the library right without reading everything again.
//! 6. Send any changes step 4 queued: local articles that became entries keep their state.
//!
//! The library keeps entries the server has since deleted; only a feed removed from the
//! server takes its articles with it. After disconnecting, the first refresh of a feed finds
//! the articles it had from the server by their links, so they are not added again.
//!
//! While connected, feeds and categories are changed on the server first, and then in the
//! library ([`subscribe`], [`update_feed`], [`unsubscribe`], [`rename_category`],
//! [`remove_category`]); [`import`] adds an OPML document's feeds here and lets the sync
//! send them. Feeds the server would not take stay the library's own and are changed only
//! here. Folders of one's own, to arrange categories in, are the library's alone.
pub mod backend;
pub mod miniflux;

pub use backend::Backend;

use crate::{
    Db, Error,
    model::{FeedId, FolderId},
    store::account::{Mirror, Unread, same_url},
};
use anyhow::{Context, Result, anyhow};
use backend::{Category, FeedChange, Selection};
use std::collections::HashSet;

/// How many days back the first sync reads read, unstarred entries; unread and starred ones
/// come whatever their age.
pub const RECENT_DAYS: i64 = 30;

/// How far before the cursor a sync reads changes, in seconds: the server's clock is only
/// known to the second, and changes made while it answered may be missing from its answer.
/// Reading a change twice does no harm.
const OVERLAP: i64 = 60;

/// What started a sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The schedule, or another command that syncs along the way. Feeds the server refused
    /// are not offered again: it fetches each one before answering, so a dead feed would slow
    /// every sync down.
    Scheduled,
    /// Someone asked for it, such as the refresh button or `omafeed sync`: feeds the server
    /// refused are offered again too.
    Manual,
}

/// What a sync did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Read and star changes sent to the server.
    pub sent: usize,
    /// Feeds the server was subscribed to.
    pub subscribed: usize,
    /// Feeds the server would not subscribe to, with its reason, by title.
    pub refused: Vec<(String, String)>,
    /// Feeds the library has from the server now.
    pub feeds: usize,
    /// Articles added.
    pub new_articles: usize,
    /// Articles whose read or star state changed to the server's.
    pub state_changes: usize,
}

/// Connect the account `backend` signs in to, and run the first [`sync`]: the library then
/// mirrors it. Fails if an account is connected already, or if the server refuses the key or
/// is too old. If the first sync fails, the account stays connected; sync again to finish.
pub async fn connect(db: &Db, backend: &dyn Backend) -> Result<Report> {
    let user = backend.check().await?;
    let server = backend.server().to_owned();
    db.call(move |s| s.begin_account(&server, &user)).await?;
    sync(db, backend, Trigger::Manual).await
}

/// Stop mirroring the account: the library keeps every feed, folder and article, and omafeed
/// fetches the feeds itself again. With a `backend`, changes not yet sent are sent first; any
/// left are dropped. Returns how many were dropped.
pub async fn disconnect(db: &Db, backend: Option<&dyn Backend>) -> Result<i64> {
    let _turn = db.sync_turn().await;
    if db.call(|s| s.account()).await?.is_none() {
        return Err(not_connected());
    }
    if let Some(backend) = backend {
        // Best effort: disconnecting must work with the server gone.
        let _ = send(db, backend).await;
    }
    db.call(|s| s.end_account()).await
}

/// Bring the library and the server into step, as the [module](self) describes. A failure is
/// recorded on the account ([`crate::model::Account::error`]) as well as returned.
pub async fn sync(db: &Db, backend: &dyn Backend, trigger: Trigger) -> Result<Report> {
    let _turn = db.sync_turn().await;
    sync_in_turn(db, backend, trigger).await
}

/// [`sync`], once it is this caller's turn.
async fn sync_in_turn(db: &Db, backend: &dyn Backend, trigger: Trigger) -> Result<Report> {
    let Some(account) = db.call(|s| s.account()).await? else {
        return Err(not_connected());
    };
    if same_url(&account.server) != same_url(backend.server()) {
        return Err(Error::Validation(format!(
            "The library is connected to {}, not {}",
            account.server,
            backend.server()
        ))
        .into());
    }
    let result = sync_now(db, backend, trigger).await;
    let now = chrono::Utc::now().timestamp();
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    db.call(move |s| s.record_sync(now, error.as_deref()))
        .await?;
    result
}

fn not_connected() -> anyhow::Error {
    Error::Validation("No account is connected".into()).into()
}

/// Add the subscriptions and folders of an OPML document to the library, as
/// [`crate::Store::import`] does, and sync to subscribe the server to them, each in the
/// category named like the folder it is in (made if need be). A feed the server refuses stays,
/// showing why, as when connecting. Returns what the import did, and the sync's report or why
/// it failed; the next sync sends what this one could not.
pub async fn import(db: &Db, backend: &dyn Backend, text: String) -> Result<Imported> {
    let _turn = db.sync_turn().await;
    if db.call(|s| s.account()).await?.is_none() {
        return Err(not_connected());
    }
    let report = db.call(move |s| s.import_now(&text)).await?;
    let synced = sync_in_turn(db, backend, Trigger::Scheduled).await;
    Ok(Imported { report, synced })
}

/// What [`import`] did.
#[derive(Debug)]
pub struct Imported {
    /// The subscriptions added to the library, skipped, and refused.
    pub report: crate::model::ImportReport,
    /// The sync that sent them to the server.
    pub synced: Result<Report>,
}

/// Subscribe the server to the feed at `url` in the category `folder` is (or the service's
/// top-level category, for the top level), named `title` unless that is empty, and sync to
/// bring it and its entries into the library; returns its feed there. Fails with
/// [`Error::DuplicateFeed`] if the library has the address, and with [`Error::Validation`]
/// for a folder of one's own, which the server knows nothing of.
pub async fn subscribe(
    db: &Db,
    backend: &dyn Backend,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<FeedId> {
    let _turn = db.sync_turn().await;
    let url = crate::util::validate_url(&url)?;
    let checked = url.clone();
    db.call(move |s| s.require_new_url(&checked, None)).await?;
    let category = category_for(db, backend, folder).await?;
    let remote = backend.subscribe(&url, category).await?;
    let title = title.trim().to_owned();
    let renamed = match title.is_empty() {
        true => Ok(()),
        false => {
            let change = FeedChange {
                title: Some(title),
                ..FeedChange::default()
            };
            backend.update_feed(remote, &change).await
        }
    };
    // The feed is on the server either way: bring it here before reporting a failed rename.
    sync_in_turn(db, backend, Trigger::Scheduled).await?;
    renamed.with_context(|| {
        format!(
            "Subscribed to {url}, but {} would not give it its title",
            backend.name()
        )
    })?;
    db.call(move |s| s.feed_for_remote(remote))
        .await?
        .ok_or_else(|| {
            anyhow!(
                "{} subscribed to {url} but does not list it",
                backend.name()
            )
        })
}

/// Rename the feed `id`, move it to the category `folder` is, or change its address: on the
/// server, then in the library, by a sync. A feed the server would not take is changed only
/// in the library; a new address makes the next sync offer it to the server again. Fails as
/// [`subscribe`] does.
pub async fn update_feed(
    db: &Db,
    backend: &dyn Backend,
    id: FeedId,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<()> {
    let _turn = db.sync_turn().await;
    let url = crate::util::validate_url(&url)?;
    let Some(remote) = db.call(move |s| s.feed_remote_id(id)).await? else {
        return db
            .call(move |s| s.immediate(|s| s.update_feed_now(id, &title, &url, folder)))
            .await;
    };
    let (current, checked) = (db.call(|s| s.library()).await?, url.clone());
    let feed = current.feed(id).ok_or(Error::FeedNotFound(id))?;
    db.call(move |s| s.require_new_url(&checked, Some(id)))
        .await?;
    let title = title.trim().to_owned();
    let change = FeedChange {
        title: (!title.is_empty() && title != feed.title).then(|| title.clone()),
        feed_url: (same_url(&url) != same_url(&feed.url)).then(|| url.clone()),
        category: match folder == feed.folder {
            true => None,
            false => category_for(db, backend, folder).await?,
        },
    };
    if change == FeedChange::default() {
        return Ok(());
    }
    backend.update_feed(remote, &change).await?;
    sync_in_turn(db, backend, Trigger::Scheduled)
        .await
        .map(drop)
}

/// Unsubscribe from the feed `id` on the server, and remove it and its articles from the
/// library. A feed the server would not take is removed only from the library.
pub async fn unsubscribe(db: &Db, backend: &dyn Backend, id: FeedId) -> Result<()> {
    let _turn = db.sync_turn().await;
    if let Some(remote) = db.call(move |s| s.feed_remote_id(id)).await? {
        backend.unsubscribe(remote).await?;
    }
    db.call(move |s| s.delete_feed_now(id)).await
}

/// Rename the category the folder `id` is, on the server and in the library. Fails with
/// [`Error::Validation`] for a folder of one's own, which [`crate::Store::rename_folder`]
/// renames.
pub async fn rename_category(
    db: &Db,
    backend: &dyn Backend,
    id: FolderId,
    name: String,
) -> Result<()> {
    let _turn = db.sync_turn().await;
    let remote = remote_category(db, backend, id).await?;
    let name = name.trim().to_owned();
    backend.rename_category(remote, &name).await?;
    db.call(move |s| s.rename_folder_now(id, &name)).await
}

/// Remove the category the folder `id` is, on the server and from the library; folders
/// inside it move up, as [`crate::Store::delete_folder`] moves them. Fails with
/// [`Error::Validation`] for a folder of one's own, and, where the service would delete the
/// category's feeds with it, while it has any.
pub async fn remove_category(db: &Db, backend: &dyn Backend, id: FolderId) -> Result<()> {
    let _turn = db.sync_turn().await;
    let remote = remote_category(db, backend, id).await?;
    if backend.deleting_a_category_deletes_its_feeds()
        && db.call(move |s| s.remote_feeds_in(id)).await? > 0
    {
        return Err(Error::Validation(format!(
            "{} would remove this category's feeds with it; move or remove them first",
            backend.name()
        ))
        .into());
    }
    backend.delete_category(remote).await?;
    db.call(move |s| s.immediate(|s| s.delete_folder_now(id)))
        .await
}

/// The category the folder `id` is.
async fn remote_category(db: &Db, backend: &dyn Backend, id: FolderId) -> Result<i64> {
    db.call(move |s| s.folder_remote_id(id))
        .await?
        .ok_or_else(|| {
            Error::Validation(format!(
                "This folder is your own, not a {} category",
                backend.name()
            ))
            .into()
        })
}

/// The category for feeds put in `folder`: the one it is, or the service's top-level
/// category (made if need be) for the top level, if it has one.
async fn category_for(
    db: &Db,
    backend: &dyn Backend,
    folder: Option<FolderId>,
) -> Result<Option<i64>> {
    match folder {
        Some(id) => {
            let category = db.call(move |s| s.folder_remote_id(id)).await?;
            let category = category.ok_or_else(|| {
                Error::Validation(format!(
                    "{} keeps feeds in its categories, and this folder is your own; choose \
                     one of its categories",
                    backend.name()
                ))
            })?;
            Ok(Some(category))
        }
        None => {
            let mut categories = backend.categories().await?;
            top_level(backend, &mut categories).await
        }
    }
}

/// The service's top-level category among `categories`, made if need be, or `None` if feeds
/// may have no category.
async fn top_level(backend: &dyn Backend, categories: &mut Vec<Category>) -> Result<Option<i64>> {
    match backend.top_level_category() {
        Some(title) => category_named(backend, categories, title).await.map(Some),
        None => Ok(None),
    }
}

/// The category titled `title` (ignoring case) among `categories`, made and added to them if
/// there is none.
async fn category_named(
    backend: &dyn Backend,
    categories: &mut Vec<Category>,
    title: &str,
) -> Result<i64> {
    if let Some(c) = categories
        .iter()
        .find(|c| c.title.eq_ignore_ascii_case(title))
    {
        return Ok(c.id);
    }
    let created = backend.create_category(title).await?;
    let id = created.id;
    categories.push(created);
    Ok(id)
}

async fn sync_now(db: &Db, backend: &dyn Backend, trigger: Trigger) -> Result<Report> {
    let mut report = Report {
        sent: send(db, backend).await?,
        ..Report::default()
    };
    let mut categories = backend.categories().await?;
    let mut feeds = backend.feeds().await?;
    let remote_urls = feeds.iter().map(|f| same_url(&f.feed_url)).collect();
    let offered = subscribe_local(
        db,
        backend,
        remote_urls,
        &mut categories,
        trigger,
        &mut report,
    )
    .await?;
    if offered {
        // Read them again: there are new feeds, and maybe categories.
        categories = backend.categories().await?;
        feeds = backend.feeds().await?;
    }
    report.feeds = feeds.len();

    let cursor = db.call(|s| s.sync_cursor()).await?;
    let now = chrono::Utc::now().timestamp();
    let mut mirror = match cursor {
        None => everything(backend, now).await?,
        Some(cursor) => {
            let changed = backend
                .entries(Selection::ChangedAfter(cursor - OVERLAP))
                .await?;
            Mirror {
                cursor: Some(changed.server_time.unwrap_or(now)),
                entries: changed.entries,
                ..Mirror::default()
            }
        }
    };
    mirror.tree = Some((categories, feeds));
    mirror.now = now;
    let applied = db.call(move |s| s.mirror(&mirror)).await?;
    report.new_articles = applied.new_articles;
    report.state_changes = applied.state_changes;
    if cursor.is_some() {
        repair(db, backend, now, &mut report).await?;
    }
    report.sent += send(db, backend).await?;
    Ok(report)
}

/// What the first sync reads: every unread and starred entry, and the recent ones.
async fn everything(backend: &dyn Backend, now: i64) -> Result<Mirror> {
    let unread = backend.entries(Selection::Unread(None)).await?;
    let starred = backend.entries(Selection::Starred).await?;
    let recent = backend
        .entries(Selection::PublishedAfter(now - RECENT_DAYS * 86_400))
        .await?;
    let ids = |entries: &[backend::Entry]| entries.iter().map(|e| e.id).collect::<HashSet<_>>();
    let mut mirror = Mirror {
        // Changes made after the server started answering are read by the next sync.
        cursor: Some(unread.server_time.unwrap_or(now)),
        unread: Some(Unread {
            feeds: None,
            entries: ids(&unread.entries),
        }),
        starred: Some(ids(&starred.entries)),
        ..Mirror::default()
    };
    mirror.entries = unread.entries;
    mirror.entries.extend(starred.entries);
    mirror.entries.extend(recent.entries);
    Ok(mirror)
}

/// Step 5 of the [module](self): read the whole state of the feeds whose counts disagree.
async fn repair(db: &Db, backend: &dyn Backend, now: i64, report: &mut Report) -> Result<()> {
    let counts = backend.unread_counts().await?;
    let starred = backend.starred_count().await?;
    let drift = db.call(move |s| s.drift(&counts, starred)).await?;
    if drift.is_empty() {
        return Ok(());
    }
    let mut mirror = Mirror {
        now,
        ..Mirror::default()
    };
    if !drift.unread.is_empty() {
        let unread = backend
            .entries(Selection::Unread(Some(&drift.unread)))
            .await?;
        mirror.unread = Some(Unread {
            feeds: Some(drift.unread),
            entries: unread.entries.iter().map(|e| e.id).collect(),
        });
        mirror.entries.extend(unread.entries);
    }
    if drift.starred {
        let starred = backend.entries(Selection::Starred).await?;
        mirror.starred = Some(starred.entries.iter().map(|e| e.id).collect());
        mirror.entries.extend(starred.entries);
    }
    let applied = db.call(move |s| s.mirror(&mirror)).await?;
    report.new_articles += applied.new_articles;
    report.state_changes += applied.state_changes;
    Ok(())
}

/// Send the outbox, returning how many changes the server took. Entries the server no longer
/// has are skipped by it, so they cannot hold up the changes after them.
async fn send(db: &Db, backend: &dyn Backend) -> Result<usize> {
    let unsent = db.call(|s| s.unsent()).await?;
    if unsent.is_empty() {
        return Ok(0);
    }
    let ids = |field: &str, value: bool| -> Vec<i64> {
        unsent
            .iter()
            .filter(|u| u.field == field && u.value == value)
            .map(|u| u.entry)
            .collect()
    };
    for value in [true, false] {
        backend.set_read(&ids("read", value), value).await?;
        backend.set_starred(&ids("starred", value), value).await?;
    }
    let count = unsent.len();
    db.call(move |s| s.sent(&unsent)).await?;
    Ok(count)
}

/// Subscribe the server to the library's feeds it does not have (`remote_urls` are its
/// feeds' addresses, as [`same_url`] writes them), in the category named like each feed's
/// folder (made if need be), or the service's top-level category. Feeds showing an error are
/// offered only when `trigger` is [`Trigger::Manual`]. Returns whether any were offered.
async fn subscribe_local(
    db: &Db,
    backend: &dyn Backend,
    remote_urls: HashSet<String>,
    categories: &mut Vec<Category>,
    trigger: Trigger,
    report: &mut Report,
) -> Result<bool> {
    let with_errors = trigger == Trigger::Manual;
    let local = db
        .call(move |s| s.local_feeds(&remote_urls, with_errors))
        .await?;
    let offered = !local.is_empty();
    for feed in local {
        let category = match &feed.folder {
            Some(name) => Some(category_named(backend, categories, name).await?),
            None => top_level(backend, categories).await?,
        };
        match backend.subscribe(&feed.url, category).await {
            Ok(_) => report.subscribed += 1,
            Err(e) => {
                let reason = format!("{} would not subscribe: {e:#}", backend.name());
                let id = feed.id;
                let error = reason.clone();
                db.call(move |s| s.set_feed_error(id, &error)).await?;
                report.refused.push((feed.title, reason));
            }
        }
    }
    Ok(offered)
}
