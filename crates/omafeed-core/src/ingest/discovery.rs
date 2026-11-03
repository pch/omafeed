//! Resolve website subscriptions before adding anything to the library.
use crate::{
    http::{BODY_LIMIT, Body, read_limited},
    util::validate_url,
};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use reqwest::Client;
use scraper::{Html, Selector};
use url::Url;

#[derive(Debug)]
pub struct Candidate {
    pub title: String,
    pub url: String,
}

async fn get(client: &Client, url: &str) -> Result<(String, Vec<u8>)> {
    let response = client.get(url).send().await?.error_for_status()?;
    let base = response.url().to_string();
    Ok((base, read_limited(response, BODY_LIMIT, Body::Whole).await?))
}
fn candidate(data: &[u8], url: &str) -> Result<Candidate> {
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(url))
        .build()
        .parse(data)?;
    Ok(Candidate {
        title: feed
            .title
            .map(|t| crate::content::plain(&t.content))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| url.to_owned()),
        url: url.to_owned(),
    })
}
/// Discover advertised RSS, Atom and JSON Feed links, resolving redirects and HTML bases.
pub fn links(html: &str, base: &str) -> Vec<String> {
    let Ok(mut base) = Url::parse(base) else {
        return vec![];
    };
    let doc = Html::parse_document(html);
    if let Some(href) = doc
        .select(&Selector::parse("base[href]").expect("constant selector"))
        .next()
        .and_then(|e| e.value().attr("href"))
        && let Ok(url) = base.join(href)
    {
        base = url;
    }
    let mut urls = Vec::new();
    let selector = Selector::parse("link[href], a[href]").expect("constant selector");
    for e in doc.select(&selector) {
        let v = e.value();
        let mime = v
            .attr("type")
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let href = v.attr("href").unwrap_or_default();
        let advertised = matches!(
            mime.as_str(),
            "application/rss+xml"
                | "application/atom+xml"
                | "application/feed+json"
                | "application/json"
        ) && (v.name() == "a"
            || v.attr("rel")
                .unwrap_or_default()
                .split_ascii_whitespace()
                .any(|r| r.eq_ignore_ascii_case("alternate")));
        let conventional = v.name() == "a"
            && (href.ends_with(".rss")
                || href.ends_with(".atom")
                || href.ends_with("/rss/")
                || href.ends_with("/feed/")
                || href.ends_with("feed.xml"));
        if (advertised || conventional)
            && let Ok(url) = base.join(href)
            && let Ok(url) = validate_url(url.as_str())
            && !urls.contains(&url)
        {
            urls.push(url);
        }
    }
    urls.truncate(12);
    urls
}
pub async fn discover(client: &Client, input: &str) -> Result<Vec<Candidate>> {
    let url = validate_url(input).context("Enter a website or feed URL starting with https://")?;
    let (base, data) = get(client, &url).await.context("Could not open this URL")?;
    let fallback_base = base.clone();
    let (direct, mut urls) = tokio::task::spawn_blocking(move || {
        (
            candidate(&data, &base),
            links(&String::from_utf8_lossy(&data), &base),
        )
    })
    .await?;
    if let Ok(feed) = direct {
        return Ok(vec![feed]);
    }
    // A page on the internet must not send us to the local network.
    let page = Url::parse(&fallback_base)?;
    urls.retain(|u| Url::parse(u).is_ok_and(|u| crate::util::may_request(&page, &u)));
    if urls.is_empty() {
        let base = &page;
        for suffix in [
            "/feed/",
            "/rss/",
            "/feed.xml",
            "/atom.xml",
            "/index.xml",
            "/feed.json",
        ] {
            urls.push(base.join(suffix)?.to_string());
        }
    }
    let mut results = stream::iter(urls.into_iter().enumerate().map(|(index, url)| async move {
        let result = async {
            let (base, data) = get(client, &url).await?;
            tokio::task::spawn_blocking(move || candidate(&data, &base)).await?
        }
        .await;
        (index, result)
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    results.sort_by_key(|(index, _)| *index);
    let mut feeds: Vec<Candidate> = Vec::new();
    for (_, result) in results {
        if let Ok(feed) = result
            && !feeds.iter().any(|f| f.url == feed.url)
        {
            feeds.push(feed);
        }
    }
    if feeds.is_empty() {
        bail!(
            "No readable RSS, Atom, or JSON feed found at this website. Try its direct feed URL."
        );
    }
    Ok(feeds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::{client, ok, server};

    const RSS: &str = r#"<rss version="2.0"><channel><title>Found</title></channel></rss>"#;

    #[tokio::test]
    async fn a_remote_page_cannot_advertise_a_local_feed() {
        let (local, hits) = server(|_| ok("application/rss+xml", RSS)).await;
        let page = format!(
            r#"<html><head><link rel="alternate" type="application/rss+xml" href="http://{local}/feed"></head></html>"#
        );
        let (site, _) = server(move |_| ok("text/html", &page)).await;
        let client = client(&[("remote.test", site)]);
        let found = discover(&client, &format!("http://remote.test:{}/", site.port())).await;
        assert!(found.is_err(), "{found:?}");
        assert!(
            hits.lock().unwrap().is_empty(),
            "the local feed was requested"
        );
    }

    #[tokio::test]
    async fn a_local_page_may_advertise_a_local_feed() {
        let (local, _) = server(|_| ok("application/rss+xml", RSS)).await;
        let page = format!(
            r#"<html><head><link rel="alternate" type="application/rss+xml" href="http://{local}/feed"></head></html>"#
        );
        let (site, _) = server(move |_| ok("text/html", &page)).await;
        let found = discover(&client(&[]), &format!("http://{site}/"))
            .await
            .unwrap();
        assert_eq!(found[0].url, format!("http://{local}/feed"));
        assert_eq!(found[0].title, "Found");
    }
}
