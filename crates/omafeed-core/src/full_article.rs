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
/// entry there is fetched by the server, if the service can ([`Backend::fetch_content`]), and
/// by omafeed if the server fails. Fails, keeping the feed's version, if the article has no
/// web page or no article text is found on it.
pub async fn fetch(db: &Db, backend: Option<&dyn Backend>, id: ArticleId) -> Result<()> {
    let (url, feed_url, entry) = db.call(move |s| s.full_article_source(id)).await?;
    if url.is_empty() {
        return Err(Error::Validation("This article has no web page to fetch".into()).into());
    }
    let mut server_error = None;
    if let (Some(backend), Some(entry)) = (backend, entry) {
        match backend.fetch_content(entry).await {
            Ok(Some(html)) => {
                let html = content::sanitize(&html, &url);
                if has_text(&html) {
                    return db.call(move |s| s.set_full_article(id, Some(&html))).await;
                }
            }
            Ok(None) => {}
            // The server may be out of reach, or unable to read the site; try the page itself.
            Err(e) => server_error = Some(e),
        }
    }
    let html = match download(&url, &feed_url).await {
        Ok((html, base)) => content::sanitize(&html, &base),
        Err(e) => match server_error {
            Some(server) => bail!("{server:#}; fetching the page directly failed too: {e:#}"),
            None => return Err(e),
        },
    };
    if !has_text(&html) {
        bail!("No article text found at {url}");
    }
    db.call(move |s| s.set_full_article(id, Some(&html))).await
}

/// Go back to the feed's version of the article `id`.
pub async fn forget(db: &Db, id: ArticleId) -> Result<()> {
    db.call(move |s| s.set_full_article(id, None)).await
}

fn has_text(html: &str) -> bool {
    !content::plain(html).trim().is_empty()
}

/// The main text of the web page at `url`, as HTML, and the address it was served from after
/// any redirects, which its relative links are relative to. A page on the local network is
/// fetched only for a feed that is there too.
async fn download(url: &str, feed_url: &str) -> Result<(String, String)> {
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
    let base = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(mime) = content_type.as_deref().and_then(|t| t.split(';').next()) {
        let mime = mime.trim().to_ascii_lowercase();
        if mime != "text/html" && mime != "application/xhtml+xml" {
            bail!("{url} is not a web page ({mime})");
        }
    }
    let body = http::read_limited(response, http::BODY_LIMIT, Body::Whole).await?;
    // Finding the text takes a while on a large page; keep it off the async threads.
    tokio::task::spawn_blocking(move || {
        let page = decode(&body, content_type.as_deref());
        extract(&page, &base).map(|html| (html, base))
    })
    .await?
}

/// The text of a web page, in the character set its `Content-Type` header or its own
/// `<meta>` declares (a byte order mark taking precedence), or else UTF-8.
fn decode(body: &[u8], content_type: Option<&str>) -> String {
    let label = content_type
        .and_then(charset)
        .or_else(|| charset(&String::from_utf8_lossy(&body[..body.len().min(1024)])));
    let encoding = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(body).0.into_owned()
}

/// The value of the first `charset=` in `text`, as in a `Content-Type` header, or a `<meta>`
/// tag's `charset` or `content` attribute.
fn charset(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("charset=")? + "charset=".len();
    let value: String = lower[start..]
        .trim_start_matches(['"', '\'', ' '])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    (!value.is_empty()).then_some(value)
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

    #[test]
    fn a_page_is_read_in_the_character_set_it_declares() {
        // "Zażółć" in ISO-8859-2.
        let latin2 = b"<p>Za\xbf\xf3\xb3\xe6</p>";
        assert_eq!(
            decode(latin2, Some("text/html; charset=ISO-8859-2")),
            "<p>Zażółć</p>"
        );
        let mut page = b"<html><head><meta charset=\"iso-8859-2\"></head>".to_vec();
        page.extend_from_slice(latin2);
        assert!(decode(&page, Some("text/html")).ends_with("<p>Zażółć</p>"));
        let http_equiv =
            br#"<meta http-equiv="Content-Type" content="text/html; charset=windows-1250">"#;
        assert_eq!(
            charset(&String::from_utf8_lossy(http_equiv)).as_deref(),
            Some("windows-1250")
        );
        assert_eq!(decode("<p>Zażółć</p>".as_bytes(), None), "<p>Zażółć</p>");
    }

    #[tokio::test]
    async fn links_are_relative_to_the_page_a_redirect_leads_to() {
        let (addr, _) = http::tests::server(|line| {
            if line.starts_with("GET /go ") {
                "HTTP/1.1 302 Found\r\nLocation: /posts/a\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            } else {
                http::tests::ok("text/html; charset=utf-8", &PAGE.replace("/more", "more"))
            }
        })
        .await;
        let (_dir, db, id) = library_with_article(&format!("http://{addr}/go")).await;
        fetch(&db, None, id).await.unwrap();
        let a = db.call(move |s| s.article(id)).await.unwrap();
        assert!(
            a.html
                .contains(&format!("href=\"http://{addr}/posts/more\""))
        );
    }

    #[tokio::test]
    async fn the_page_is_fetched_directly_when_the_server_fails_to() {
        let (page, _) = http::tests::server(|_| http::tests::ok("text/html", PAGE)).await;
        let (server, log) = http::tests::server(|_| {
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into()
        })
        .await;
        let (dir, db, id) = library_with_article(&format!("http://{page}/post")).await;
        // The article is an entry of the connected account.
        rusqlite::Connection::open(dir.path().join("test.db"))
            .unwrap()
            .execute("UPDATE articles SET remote_id = 42 WHERE id = ?1", [id])
            .unwrap();
        let client =
            crate::sync::miniflux::Client::new(&format!("http://{server}"), "key").unwrap();
        fetch(&db, Some(&client), id).await.unwrap();
        assert!(log.lock().unwrap()[0].contains("/v1/entries/42/fetch-content"));
        let a = db.call(move |s| s.article(id)).await.unwrap();
        assert!(a.full && a.html.contains("The second paragraph goes on"));
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_web_page_is_refused() {
        let (addr, _) =
            http::tests::server(|_| http::tests::ok("application/pdf", "%PDF-1.7")).await;
        let (_dir, db, id) = library_with_article(&format!("http://{addr}/paper.pdf")).await;
        let error = fetch(&db, None, id).await.unwrap_err();
        assert!(format!("{error:#}").contains("not a web page"));
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
