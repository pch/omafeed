//! Turning feed documents into entries ready to store.
use crate::{
    content,
    model::Entry,
    util::{escape, resolve_http, sha256_hex, validate_url},
};
use anyhow::Result;

/// The articles in an RSS, Atom or JSON feed document served from `base`.
pub fn parse(data: &[u8], base: &str) -> Result<Vec<Entry>> {
    Ok(parse_document(data, base)?.0)
}

fn alternate(links: &[feed_rs::model::Link]) -> Option<&feed_rs::model::Link> {
    links
        .iter()
        .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
}

/// Attachment URLs: Atom `rel="enclosure"` links, plus RSS enclosures and Media RSS
/// audio/video, which feed-rs reports as media objects.
fn attachments(e: &feed_rs::model::Entry, base: &str) -> Vec<String> {
    let links = e
        .links
        .iter()
        .filter(|l| l.rel.as_deref() == Some("enclosure"))
        .map(|l| l.href.clone());
    let media = e
        .media
        .iter()
        .flat_map(|m| &m.content)
        .filter(|c| {
            c.content_type.as_ref().is_some_and(|t| {
                let t = t.to_string();
                t.starts_with("audio/") || t.starts_with("video/")
            })
        })
        .filter_map(|c| c.url.as_ref().map(|u| u.to_string()));
    let mut urls = Vec::new();
    for url in links.chain(media).filter_map(|u| resolve_http(base, &u)) {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

fn entry(e: feed_rs::model::Entry, base: &str) -> Entry {
    let link = alternate(&e.links).map(|l| l.href.clone());
    // Only HTTP(S) links are kept; the reader hides "Open original" when empty.
    let url = link
        .as_deref()
        .and_then(|l| resolve_http(base, l))
        .unwrap_or_default();
    let title = e
        .title
        .as_ref()
        .map(|t| content::plain(&t.content))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled article".into());
    let identity = if !e.id.is_empty() {
        e.id.clone()
    } else if let Some(link) = &link {
        format!("url:{link}")
    } else {
        let published = e.published.map(|d| d.to_rfc3339()).unwrap_or_default();
        let digest = sha256_hex(format!("{title}\n{published}").as_bytes());
        format!("fallback:{digest}")
    };
    let mut body = e
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .or_else(|| e.summary.as_ref().map(|s| s.content.clone()))
        .unwrap_or_default();
    for href in attachments(&e, base) {
        body.push_str(&format!(
            "<p><a href=\"{}\">Download attachment</a></p>",
            escape(&href)
        ));
    }
    let html = content::sanitize(&body, if url.is_empty() { base } else { &url });
    Entry {
        identity,
        title,
        text: content::plain(&html),
        html,
        url,
        author: e
            .authors
            .iter()
            .filter_map(|a| a.name.as_deref().or(a.email.as_deref()))
            .collect::<Vec<_>>()
            .join(", "),
        published: e.published.or(e.updated).map(|d| d.timestamp()),
    }
}

pub(super) fn parse_document(data: &[u8], base: &str) -> Result<(Vec<Entry>, Option<String>)> {
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(base))
        // Missing IDs fall back to the link or a content hash, not a random ID.
        .id_generator(|_, _, _| String::new())
        .build()
        .parse(data)?;
    let site_url = alternate(&feed.links)
        .and_then(|l| validate_url(&l.href).ok())
        .or_else(|| {
            url::Url::parse(base)
                .ok()
                .map(|u| format!("{}/", u.origin().ascii_serialization()))
        });
    let entries = feed.entries.into_iter().map(|e| entry(e, base)).collect();
    Ok((entries, site_url))
}
