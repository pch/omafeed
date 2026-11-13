//! Mirroring a Miniflux account. While one is connected, Miniflux fetches the feeds and decides
//! which feeds and folders the library has; omafeed reads them from it and keeps read and star
//! state the same on both sides. [`miniflux`] is the client for its API.
//!
//! A sync ([`sync`]) works in this order:
//!
//! 1. Send the read and star changes waiting in the outbox. Changes made here win over the
//!    server's state until the server has them. Miniflux before 2.3.2 can only toggle a star,
//!    so stars are toggled only where the server's differ.
//! 2. Subscribe the server to feeds it does not have (the library's own, when connecting); a
//!    feed it refuses stays, showing why.
//! 3. Read the categories, the feeds, every unread and every starred entry, and the entries
//!    published in the last [`RECENT_DAYS`] days that are newer than the newest one the library
//!    has (all of those days on the first sync).
//! 4. Store all of it in one transaction ([`crate::Store`]), with every entry's read and star
//!    state set to the server's, except where a change still waits to be sent.
//! 5. Send any changes step 4 queued: local articles that became entries keep their state.
//!
//! The library keeps entries the server has since deleted; only a feed removed from the
//! server takes its articles with it. After disconnecting, the first refresh of a feed finds
//! the articles it had from the server by their links, so they are not added again.
//!
//! While connected, feeds and categories are changed on the server first, and then in the
//! library ([`subscribe`], [`update_feed`], [`unsubscribe`], [`rename_category`],
//! [`remove_category`]); [`import`] adds an OPML document's feeds here and lets the sync
//! send them. Feeds the server would not take stay the library's own and are
//! changed only here; the next sync offers them to the server again. Folders of one's own,
//! to arrange categories in, are the library's alone.
pub mod miniflux;

use crate::{
    Db, Error,
    model::{FeedId, FolderId},
    store::account::same_url,
};
use anyhow::{Result, anyhow};
use std::collections::HashSet;

/// One sync or change at a time in this process: two at once could both see a star the
/// server lacks, and both toggle it.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How many days back a sync reads read, unstarred entries; unread and starred ones come
/// whatever their age.
pub const RECENT_DAYS: i64 = 30;

/// The category feeds at the top level of the library go into when they are sent to the
/// server; Miniflux makes one by this name for every user.
const DEFAULT_CATEGORY: &str = "All";

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

/// Connect the account `client` signs in to, and run the first [`sync`]: the library then
/// mirrors it. Fails if an account is connected already, or if the server refuses the key.
/// If the first sync fails, the account stays connected; sync again to finish.
pub async fn connect(db: &Db, client: &miniflux::Client) -> Result<Report> {
    let user = client.me().await?;
    let server = client.server().to_owned();
    db.call(move |s| s.begin_account(&server, &user)).await?;
    sync(db, client).await
}

/// Stop mirroring the account: the library keeps every feed, folder and article, and omafeed
/// fetches the feeds itself again. With a `client`, changes not yet sent are sent first; any
/// left are dropped. Returns how many were dropped.
pub async fn disconnect(db: &Db, client: Option<&miniflux::Client>) -> Result<i64> {
    if db.call(|s| s.account()).await?.is_none() {
        return Err(not_connected());
    }
    let _turn = ONE_AT_A_TIME.lock().await;
    if let Some(client) = client {
        // Best effort: disconnecting must work with the server gone.
        let _ = send(db, client).await;
    }
    db.call(|s| s.end_account()).await
}

/// Bring the library and the server into step, as the [module](self) describes. A failure is
/// recorded on the account ([`crate::model::Account::error`]) as well as returned.
pub async fn sync(db: &Db, client: &miniflux::Client) -> Result<Report> {
    let _turn = ONE_AT_A_TIME.lock().await;
    sync_in_turn(db, client).await
}

/// [`sync`], once it is this caller's turn.
async fn sync_in_turn(db: &Db, client: &miniflux::Client) -> Result<Report> {
    let Some(account) = db.call(|s| s.account()).await? else {
        return Err(not_connected());
    };
    if same_url(&account.server) != same_url(client.server()) {
        return Err(Error::Validation(format!(
            "The library is connected to {}, not {}",
            account.server,
            client.server()
        ))
        .into());
    }
    let result = sync_now(db, client, account.last_sync.is_none()).await;
    let now = chrono::Utc::now().timestamp();
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    db.call(move |s| s.record_sync(now, error.as_deref()))
        .await?;
    result
}

fn not_connected() -> anyhow::Error {
    Error::Validation("No Miniflux account is connected".into()).into()
}

