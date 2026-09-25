use crate::{
    Db, article,
    db::Feed,
    http::{self, BODY_LIMIT},
    util::{escape, resolve_http, validate_url},
};
use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

const CONCURRENT_FEEDS: usize = 8;
const CONCURRENT_PER_HOST: usize = 2;
const MAX_BACKOFF_SECONDS: i64 = 86400;

#[derive(Clone, Debug)]
pub struct Entry {
    pub identity: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub published: Option<i64>,
    pub html: String,
    pub text: String,
}

#[derive(Debug)]
pub enum Download {
    /// The server answered 304; cached articles stay as they are.
    NotModified,
    Updated(Update),
}

#[derive(Debug)]
pub struct Update {
    pub site_url: Option<String>,
    pub entries: Vec<Entry>,
    pub etag: Option<String>,
    pub modified: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Progress {
    Started(usize),
    Feed {
        title: String,
        error: Option<String>,
        done: usize,
        total: usize,
    },
    Finished {
        total: usize,
        failed: usize,
    },
}

#[derive(Clone)]
pub struct Refresher {
    client: Client,
    busy: Arc<AtomicBool>,
    cache: Option<PathBuf>,
}

/// Shared state for one refresh pass.
struct Pass {
    db: Db,
    client: Client,
    cache: Option<PathBuf>,
    force: bool,
    minutes: u32,
    now: i64,
    hosts: HashMap<String, Arc<Semaphore>>,
    /// Icon cache files already claimed by a feed in this pass.
    icons: Mutex<HashSet<PathBuf>>,
}

fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

impl Refresher {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http::client()?,
            busy: Arc::new(AtomicBool::new(false)),
            cache: None,
        })
    }

    pub fn with_cache(mut self, path: PathBuf) -> Self {
        self.cache = Some(path);
        self
    }

    pub async fn discover(&self, input: &str) -> Result<Vec<crate::discovery::Candidate>> {
        tokio::time::timeout(
            Duration::from_secs(60),
            crate::discovery::discover(&self.client, input),
        )
        .await
        .context("Feed discovery timed out")?
    }

    /// Refresh due feeds (or all with `force`). Overlapping calls return immediately.
    pub async fn refresh(
        &self,
        db: Db,
        force: bool,
        minutes: u32,
        events: async_channel::Sender<Progress>,
    ) -> Result<()> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        struct Reset(Arc<AtomicBool>);
        impl Drop for Reset {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _reset = Reset(self.busy.clone());
        let now = chrono::Utc::now().timestamp();
        let feeds = db
            .call(|s| Ok(s.library()?.feeds))
            .await?
            .into_iter()
            .filter(|f| force || f.next_fetch <= now)
            .collect::<Vec<_>>();
        let total = feeds.len();
        let _ = events.send(Progress::Started(total)).await;
        let hosts = feeds
            .iter()
            .map(|f| (host(&f.url), Arc::new(Semaphore::new(CONCURRENT_PER_HOST))))
            .collect();
        let pass = Pass {
            db,
            client: self.client.clone(),
            cache: self.cache.clone(),
            force,
            minutes,
            now,
            hosts,
            icons: Mutex::default(),
        };
        let mut work = stream::iter(feeds.into_iter().map(|feed| pass.refresh_feed(feed)))
            .buffer_unordered(CONCURRENT_FEEDS);
        let mut done = 0;
        let mut failed = 0;
        while let Some((title, error)) = work.next().await {
            done += 1;
            failed += usize::from(error.is_some());
            let _ = events
                .send(Progress::Feed {
                    title,
                    error,
                    done,
                    total,
                })
                .await;
        }
        if let Some(cache) = self.cache.clone() {
            let _ = tokio::task::spawn_blocking(move || crate::icons::prune(&cache)).await;
        }
        let _ = events.send(Progress::Finished { total, failed }).await;
        Ok(())
    }
}

