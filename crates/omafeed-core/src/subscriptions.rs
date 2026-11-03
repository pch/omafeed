//! Subscribing, editing and unsubscribing: what the command line and the window both do with
//! feeds. Finding the feed (discovery) comes first and is up to the caller, which decides
//! which candidate to take.
use crate::{
    Db, Error,
    model::{FeedId, FolderId},
};
use anyhow::Result;

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
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<FeedId> {
    db.call(move |s| s.add_feed(&title, &url, folder)).await
}

/// Rename a subscription, move it to `folder`, or change its URL, which forgets what was
/// learned from the old one. Fails with [`Error::DuplicateFeed`] if another subscription
/// already has the new URL.
pub async fn update(
    db: &Db,
    id: FeedId,
    title: String,
    url: String,
    folder: Option<FolderId>,
) -> Result<()> {
    db.call(move |s| s.update_feed(id, &title, &url, folder))
        .await
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
pub async fn unsubscribe(db: &Db, id: FeedId, confirmed: bool) -> Result<Removal> {
    db.call(move |s| {
        s.immediate(|s| {
            let lib = s.library()?;
            let feed = lib.feed(id).ok_or(Error::FeedNotFound(id))?;
            let (articles, starred) = s.feed_article_counts(id)?;
            if confirmed {
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
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriptions_are_added_edited_reported_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        let url = "https://example.org/feed".to_string();
        let id = subscribe(&db, "Feed".into(), url.clone(), None)
            .await
            .unwrap();
        let again = subscribe(&db, "Again".into(), url.clone(), None).await;
        assert!(matches!(
            again.unwrap_err().downcast_ref::<Error>(),
            Some(Error::DuplicateFeed(_))
        ));
        assert_eq!(
            subscribed_title(&db, url.clone()).await.unwrap().as_deref(),
            Some("Feed")
        );
        let other = subscribe(&db, String::new(), "https://example.org/other".into(), None)
            .await
            .unwrap();
        let moved = update(&db, other, "Other".into(), url.clone(), None).await;
        assert!(matches!(
            moved.unwrap_err().downcast_ref::<Error>(),
            Some(Error::DuplicateFeed(_))
        ));
        let report = unsubscribe(&db, id, false).await.unwrap();
        assert_eq!((report.title.as_str(), report.articles), ("Feed", 0));
        assert!(subscribed_title(&db, url.clone()).await.unwrap().is_some());
        assert_eq!(unsubscribe(&db, id, true).await.unwrap(), report);
        assert!(subscribed_title(&db, url).await.unwrap().is_none());
        let gone = unsubscribe(&db, id, true).await.unwrap_err();
        assert_eq!(gone.to_string(), format!("No feed with id {id}"));
    }
}