/// Add the subscriptions and folders of an OPML document to the library, as
/// [`crate::Store::import`] does, and sync to subscribe the server to them, each in the
/// category named like the folder it is in (made if need be). A feed the server refuses stays,
/// showing why, as when connecting. Returns what the import did, and the sync's report or why
/// it failed; the next sync sends what this one could not.
pub async fn import(db: &Db, client: &miniflux::Client, text: String) -> Result<Imported> {
    let _turn = ONE_AT_A_TIME.lock().await;
    if db.call(|s| s.account()).await?.is_none() {
        return Err(not_connected());
    }
    let report = db.call(move |s| s.import_now(&text)).await?;
    let synced = sync_in_turn(db, client).await;
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

/// Subscribe the server to the feed at `url` in the category `folder` is (or the "All"
/// category Miniflux makes, for the top level), named `title` unless that is empty, and sync
/// to bring it and its entries into the library; returns its feed there. Fails with
/// [`Error::DuplicateFeed`] if the library has the address, and with [`Error::Validation`]
/// for a folder of one's own, which the server knows nothing of.
pub async fn subscribe(
    db: &Db,
    client: &miniflux::Client,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<FeedId> {
    let _turn = ONE_AT_A_TIME.lock().await;
    let url = crate::util::validate_url(&url)?;
    let checked = url.clone();
    db.call(move |s| s.require_new_url(&checked, None)).await?;
    let category = category_for(db, client, folder).await?;
    let remote = client.create_feed(&url, category).await?;
    if !title.trim().is_empty() {
        let change = miniflux::FeedChange {
            title: Some(title.trim().to_owned()),
            ..Default::default()
        };
        client.update_feed(remote, &change).await?;
    }
    sync_in_turn(db, client).await?;
    db.call(move |s| s.feed_for_remote(remote))
        .await?
        .ok_or_else(|| anyhow!("Miniflux subscribed to {url} but does not list it"))
}

/// Rename the feed `id`, move it to the category `folder` is, or change its address: on the
/// server, then in the library, by a sync. A feed the server would not take is changed only
/// in the library. Fails as [`subscribe`] does.
pub async fn update_feed(
    db: &Db,
    client: &miniflux::Client,
    id: FeedId,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<()> {
    let _turn = ONE_AT_A_TIME.lock().await;
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
    let change = miniflux::FeedChange {
        title: (!title.is_empty() && title != feed.title).then(|| title.clone()),
        feed_url: (same_url(&url) != same_url(&feed.url)).then(|| url.clone()),
        category_id: match folder == feed.folder {
            true => None,
            false => Some(category_for(db, client, folder).await?),
        },
    };
    if change == miniflux::FeedChange::default() {
        return Ok(());
    }
    client.update_feed(remote, &change).await?;
    sync_in_turn(db, client).await.map(drop)
}

/// Unsubscribe from the feed `id` on the server, and remove it and its articles from the
/// library. A feed the server would not take is removed only from the library.
pub async fn unsubscribe(db: &Db, client: &miniflux::Client, id: FeedId) -> Result<()> {
    let _turn = ONE_AT_A_TIME.lock().await;
    if let Some(remote) = db.call(move |s| s.feed_remote_id(id)).await? {
        client.delete_feed(remote).await?;
    }
    db.call(move |s| s.delete_feed_now(id)).await
}

/// Rename the category the folder `id` is, on the server and in the library. Fails with
/// [`Error::Validation`] for a folder of one's own, which [`crate::Store::rename_folder`]
/// renames.
pub async fn rename_category(
    db: &Db,
    client: &miniflux::Client,
    id: FolderId,
    name: String,
) -> Result<()> {
    let _turn = ONE_AT_A_TIME.lock().await;
    let remote = remote_category(db, id).await?;
    let name = name.trim().to_owned();
    client.update_category(remote, &name).await?;
    db.call(move |s| s.rename_folder_now(id, &name)).await
}

/// Remove the category the folder `id` is, on the server and from the library; folders
/// inside it move up, as [`crate::Store::delete_folder`] moves them. The server would delete
/// the category's feeds with it, so this fails with [`Error::Validation`] while it has any,
/// and for a folder of one's own.
pub async fn remove_category(db: &Db, client: &miniflux::Client, id: FolderId) -> Result<()> {
    let _turn = ONE_AT_A_TIME.lock().await;
    let remote = remote_category(db, id).await?;
    if db.call(move |s| s.remote_feeds_in(id)).await? > 0 {
        return Err(Error::Validation(
            "Miniflux would remove this category's feeds with it; move or remove them first".into(),
        )
        .into());
    }
    client.delete_category(remote).await?;
    db.call(move |s| s.immediate(|s| s.delete_folder_now(id)))
        .await
}

/// The category the folder `id` is.
async fn remote_category(db: &Db, id: FolderId) -> Result<i64> {
    db.call(move |s| s.folder_remote_id(id))
        .await?
        .ok_or_else(|| {
            Error::Validation("This folder is your own, not a Miniflux category".into()).into()
        })
}

/// The category for feeds put in `folder`: the one it is, or [`DEFAULT_CATEGORY`] (made if
/// need be) for the top level.
async fn category_for(db: &Db, client: &miniflux::Client, folder: Option<FolderId>) -> Result<i64> {
    match folder {
        Some(id) => db
            .call(move |s| s.folder_remote_id(id))
            .await?
            .ok_or_else(|| {
                Error::Validation(
                "Miniflux keeps feeds in its categories, and this folder is your own; choose one \
                 of its categories"
                    .into(),
            )
            .into()
            }),
        None => {
            let mut categories = client.categories().await?;
            category_named(client, &mut categories, DEFAULT_CATEGORY).await
        }
    }
}

/// The category titled `title` (ignoring case) among `categories`, made and added to them if
/// there is none.
async fn category_named(
    client: &miniflux::Client,
    categories: &mut Vec<miniflux::Category>,
    title: &str,
) -> Result<i64> {
    if let Some(c) = categories
        .iter()
        .find(|c| c.title.eq_ignore_ascii_case(title))
    {
        return Ok(c.id);
    }
    let created = client.create_category(title).await?;
    let id = created.id;
    categories.push(created);
    Ok(id)
}

async fn sync_now(db: &Db, client: &miniflux::Client, first: bool) -> Result<Report> {
    let mut report = Report {
        sent: send(db, client).await?,
        ..Report::default()
    };
    subscribe_local(db, client, &mut report).await?;

    let categories = client.categories().await?;
    let feeds = client.feeds().await?;
    let unread_entries = client
        .all_entries(&miniflux::EntryQuery {
            read: Some(false),
            ..Default::default()
        })
        .await?;
    let starred_entries = client
        .all_entries(&miniflux::EntryQuery {
            starred: Some(true),
            ..Default::default()
        })
        .await?;
    let newest = db.call(|s| s.newest_entry()).await?.filter(|_| !first);
    let now = chrono::Utc::now().timestamp();
    let recent = client
        .all_entries(&miniflux::EntryQuery {
            after_entry_id: newest,
            published_after: Some(now - RECENT_DAYS * 86_400),
            ..Default::default()
        })
        .await?;
    let unread = unread_entries.iter().map(|e| e.id).collect::<HashSet<_>>();
    let starred = starred_entries.iter().map(|e| e.id).collect::<HashSet<_>>();
    let mut entries = unread_entries;
    entries.extend(starred_entries);
    entries.extend(recent);
    report.feeds = feeds.len();
    let remote = crate::store::account::Remote {
        categories,
        feeds,
        entries,
        unread,
        starred,
        now,
    };
    let applied = db.call(move |s| s.apply(&remote)).await?;
    report.new_articles = applied.new_articles;
    report.state_changes = applied.state_changes;
    report.sent += send(db, client).await?;
    Ok(report)
}

/// Send the outbox, returning how many changes the server took.
async fn send(db: &Db, client: &miniflux::Client) -> Result<usize> {
    let unsent = db.call(|s| s.unsent()).await?;
    if unsent.is_empty() {
        return Ok(0);
    }
    let (reads, stars): (Vec<_>, Vec<_>) = unsent.iter().partition(|u| u.field == "read");
    let ids = |read: bool| -> Vec<i64> {
        reads
            .iter()
            .filter(|u| u.value == read)
            .map(|u| u.entry)
            .collect()
    };
    client.mark_read(&ids(true), true).await?;
    client.mark_read(&ids(false), false).await?;
    if !stars.is_empty() {
        let starred = client
            .all_entries(&miniflux::EntryQuery {
                starred: Some(true),
                ..Default::default()
            })
            .await?
            .into_iter()
            .map(|e| e.id)
            .collect::<HashSet<_>>();
        for star in &stars {
            if starred.contains(&star.entry) != star.value {
                client.toggle_star(star.entry).await?;
            }
        }
    }
    let count = unsent.len();
    db.call(move |s| s.sent(&unsent)).await?;
    Ok(count)
}

/// Subscribe the server to the library's feeds it does not have, in the category named like
/// each feed's folder (made if need be), or [`DEFAULT_CATEGORY`] at the top level.
async fn subscribe_local(db: &Db, client: &miniflux::Client, report: &mut Report) -> Result<()> {
    let remote_urls = client
        .feeds()
        .await?
        .iter()
        .map(|f| same_url(&f.feed_url))
        .collect::<HashSet<_>>();
    let local = db.call(move |s| s.local_feeds(&remote_urls)).await?;
    if local.is_empty() {
        return Ok(());
    }
    let mut categories = client.categories().await?;
    for feed in local {
        let wanted = feed.folder.as_deref().unwrap_or(DEFAULT_CATEGORY);
        let category = category_named(client, &mut categories, wanted).await?;
        match client.create_feed(&feed.url, category).await {
            Ok(_) => report.subscribed += 1,
            Err(e) => {
                let reason = format!("Miniflux would not subscribe: {e:#}");
                let id = feed.id;
                let error = reason.clone();
                db.call(move |s| s.set_feed_error(id, &error)).await?;
                report.refused.push((feed.title, reason));
            }
        }
    }
    Ok(())
}
