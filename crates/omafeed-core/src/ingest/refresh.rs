//! Refresh passes: which feeds are due, how many download at once, and storing the results.
use super::{
    download::{RetryDelay, download_from},
    limits::{HostLimits, host},
    schedule,
};
use crate::{
    Db, http,
    model::{Download, Feed, FeedId},
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
/// Downloads at once from one host, once its first answer has set no limit.
const CONCURRENT_PER_HOST: usize = 2;

/// Which feeds a refresh pass downloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Which {
    /// Those whose refresh interval has elapsed.
    Due,
    /// Every feed, due or not.
    All,
    /// These feeds, due or not. IDs no longer in the library are skipped.
    Only(Vec<FeedId>),
}

impl Which {
    fn includes(&self, feed: &Feed, now: i64) -> bool {
        match self {
            Self::Due => feed.next_fetch <= now,
            Self::All => true,
            Self::Only(ids) => ids.contains(&feed.id),
        }
    }

    /// Whether the user asked for it, rather than its interval coming round: retry failed
    /// icons too.
    fn forced(&self) -> bool {
        *self != Self::Due
    }
}

/// What a refresh pass reports as it goes.
#[derive(Clone, Debug)]
pub enum Progress {
    /// The pass started with this many due feeds.
    Started(usize),
    /// One feed finished, in completion order.
    Feed {
        /// The feed's title.
        title: String,
        /// Why it failed, or `None`.
        error: Option<String>,
        /// Feeds finished so far, this one included.
        done: usize,
        /// Feeds in the pass.
        total: usize,
    },
    /// The pass ended.
    Finished {
        /// Feeds in the pass.
        total: usize,
        /// Feeds that failed.
        failed: usize,
    },
}

/// Refreshes feeds and finds them. Cheap to clone; clones share one HTTP client and allow
/// only one refresh pass at a time between them.
#[derive(Clone)]
pub struct Refresher {
    client: Client,
    busy: Arc<AtomicBool>,
    cache: Option<PathBuf>,
    limits: HostLimits,
}

/// Shared state for one refresh pass.
struct Pass {
    db: Db,
    client: Client,
    cache: Option<PathBuf>,
    force: bool,
    minutes: u32,
    now: i64,
    limits: HostLimits,
    hosts: HashMap<String, Gate>,
    /// Icon cache files already claimed by a feed in this pass.
    icons: Mutex<HashSet<PathBuf>>,
}

/// The downloads one host allows at a time: one until its first answer, which may set a rate
/// limit (Reddit allows one feed a minute), then [`CONCURRENT_PER_HOST`] if it did not.
struct Gate {
    permits: Semaphore,
    opened: AtomicBool,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            permits: Semaphore::new(1),
            opened: AtomicBool::new(false),
        }
    }
}

