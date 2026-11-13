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
pub mod miniflux;

use crate::{Db, Error, store::account::same_url};
use anyhow::Result;
use std::collections::HashSet;

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
    if let Some(client) = client {
        // Best effort: disconnecting must work with the server gone.
        let _ = send(db, client).await;
    }
    db.call(|s| s.end_account()).await
}

/// Bring the library and the server into step, as the [module](self) describes. A failure is
/// recorded on the account ([`crate::model::Account::error`]) as well as returned.
pub async fn sync(db: &Db, client: &miniflux::Client) -> Result<Report> {
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
        let category = match categories
            .iter()
            .find(|c| c.title.eq_ignore_ascii_case(wanted))
        {
            Some(c) => c.id,
            None => {
                let created = client.create_category(wanted).await?;
                let id = created.id;
                categories.push(created);
                id
            }
        };
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
