//! Subscribing, importing, editing and unsubscribing: what the command line and the window both do with
//! feeds. Finding the feed (discovery) comes first and is up to the caller, which decides
//! which candidate to take. While an account is connected, each takes its `backend` and
//! makes the change on the server too ([`crate::sync`]); without one it fails with
//! [`Error::Managed`] then.
use crate::{
    Db, Error,
    model::{FeedId, FolderId, ImportReport},
    sync::{self, Backend},
};
use anyhow::Result;

/// `client` if an account is connected, `None` if the library is local, and
/// [`Error::Managed`] if one is connected but there is no client for it.
async fn connected<'a>(
    db: &Db,
    client: Option<&'a dyn Backend>,
) -> Result<Option<&'a dyn Backend>> {
    if db.call(|s| s.account()).await?.is_none() {
        return Ok(None);
    }
    client.map(Some).ok_or_else(|| Error::Managed.into())
}

/// A feed removed by [`unsubscribe`], or that would be without confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    /// The feed's ID, no longer in use once it is removed.
    pub id: FeedId,
    /// Its title.
    pub title: String,
    /// Its feed address.
    pub url: String,
    /// Its stored articles, starred ones included.
    pub articles: i64,
    /// How many of those are starred.
    pub starred: i64,
}

/// Subscribe to the feed at `url` as `title` (the URL if empty), in `folder`. Fails with
/// [`Error::DuplicateFeed`] if a subscription already has that URL.
pub async fn subscribe(
    db: &Db,
    client: Option<&dyn Backend>,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<FeedId> {
    if let Some(client) = connected(db, client).await? {
        return sync::subscribe(db, client, title, url, folder).await;
    }
    db.call(move |s| s.add_feed(&title, &url, folder)).await
}

/// Rename a subscription, move it to `folder`, or change its URL, which forgets what was
/// learned from the old one. Fails with [`Error::DuplicateFeed`] if another subscription
/// already has the new URL.
pub async fn update(
    db: &Db,
    client: Option<&dyn Backend>,
    id: FeedId,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<()> {
    if let Some(client) = connected(db, client).await? {
        return sync::update_feed(db, client, id, title, url, folder).await;
    }
    db.call(move |s| s.update_feed(id, &title, &url, folder))
        .await
}

/// What [`import`] did.
#[derive(Debug)]
pub struct Imported {
    /// The subscriptions added, skipped, and refused.
    pub report: ImportReport,
    /// While a Miniflux account is connected, the sync that sent the subscriptions to it, or
    /// why it failed; `None` for a local library, whose new feeds are due for a refresh.
    pub synced: Option<Result<sync::Report>>,
}

/// Add the subscriptions and folders of an OPML document, as [`crate::Store::import`] does;
/// while connected, Miniflux is subscribed to them too ([`sync::import`]).
pub async fn import(db: &Db, client: Option<&dyn Backend>, text: String) -> Result<Imported> {
    if let Some(client) = connected(db, client).await? {
        let done = sync::import(db, client, text).await?;
        return Ok(Imported {
            report: done.report,
            synced: Some(done.synced),
        });
    }
    let report = db.call(move |s| s.import(&text)).await?;
    Ok(Imported {
        report,
        synced: None,
    })
}

/// The title of the subscription with feed URL `url`, if there is one; for saying which
/// feed a [`Error::DuplicateFeed`] meant.
pub async fn subscribed_title(db: &Db, url: String) -> Result<Option<String>> {
    db.call(move |s| {
        Ok(s.library()?
            .feeds
            .into_iter()
            .find(|f| f.url == url)
            .map(|f| f.title))
    })
    .await
}

/// Remove a subscription with all its articles, starred ones included, if `confirmed`;
/// otherwise only report what would go. Either way the report is exactly what is (or would
/// be) deleted. Fails with [`Error::FeedNotFound`] if there is no such feed.
pub async fn unsubscribe(
    db: &Db,
    client: Option<&dyn Backend>,
    id: FeedId,
    confirmed: bool,
) -> Result<Removal> {
    // Only removing needs the server.
    let server = match confirmed {
        true => connected(db, client).await?,
        false => None,
    };
    let local = confirmed && server.is_none();
    let removal = db
        .call(move |s| {
            s.immediate(|s| {
                let lib = s.library()?;
                let feed = lib.feed(id).ok_or(Error::FeedNotFound(id))?;
                let (articles, starred) = s.feed_article_counts(id)?;
                if local {
                    s.delete_feed(id)?;
                }
                Ok(Removal {
                    id,
                    title: feed.title.clone(),
                    url: feed.url.clone(),
                    articles,
                    starred,
                })
            })
        })
        .await?;
    if let (true, Some(client)) = (confirmed, server) {
        sync::unsubscribe(db, client, id).await?;
    }
    Ok(removal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriptions_are_added_edited_reported_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        let url = "https://example.org/feed".to_string();
        let id = subscribe(&db, None, "Feed".into(), url.clone(), None)
            .await
            .unwrap();
        let again = subscribe(&db, None, "Again".into(), url.clone(), None).await;
        assert!(matches!(
            again.unwrap_err().downcast_ref::<Error>(),
            Some(Error::DuplicateFeed(_))
        ));
        assert_eq!(
            subscribed_title(&db, url.clone()).await.unwrap().as_deref(),
            Some("Feed")
        );
        let other = subscribe(
            &db,
            None,
            String::new(),
            "https://example.org/other".into(),
            None,
        )
        .await
        .unwrap();
        let moved = update(&db, None, other, "Other".into(), url.clone(), None).await;
        assert!(matches!(
            moved.unwrap_err().downcast_ref::<Error>(),
            Some(Error::DuplicateFeed(_))
        ));
        let report = unsubscribe(&db, None, id, false).await.unwrap();
        assert_eq!((report.title.as_str(), report.articles), ("Feed", 0));
        assert!(subscribed_title(&db, url.clone()).await.unwrap().is_some());
        assert_eq!(unsubscribe(&db, None, id, true).await.unwrap(), report);
        assert!(subscribed_title(&db, url).await.unwrap().is_none());
        let gone = unsubscribe(&db, None, id, true).await.unwrap_err();
        assert_eq!(gone.to_string(), format!("No feed with id {id}"));
    }
}