impl Refresher {
    /// A refresher that does not fetch website icons.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http::client()?,
            busy: Arc::new(AtomicBool::new(false)),
            cache: None,
            limits: HostLimits::default(),
        })
    }

    /// Also fetch website icons into the cache directory `path` while refreshing.
    pub fn with_cache(mut self, path: PathBuf) -> Self {
        self.cache = Some(path);
        self
    }

    /// The feeds at `input`: the feed itself if it is one, else those the page advertises (or,
    /// failing that, the usual feed addresses on its site) that turn out to be readable feeds,
    /// in the page's order. Fails if there are none, or after a minute.
    pub async fn discover(&self, input: &str) -> Result<Vec<super::discovery::Candidate>> {
        tokio::time::timeout(
            Duration::from_secs(60),
            super::discovery::discover(&self.client, &self.limits, input),
        )
        .await
        .context("Feed discovery timed out")?
    }

    /// Refresh the feeds `which` chooses. Overlapping calls return immediately. While a
    /// Miniflux account is connected, Miniflux fetches the feeds and this refreshes none; see
    /// [`crate::sync::sync`].
    pub async fn refresh(
        &self,
        db: Db,
        which: Which,
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
        let mut feeds = db
            .call(|s| {
                s.snapshot(|s| {
                    let connected = s.account()?.is_some();
                    let feeds = s.library()?.feeds;
                    Ok(if connected { Vec::new() } else { feeds })
                })
            })
            .await?
            .into_iter()
            .filter(|f| which.includes(f, now))
            .collect::<Vec<_>>();
        // Least recently tried first: a host that answers one feed at a time (each host's gate
        // takes them in turn) then gets round to all of its feeds, not the same one every pass.
        // Postponing a feed does not count as trying it.
        feeds.sort_by_key(|f| f.last_attempt);
        let total = feeds.len();
        let _ = events.send(Progress::Started(total)).await;
        let hosts = feeds
            .iter()
            .map(|f| (host(&f.url), Gate::default()))
            .collect();
        let pass = Pass {
            db,
            client: self.client.clone(),
            cache: self.cache.clone(),
            force: which.forced(),
            minutes,
            now,
            limits: self.limits.clone(),
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
            let gate = &self.hosts[&host(&feed.url)];
            let _permit = gate.permits.acquire().await;
            let now = chrono::Utc::now().timestamp();
            if let Some(until) = self.limits.until(&feed.url, now) {
                return self.postpone(feed, until - now, until).await;
            }
            let result = download_from(&self.client, &self.limits, &feed).await;
            let now = chrono::Utc::now().timestamp();
            if self.limits.until(&feed.url, now).is_none()
                && !gate.opened.swap(true, Ordering::SeqCst)
            {
                gate.permits.add_permits(CONCURRENT_PER_HOST - 1);
            }
            result
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
                    .call(move |s| {
                        let now = chrono::Utc::now().timestamp();
                        let next = schedule::after_success(now, minutes);
                        s.commit_download(id, revision, download, now, next)
                    })
                    .await
                    .err()
                    .map(|e| format!("{e:#}"))
            }
            Err(e) => {
                let message = format!("{e:#}");
                let asked = e.downcast_ref::<RetryDelay>().map(|r| r.0);
                let (error, now) = (message.clone(), self.now);
                let next = schedule::after_failure(now, feed.failures, asked);
                let saved = self
                    .db
                    .call(move |s| s.record_failure(id, revision, &error, now, next))
                    .await;
                Some(match saved {
                    Ok(()) => message,
                    Err(e) => format!("{message}; saving error: {e}"),
                })
            }
        };
        // Not while the site's host is holding requests back: its icon can wait for a later pass.
        let held_back = self
            .limits
            .until(&site_url, chrono::Utc::now().timestamp())
            .is_some();
        if let Some(cache) = self.cache.as_ref().filter(|_| !held_back) {
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

    /// Leave `feed` for `until`, without counting a failure: its host asked for `wait` more
    /// seconds without requests.
    async fn postpone(&self, feed: Feed, wait: i64, until: i64) -> (String, Option<String>) {
        let (id, revision) = (feed.id, feed.revision);
        let message = format!(
            "Postponed: {} allows another request in {wait} seconds",
            host(&feed.url)
        );
        let saved = self.db.call(move |s| s.postpone(id, revision, until)).await;
        let error = match saved {
            Ok(()) => message,
            Err(e) => format!("{message}; saving error: {e}"),
        };
        (feed.title, Some(error))
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
            limits: HostLimits::default(),
        };
        let (tx, _rx) = async_channel::unbounded();
        refresher.refresh(db, Which::All, 30, tx).await.unwrap();
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
            limits: HostLimits::default(),
        };
        for _ in 0..2 {
            let (tx, _rx) = async_channel::unbounded();
            refresher
                .refresh(db.clone(), Which::All, 30, tx)
                .await
                .unwrap();
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

    /// A server answering as Reddit does: one request a minute, said only in X-RateLimit
    /// headers, with `status` (a 200 with a feed, or a 304).
    async fn rate_limited(
        status: &'static str,
    ) -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        server(move |_| {
            let body = if status.starts_with("200") {
                feed_naming("")
            } else {
                String::new()
            };
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/rss+xml\r\nx-ratelimit-remaining: 0.0\r\nx-ratelimit-reset: 60\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        })
        .await
    }

    #[tokio::test]
    async fn a_host_out_of_requests_is_not_asked_again_until_it_resets() {
        let (server, hits) = rate_limited("200 OK").await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        db.call(move |s| {
            for name in ["a", "b", "c"] {
                s.add_feed(name, &format!("http://{server}/{name}"), None)?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let refresher = Refresher {
            client: client(&[]),
            busy: Arc::default(),
            cache: Some(dir.path().join("cache")),
            limits: HostLimits::default(),
        };
        let (tx, rx) = async_channel::unbounded();
        refresher
            .refresh(db.clone(), Which::All, 30, tx)
            .await
            .unwrap();
        assert_eq!(hits.lock().unwrap().len(), 1, "{:?}", hits.lock().unwrap());
        let errors: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|p| match p {
                Progress::Feed { error, .. } => Some(error),
                _ => None,
            })
            .collect();
        assert_eq!(errors.iter().filter(|e| e.is_none()).count(), 1);
        assert!(
            errors
                .iter()
                .flatten()
                .all(|e| e.starts_with("Postponed: 127.0.0.1 allows another request in")),
            "{errors:?}"
        );
        let now = chrono::Utc::now().timestamp();
        let feeds = db.call(|s| Ok(s.library()?.feeds)).await.unwrap();
        for feed in feeds {
            assert!(feed.next_fetch > now + 50, "{} is due too soon", feed.title);
            assert_eq!((feed.failures, feed.error), (0, None), "{}", feed.title);
        }
        // Until it resets, the next pass asks for nothing, even forced.
        let (tx, _rx) = async_channel::unbounded();
        refresher
            .refresh(db.clone(), Which::All, 30, tx)
            .await
            .unwrap();
        assert_eq!(hits.lock().unwrap().len(), 1);
        // After it, the feeds not yet tried go first, though postponing pushed them later.
        let refresher = Refresher {
            limits: HostLimits::default(),
            ..refresher
        };
        let (tx, _rx) = async_channel::unbounded();
        refresher.refresh(db, Which::All, 30, tx).await.unwrap();
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 2);
        assert_ne!(hits[0], hits[1], "{hits:?}");
    }

    #[tokio::test]
    async fn an_unchanged_feed_still_counts_against_its_hosts_requests() {
        let (server, hits) = rate_limited("304 Not Modified").await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        db.call(move |s| {
            for name in ["a", "b"] {
                s.add_feed(name, &format!("http://{server}/{name}"), None)?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let refresher = Refresher {
            client: client(&[]),
            busy: Arc::default(),
            cache: None,
            limits: HostLimits::default(),
        };
        let (tx, _rx) = async_channel::unbounded();
        refresher.refresh(db, Which::All, 30, tx).await.unwrap();
        assert_eq!(hits.lock().unwrap().len(), 1, "{:?}", hits.lock().unwrap());
    }

    #[tokio::test]
    async fn a_refresh_of_chosen_feeds_downloads_only_those() {
        let (server, hits) = server(|_| ok("application/rss+xml", &feed_naming(""))).await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("test.db")).unwrap();
        let (a, _b) = db
            .call(move |s| {
                Ok((
                    s.add_feed("A", &format!("http://{server}/a"), None)?,
                    s.add_feed("B", &format!("http://{server}/b"), None)?,
                ))
            })
            .await
            .unwrap();
        let refresher = Refresher {
            client: client(&[]),
            busy: Arc::default(),
            cache: None,
            limits: HostLimits::default(),
        };
        let (tx, rx) = async_channel::unbounded();
        refresher
            .refresh(db.clone(), Which::Only(vec![a, FeedId(999)]), 30, tx)
            .await
            .unwrap();
        assert_eq!(*hits.lock().unwrap(), ["GET /a HTTP/1.1"]);
        assert!(matches!(rx.recv().await, Ok(Progress::Started(1))));
        // Both are now due, B never having been fetched and A forced: Due takes only B.
        let (tx, _rx) = async_channel::unbounded();
        refresher.refresh(db, Which::Due, 30, tx).await.unwrap();
        assert_eq!(hits.lock().unwrap().len(), 2);
        assert_eq!(hits.lock().unwrap()[1], "GET /b HTTP/1.1");
    }
}
