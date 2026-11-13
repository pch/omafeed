//! The full article, for feeds that give only a summary: fetched from the article's web page
//! on request, with its main text found the way Firefox's Reader View finds it, and kept
//! beside the feed's version, which a refresh may still update. While a Miniflux account is
//! connected, the server fetches its entries instead if it can, as Miniflux does with the
//! scraper rules and cookies set for each feed there.
use crate::{
    Db, Error, content,
    http::{self, Body},
    model::ArticleId,
    sync::Backend,
    util::may_request,
};
use anyhow::{Context, Result, anyhow, bail};
use url::Url;

/// Fetch the article `id` in full and keep it, so that [`crate::model::Article::html`] is the
/// full article from now on. With the `backend` of a connected account, an article that is an
/// entry there is fetched by the server, if the service can ([`Backend::fetch_content`]). Fails, keeping the feed's version, if the
/// article has no web page or no article text is found on it.
pub async fn fetch(db: &Db, backend: Option<&dyn Backend>, id: ArticleId) -> Result<()> {
    let (url, feed_url, entry) = db.call(move |s| s.full_article_source(id)).await?;
    if url.is_empty() {
        return Err(Error::Validation("This article has no web page to fetch".into()).into());
    }
    let fetched = match (backend, entry) {
        (Some(backend), Some(entry)) => backend.fetch_content(entry).await?,
        _ => None,
    };
    let html = match fetched {
        Some(html) => html,
        None => download(&url, &feed_url).await?,
    };
    let html = content::sanitize(&html, &url);
    if content::plain(&html).trim().is_empty() {
        bail!("No article text found at {url}");
    }
    db.call(move |s| s.set_full_article(id, Some(&html))).await
}

/// Go back to the feed's version of the article `id`.
pub async fn forget(db: &Db, id: ArticleId) -> Result<()> {
    db.call(move |s| s.set_full_article(id, None)).await
}

/// The main text of the web page at `url`, as HTML. A page on the local network is fetched
/// only for a feed that is there too.
async fn download(url: &str, feed_url: &str) -> Result<String> {
    let page = Url::parse(url)?;
    let feed = Url::parse(feed_url)?;
    if !may_request(&feed, &page) {
        bail!("A feed on the internet may not lead omafeed to a local address");
    }
    let response = http::client()?
        .get(page)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("Could not fetch {url}"))?;
    let body = http::read_limited(response, http::BODY_LIMIT, Body::Whole).await?;
    let page = String::from_utf8_lossy(&body).into_owned();
    let url = url.to_owned();
    // Finding the text takes a while on a large page; keep it off the async threads.
    tokio::task::spawn_blocking(move || extract(&page, &url)).await?
}

/// The main text of the web page `page`, found at `url`, as HTML.
fn extract(page: &str, url: &str) -> Result<String> {
    let article = dom_smoothie::Readability::new(page, Some(url), None)
        .and_then(|mut r| r.parse())
        .map_err(|e| anyhow!("No article text found at {url}: {e}"))?;
    Ok(article.content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Download, StatePatch, Update};

    const PAGE: &str = r#"<!doctype html><html><head><title>A post</title></head><body>
        <nav><a href="/">Home</a> <a href="/about">About</a></nav>
        <article><h1>A post</h1>
        <p>The first paragraph of the post says enough to be worth reading in full, with a
        <a href="/more">relative link</a> and more words, so that it is clearly the text.</p>
        <p>The second paragraph goes on, at length, about the same subject, because a page's
        main text is the part with the most prose in it, and this is that part of the page.</p>
        <p>A third paragraph closes it, long enough again to leave no doubt where it is.</p>
        </article><footer>Copyright and other things nobody reads</footer></body></html>"#;

    #[test]
    fn the_main_text_is_found_without_the_pages_furniture() {
        let html = extract(PAGE, "https://blog.example/post").unwrap();
        assert!(html.contains("The second paragraph goes on"));
        assert!(!html.contains("nobody reads"));
        assert!(!html.contains("About"));
    }

    /// Store a download of the feed with one item linking to `page_url`, whose text is
    /// `summary`.
    fn download_item(s: &crate::Store, page_url: &str, summary: &str) -> Result<()> {
        let feed = s.library()?.feeds[0].clone();
        let entries = crate::ingest::parse(
            format!(
                "<rss version=\"2.0\"><channel><title>T</title><item><guid>1</guid>\
                 <title>A post</title><link>{page_url}</link>\
                 <description>{summary}</description></item></channel></rss>"
            )
            .as_bytes(),
            &feed.url,
        )?;
        let download = Download::Updated(Update {
            site_url: None,
            entries,
            etag: None,
            modified: None,
        });
        s.commit_download(feed.id, feed.revision, download, 0, 0)
    }

    async fn library_with_article(page_url: &str) -> (tempfile::TempDir, Db, ArticleId) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        let url = page_url.to_owned();
        let id = db
            .call(move |s| {
                s.add_feed("Blog", "http://127.0.0.1/feed", None)?;
                download_item(s, &url, "Summary")?;
                Ok(s.articles(&crate::model::Query::default())?[0].id)
            })
            .await
            .unwrap();
        (dir, db, id)
    }

    #[tokio::test]
    async fn the_full_article_replaces_the_summary_until_forgotten() {
        let (addr, _) = http::tests::server(|_| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{PAGE}",
                PAGE.len()
            )
        })
        .await;
        let (_dir, db, id) = library_with_article(&format!("http://{addr}/post")).await;
        fetch(&db, None, id).await.unwrap();
        let a = db.call(move |s| s.article(id)).await.unwrap();
        assert!(a.full);
        assert!(a.html.contains("The second paragraph goes on"));
        assert!(a.html.contains(&format!("href=\"http://{addr}/more\"")));
        // A refresh updates the feed's version, beside the full article.
        let page = format!("http://{addr}/post");
        db.call(move |s| download_item(s, &page, "Summary, edited"))
            .await
            .unwrap();
        assert!(db.call(move |s| s.article(id)).await.unwrap().full);
        // Read and star state are the article's still.
        db.call(move |s| s.set_state(id, StatePatch::read(true)))
            .await
            .unwrap();
        forget(&db, id).await.unwrap();
        let a = db.call(move |s| s.article(id)).await.unwrap();
        assert!(!a.full && a.read);
        assert_eq!(a.html, "Summary, edited");
    }

    #[tokio::test]
    async fn a_page_without_text_or_a_feed_on_the_internet_leaves_the_summary() {
        let (addr, _) = http::tests::server(|_| {
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\n\r\n<html></html>"
                .into()
        })
        .await;
        let (_dir, db, id) = library_with_article(&format!("http://{addr}/empty")).await;
        assert!(fetch(&db, None, id).await.is_err());
        let a = db.call(move |s| s.article(id)).await.unwrap();
        assert!(!a.full);
        assert!(
            download("http://127.0.0.1/post", "https://blog.example/feed")
                .await
                .is_err()
        );
        let (_dir, db, id) = library_with_article("").await;
        assert!(fetch(&db, None, id).await.is_err());
    }
}
