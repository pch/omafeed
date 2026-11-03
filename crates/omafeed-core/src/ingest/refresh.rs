//! Refresh passes: which feeds are due, how many download at once, and storing the results.
use super::download::{MAX_BACKOFF_SECONDS, RetryDelay, download_from};
use crate::{
    Db, http,
    model::{Download, Feed},
};
use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use reqwest::Client;
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

    pub async fn discover(&self, input: &str) -> Result<Vec<super::discovery::Candidate>> {
        tokio::time::timeout(
            Duration::from_secs(60),
            super::discovery::discover(&self.client, input),
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
            let _ = tokio::task::spawn_blocking(move || super::icons::prune(&cache)).await;
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
            download_from(&self.client, &feed).await
        };
        // The website's icon is fetched below. Where its address came from decides whether it
        // may be local: the feed as finally served (after redirects) when it named it now, the
        // address the user entered when there is none, and unknown for one stored earlier (a
        // 304 says nothing about which response named it).
        let fresh = match &result {
            Ok((Download::Updated(u), from)) => u
                .site_url
                .clone()
                .filter(|s| !s.is_empty())
                .map(|site| (site, from.clone())),
            _ => None,
        };
        let (site_url, source) = match fresh {
            Some((site, from)) => (site, Some(from)),
            None if feed.site_url.is_empty() => (feed.url.clone(), Some(feed.url.clone())),
            None => (feed.site_url.clone(), None),
        };
        let result = result.map(|(download, _)| download);
        // The feed may be edited or deleted while it downloads; the store then drops the result.
        let (id, revision) = (feed.id, feed.revision);
        let error = match result {
            Ok(download) => {
                let minutes = self.minutes;
                self.db
                    .call(move |s| s.commit_download(id, revision, download, minutes))
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
                    .call(move |s| s.record_failure(id, revision, &error, now, delay))
                    .await;
                Some(match saved {
                    Ok(()) => message,
                    Err(e) => format!("{message}; saving error: {e}"),
                })
            }
        };
        if let Some(cache) = &self.cache {
            let target = super::icons::path(cache, &site_url);
            if self.icons.lock().unwrap().insert(target) {
                super::icons::cache(
                    &self.client,
                    cache,
                    &site_url,
                    source.as_deref(),
                    self.force,
                )
                .await;
            }
        }
        (feed.title, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::{client, ok, server};

    /// Refresh one feed at `url` with a client that sees `remote` names as remote hosts.
    async fn refresh(url: String, remote: &[(&str, std::net::SocketAddr)]) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        db.call(move |s| s.add_feed("Feed", &url, None))
            .await
            .unwrap();
        let refresher = Refresher {
            client: client(remote),
            busy: Arc::default(),
            cache: Some(dir.path().join("cache")),
        };
        let (tx, _rx) = async_channel::unbounded();
        refresher.refresh(db, true, 30, tx).await.unwrap();
    }

    fn feed_naming(site: &str) -> String {
        format!(
            r#"<rss version="2.0"><channel><title>Feed</title><link>{site}</link></channel></rss>"#
        )
    }

    #[tokio::test]
    async fn a_remote_feed_reached_through_a_local_redirect_cannot_name_a_local_site() {
        let (site, hits) = server(|_| ok("text/html", "<html></html>")).await;
        let body = feed_naming(&format!("http://{site}/"));
        let (remote, _) = server(move |_| ok("application/rss+xml", &body)).await;
        let location = format!("http://remote.test:{}/feed", remote.port());
        let (local, _) = server(move |_| {
            format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        })
        .await;
        refresh(format!("http://{local}/feed"), &[("remote.test", remote)]).await;
        assert!(
            hits.lock().unwrap().is_empty(),
            "the local site was requested"
        );
    }

    #[tokio::test]
    async fn a_stored_website_is_not_trusted_because_its_feed_is_unchanged() {
        let (site, hits) = server(|_| ok("text/html", "<html></html>")).await;
        let body = feed_naming(&format!("http://{site}/"));
        let (remote, _) = server(move |_| ok("application/rss+xml", &body)).await;
        let location = format!("http://remote.test:{}/feed", remote.port());
        // First a redirect to the remote feed, then "not modified" from the local address.
        let answered = std::sync::atomic::AtomicBool::new(false);
        let (local, _) = server(move |_| {
            if answered.swap(true, std::sync::atomic::Ordering::SeqCst) {
                "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            } else {
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        let url = format!("http://{local}/feed");
        db.call(move |s| s.add_feed("Feed", &url, None))
            .await
            .unwrap();
        let refresher = Refresher {
            client: client(&[("remote.test", remote)]),
            busy: Arc::default(),
            cache: Some(dir.path().join("cache")),
        };
        for _ in 0..2 {
            let (tx, _rx) = async_channel::unbounded();
            refresher.refresh(db.clone(), true, 30, tx).await.unwrap();
        }
        let stored = db
            .call(|s| Ok(s.library()?.feeds[0].site_url.clone()))
            .await
            .unwrap();
        assert_eq!(stored, format!("http://{site}/"));
        assert!(
            hits.lock().unwrap().is_empty(),
            "the local site was requested"
        );
    }

    #[tokio::test]
    async fn a_local_feed_may_name_a_local_site() {
        let (site, hits) = server(|_| ok("text/html", "<html></html>")).await;
        let body = feed_naming(&format!("http://{site}/"));
        let (local, _) = server(move |_| ok("application/rss+xml", &body)).await;
        refresh(format!("http://{local}/feed"), &[]).await;
        assert!(
            !hits.lock().unwrap().is_empty(),
            "the local site's icon was not looked for"
        );
    }
}
