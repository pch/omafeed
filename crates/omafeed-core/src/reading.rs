//! Reading: marking articles read, unread, starred or not, one at a time or a whole view at
//! once, as both the command line and the window do.
use crate::{
    Db,
    model::{ArticleId, Query, StatePatch},
};
use anyhow::Result;

/// Change every article in `ids` as `patch` says, or none if one of them does not exist
/// ([`crate::Error::ArticleNotFound`]). Returns each article changed with its title, in ID
/// order.
pub async fn change(
    db: &Db,
    ids: Vec<ArticleId>,
    patch: StatePatch,
) -> Result<Vec<(ArticleId, String)>> {
    db.call(move |s| s.set_article_states(&ids, patch)).await
}

/// Change one article as `patch` says. An article deleted meanwhile is left alone, without
/// an error, since there is nothing left to show it on.
pub async fn change_one(db: &Db, id: ArticleId, patch: StatePatch) -> Result<()> {
    db.call(move |s| s.set_state(id, patch)).await
}

/// Mark every unread article in a view read, returning those it changed, for [`undo_read`].
pub async fn mark_view_read(db: &Db, view: Query) -> Result<Vec<ArticleId>> {
    db.call(move |s| s.mark_read(&view)).await
}

/// Mark unread again the articles a [`mark_view_read`] changed.
pub async fn undo_read(db: &Db, ids: Vec<ArticleId>) -> Result<()> {
    db.call(move |s| s.undo_read(&ids)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Download, Scope, Update};

    #[tokio::test]
    async fn views_are_marked_read_and_back_and_single_changes_ignore_deleted_articles() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        let ids = db
            .call(|s| {
                let feed = s.add_feed("Feed", "https://example.org/feed", None)?;
                let revision = s.library()?.feeds[0].revision;
                let entries = crate::ingest::parse(
                    br#"<rss version="2.0"><channel><title>T</title><item><guid>1</guid><title>One</title></item><item><guid>2</guid><title>Two</title></item></channel></rss>"#,
                    "https://example.org/feed",
                )?;
                let download = Download::Updated(Update {
                    site_url: None,
                    entries,
                    etag: None,
                    modified: None,
                });
                s.commit_download(feed, revision, download, 0, 0)?;
                let all = Query {
                    scope: Scope::All,
                    ..Default::default()
                };
                Ok(s.articles(&all)?.into_iter().map(|a| a.id).collect::<Vec<_>>())
            })
            .await
            .unwrap();
        let unread = Query::default();
        let marked = mark_view_read(&db, unread.clone()).await.unwrap();
        assert_eq!(marked.len(), 2);
        assert!(
            mark_view_read(&db, unread.clone())
                .await
                .unwrap()
                .is_empty()
        );
        undo_read(&db, marked).await.unwrap();
        let changed = change(&db, vec![ids[1], ids[0]], StatePatch::starred(true))
            .await
            .unwrap();
        assert_eq!(changed.len(), 2);
        assert!(
            change(&db, vec![ids[0], ArticleId(404)], StatePatch::read(true))
                .await
                .is_err()
        );
        change_one(&db, ArticleId(404), StatePatch::read(true))
            .await
            .unwrap();
        assert_eq!(mark_view_read(&db, unread).await.unwrap().len(), 2);
    }
}