impl Pass {
    /// Download and store one feed, returning its title and any error message.
    async fn refresh_feed(&self, feed: Feed) -> (String, Option<String>) {
        let result = {
            let _permit = self.hosts[&host(&feed.url)].acquire().await;
            download(&self.client, &feed).await
        };
        let site_url = match &result {
            Ok(Download::Updated(u)) => u.site_url.clone().filter(|s| !s.is_empty()),
            _ => None,
        }
        .unwrap_or_else(|| feed.website().to_owned());
        let id = feed.id;
        let error = match result {
            Ok(download) => {
                let minutes = self.minutes;
                self.db
                    .call(move |s| s.commit_download(id, download, minutes))
                    .await
                    .err()
                    .map(|e| format!("{e:#}"))
            }
            Err(e) => {
                let message = format!("{e:#}");
                let delay = e.downcast_ref::<RetryDelay>().map_or_else(
                    || (60 * 2_i64.pow(feed.failures.clamp(0, 9) as u32)).min(MAX_BACKOFF_SECONDS),
                    |r| r.0,
                );
                let (error, now) = (message.clone(), self.now);
                let saved = self
                    .db
                    .call(move |s| s.record_failure(id, &error, now, delay))
                    .await;
                Some(match saved {
                    Ok(()) => message,
                    Err(e) => format!("{message}; saving error: {e}"),
                })
            }
        };
        if let Some(cache) = &self.cache {
            let target = crate::icons::path(cache, &site_url);
            if self.icons.lock().unwrap().insert(target) {
                crate::icons::cache(&self.client, cache, &site_url, self.force).await;
            }
        }
        (feed.title, error)
    }
}

#[derive(Debug)]
struct RetryDelay(i64);

impl std::fmt::Display for RetryDelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Server requested retry in {} seconds", self.0)
    }
}

impl std::error::Error for RetryDelay {}

fn retry_after(value: &str) -> i64 {
    value
        .parse::<i64>()
        .ok()
        .or_else(|| {
            let date = httpdate::parse_http_date(value).ok()?;
            let wait = date.duration_since(std::time::SystemTime::now()).ok()?;
            Some(wait.as_secs() as i64)
        })
        .unwrap_or(300)
        .clamp(1, MAX_BACKOFF_SECONDS)
}

pub async fn download(client: &Client, feed: &Feed) -> Result<Download> {
    let mut request = client.get(&feed.url);
    if let Some(etag) = &feed.etag {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    if let Some(modified) = &feed.modified {
        request = request.header(header::IF_MODIFIED_SINCE, modified);
    }
    let response = request.send().await.context("Download feed")?;
    let status = response.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Download::NotModified);
    }
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) && let Some(value) = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
    {
        return Err(RetryDelay(retry_after(value)).into());
    }
    let response = response.error_for_status()?;
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let etag = header(header::ETAG);
    let modified = header(header::LAST_MODIFIED);
    let base = response.url().to_string();
    let data = http::read_limited(response, BODY_LIMIT).await?;
    let (entries, site_url) =
        tokio::task::spawn_blocking(move || parse_document(&data, &base)).await??;
    Ok(Download::Updated(Update {
        site_url,
        entries,
        etag,
        modified,
    }))
}

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
        .map(|t| article::plain(&t.content))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled article".into());
    let identity = if !e.id.is_empty() {
        e.id.clone()
    } else if let Some(link) = &link {
        format!("url:{link}")
    } else {
        let published = e.published.map(|d| d.to_rfc3339()).unwrap_or_default();
        let digest = Sha256::digest(format!("{title}\n{published}").as_bytes());
        format!("fallback:{digest:x}")
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
    let html = article::sanitize(&body, if url.is_empty() { base } else { &url });
    Entry {
        identity,
        title,
        text: article::plain(&html),
        html,
        url,
        author: e
            .authors
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        published: e.published.or(e.updated).map(|d| d.timestamp()),
    }
}

fn parse_document(data: &[u8], base: &str) -> Result<(Vec<Entry>, Option<String>)> {
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
