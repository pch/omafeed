use crate::{Db, Store, article, db::Feed};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use reqwest::{Client, header};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const BODY_LIMIT: usize = 10 * 1024 * 1024;
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
pub struct Download {
    pub site_url: Option<String>,
    pub entries: Vec<Entry>,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub not_modified: bool,
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
    cache: Option<std::path::PathBuf>,
}
impl Refresher {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!(
                    "Omafeed/",
                    env!("CARGO_PKG_VERSION"),
                    " (+https://github.com/pch/omafeed)"
                ))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::limited(8))
                .build()?,
            busy: Arc::new(AtomicBool::new(false)),
            cache: None,
        })
    }
    pub fn with_cache(mut self, path: std::path::PathBuf) -> Self {
        self.cache = Some(path);
        self
    }
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
        let mut hosts = HashMap::new();
        for f in &feeds {
            hosts
                .entry(
                    url::Url::parse(&f.url)?
                        .host_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(2)));
        }
        let tasks=feeds.into_iter().map(|feed| {
            let cache=self.cache.clone();let db=db.clone();let client=self.client.clone();let host=url::Url::parse(&feed.url).ok().and_then(|u|u.host_str().map(str::to_owned)).unwrap_or_default();let sem=hosts[&host].clone();
            async move {
                let title=feed.title.clone();let _permit=sem.acquire().await;
                let result=download(&client,&feed).await;
                let site_url=result.as_ref().ok().and_then(|d|d.site_url.clone()).filter(|s|!s.is_empty()).unwrap_or_else(||if feed.site_url.is_empty(){feed.url.clone()}else{feed.site_url.clone()});
                let error=match result {
                    Ok(download)=>{let id=feed.id; db.call(move|s|s.commit_download(id,download,minutes)).await.err().map(|e|e.to_string())},
                    Err(e)=>{
                        let message=format!("{e:#}");let err=message.clone();let id=feed.id;
                        let delay=e.downcast_ref::<RetryDelay>().map(|r|r.0).unwrap_or_else(||(60_i64*2_i64.pow(feed.failures.min(9) as u32)).min(86400));
                        let result=db.call(move|s| {s.conn.execute("UPDATE feeds SET error=?1,failures=failures+1,last_attempt=?2,next_fetch=?3 WHERE id=?4",params![err,now,now+delay,id])?;Ok(())}).await;
                        Some(if let Err(e)=result{format!("{message}; saving error: {e}")}else{message})
                    }
                };
                if let Some(cache)=cache {crate::icons::cache(&client,&cache,&site_url).await;}
                (title,error)
            }
        });
        let mut work = stream::iter(tasks).buffer_unordered(8);
        let mut done = 0;
        let mut failed = 0;
        while let Some((title, error)) = work.next().await {
            done += 1;
            if error.is_some() {
                failed += 1;
            }
            let _ = events
                .send(Progress::Feed {
                    title,
                    error,
                    done,
                    total,
                })
                .await;
        }
        if let Some(cache) = &self.cache {
            crate::icons::prune(cache);
        }
        let _ = events.send(Progress::Finished { total, failed }).await;
        Ok(())
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
pub async fn download(client: &Client, feed: &Feed) -> Result<Download> {
    let mut request = client.get(&feed.url);
    if let Some(etag) = &feed.etag
        && !feed.site_url.is_empty()
    {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    if let Some(modified) = &feed.modified
        && !feed.site_url.is_empty()
    {
        request = request.header(header::IF_MODIFIED_SINCE, modified);
    }
    let response = request.send().await.context("Download feed")?;
    if response.status().as_u16() == 304 {
        return Ok(Download {
            entries: vec![],
            etag: None,
            modified: None,
            not_modified: true,
            site_url: None,
        });
    }
    if matches!(response.status().as_u16(), 429 | 503)
        && let Some(value) = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
    {
        let seconds = value
            .parse::<i64>()
            .ok()
            .or_else(|| {
                httpdate::parse_http_date(value).ok().and_then(|d| {
                    d.duration_since(std::time::SystemTime::now())
                        .ok()
                        .map(|d| d.as_secs() as i64)
                })
            })
            .unwrap_or(300)
            .clamp(1, 86400);
        return Err(RetryDelay(seconds).into());
    }
    let response = response.error_for_status()?;
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let modified = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let base = response.url().to_string();
    let mut data = Vec::new();
    let mut bytes = response.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk?;
        if data.len() + chunk.len() > BODY_LIMIT {
            bail!("Feed exceeds 10 MB decompressed limit");
        }
        data.extend_from_slice(&chunk);
    }
    let (entries, site_url) =
        tokio::task::spawn_blocking(move || parse_document(&data, &base)).await??;
    Ok(Download {
        entries,
        etag,
        modified,
        not_modified: false,
        site_url,
    })
}
pub fn parse(data: &[u8], base: &str) -> Result<Vec<Entry>> {
    Ok(parse_document(data, base)?.0)
}
fn parse_document(data: &[u8], base: &str) -> Result<(Vec<Entry>, Option<String>)> {
    let parser = feed_rs::parser::Builder::new()
        .base_uri(Some(base))
        .id_generator(|_, _, _| String::new())
        .build();
    let feed = parser.parse(data)?;
    let site_url = feed
        .links
        .iter()
        .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .and_then(|l| crate::opml::validate_url(&l.href).ok())
        .or_else(|| {
            url::Url::parse(base)
                .ok()
                .map(|u| format!("{}/", u.origin().ascii_serialization()))
        });
    Ok((
        feed.entries
            .into_iter()
            .map(|e| {
                let url = e
                    .links
                    .iter()
                    .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                    .map(|l| l.href.clone())
                    .unwrap_or_else(|| base.into());
                let title = e
                    .title
                    .map(|t| article::plain(&t.content))
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| "Untitled article".into());
                let mut body = e
                    .content
                    .and_then(|c| c.body)
                    .or_else(|| e.summary.map(|s| s.content))
                    .unwrap_or_default();
                for link in e
                    .links
                    .iter()
                    .filter(|l| l.rel.as_deref() == Some("enclosure"))
                {
                    body.push_str(&format!(
                        "<p><a href=\"{}\">Download attachment</a></p>",
                        crate::opml::escape(&link.href)
                    ));
                }
                let html = article::sanitize(&body, &url);
                let text = article::plain(&html);
                let identity = if !e.id.is_empty() {
                    e.id.clone()
                } else if let Some(link) = e
                    .links
                    .iter()
                    .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                {
                    format!("url:{}", link.href)
                } else {
                    format!(
                        "fallback:{:x}",
                        Sha256::digest(
                            format!(
                                "{}\n{}",
                                title,
                                e.published.map(|d| d.to_rfc3339()).unwrap_or_default()
                            )
                            .as_bytes()
                        )
                    )
                };
                Entry {
                    identity,
                    title,
                    url,
                    author: e
                        .authors
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    published: e.published.or(e.updated).map(|d| d.timestamp()),
                    html,
                    text,
                }
            })
            .collect(),
        site_url,
    ))
}
impl Store {
    pub fn commit_download(
        &mut self,
        feed_id: i64,
        download: Download,
        minutes: u32,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM feeds WHERE id=?1)",
            [feed_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(());
        }
        if !download.not_modified {
            for e in download.entries {
                tx.execute("INSERT INTO articles(feed_id,identity,title,url,author,published,first_seen,html,text) VALUES(?1,?2,?3,?4,?5,COALESCE(?6,?7),?7,?8,?9) ON CONFLICT(feed_id,identity) DO UPDATE SET title=excluded.title,url=excluded.url,author=excluded.author,published=COALESCE(?6,articles.published),html=excluded.html,text=excluded.text",params![feed_id,e.identity,e.title,e.url,e.author,e.published,now,e.html,e.text])?;
            }
            tx.execute(
                "UPDATE feeds SET etag=?1,modified=?2,site_url=COALESCE(?4,site_url) WHERE id=?3",
                params![download.etag, download.modified, feed_id, download.site_url],
            )?;
        }
        tx.execute("UPDATE feeds SET last_attempt=?1,last_success=?1,next_fetch=?2,error=NULL,failures=0 WHERE id=?3",params![now,now+i64::from(minutes)*60,feed_id])?;
        tx.commit()?;
        Ok(())
    }
}
