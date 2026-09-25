//! Command-line operations that run without opening a window.
use crate::DB_FILE;
use anyhow::{Context, Result, bail};
use omafeed_core::{
    Db, Paths, Settings, Store,
    article::plain,
    db::{Article, PAGE_SIZE, Query, Scope},
    fetch::{Progress, Refresher},
};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Import subscriptions from an OPML file
    Import { file: PathBuf },
    /// Export subscriptions to an OPML file
    Export { file: PathBuf },
    /// Refresh every feed now
    Refresh {
        /// Print one line of JSON when done instead of progress text
        #[arg(long)]
        json: bool,
    },
    /// Show library totals and feed errors
    Status,
    /// List the feeds a website advertises
    Discover {
        url: String,
        /// Print JSON instead of tab-separated text
        #[arg(long)]
        json: bool,
    },
    /// Print folders, feeds, unread counts and feed errors as JSON
    Feeds,
    /// Print articles as JSON, newest first
    Articles {
        /// unread, today, starred, all, feed:ID or folder:ID
        #[arg(long, default_value = "unread", value_parser = parse_scope)]
        scope: Scope,
        /// Full-text search within the scope
        #[arg(long)]
        search: Option<String>,
        /// Only unread articles
        #[arg(long)]
        unread: bool,
        /// Only articles published this recently, e.g. 90m, 24h, 2d or 1w
        #[arg(long, value_parser = parse_duration)]
        since: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Print one article as JSON, with its text (or sanitized HTML with --html)
    Article {
        id: i64,
        #[arg(long)]
        html: bool,
    },
    /// Mark articles read
    Read {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Mark articles unread
    Unread {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Star articles
    Star {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Remove the star from articles
    Unstar {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Subscribe to a website or feed URL; run `refresh` afterwards to fetch articles
    Subscribe {
        url: String,
        /// Folder ID from `feeds`
        #[arg(long)]
        folder: Option<i64>,
        /// Name to use instead of the feed's own title
        #[arg(long)]
        title: Option<String>,
    },
    /// Delete a feed and all its articles, starred ones included (requires --yes)
    Unsubscribe {
        id: i64,
        /// Confirm the permanent deletion
        #[arg(long)]
        yes: bool,
    },
}

pub fn run(command: Command) -> Result<()> {
    let paths = Paths::discover()?;
    let db = Db::open(paths.data.join(DB_FILE))?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        match command {
            Command::Import { file } => import(&db, &file).await,
            Command::Export { file } => export(&db, &file).await,
            Command::Refresh { json } => refresh(&db, &paths, json).await,
            Command::Status => status(&db).await,
            Command::Discover { url, json } => discover(&url, json).await,
            Command::Feeds => feeds(&db).await,
            Command::Articles {
                scope,
                search,
                unread,
                since,
                limit,
                offset,
            } => {
                let query = Query {
                    scope,
                    search: search.unwrap_or_default(),
                    unread_only: unread,
                    offset,
                };
                let cutoff = since.map(|seconds| chrono::Utc::now().timestamp() - seconds);
                articles(&db, query, limit, cutoff).await
            }
            Command::Article { id, html } => article(&db, id, html).await,
            Command::Read { ids } => set_state(&db, ids, |s, id| s.set_read(id, true)).await,
            Command::Unread { ids } => set_state(&db, ids, |s, id| s.set_read(id, false)).await,
            Command::Star { ids } => set_state(&db, ids, |s, id| s.set_starred(id, true)).await,
            Command::Unstar { ids } => set_state(&db, ids, |s, id| s.set_starred(id, false)).await,
            Command::Subscribe { url, folder, title } => subscribe(&db, &url, folder, title).await,
            Command::Unsubscribe { id, yes } => unsubscribe(&db, id, yes).await,
        }
    })
}

async fn import(db: &Db, file: &PathBuf) -> Result<()> {
    let text = std::fs::read_to_string(file).with_context(|| format!("Read {}", file.display()))?;
    let report = db.call(move |s| s.import(&text)).await?;
    println!(
        "Imported {}, skipped {}, invalid {}",
        report.added,
        report.skipped,
        report.invalid.len()
    );
    for e in report.invalid {
        eprintln!("{e}");
    }
    Ok(())
}

async fn export(db: &Db, file: &PathBuf) -> Result<()> {
    let opml = db.call(|s| s.export()).await?;
    std::fs::write(file, opml).with_context(|| format!("Write {}", file.display()))?;
    println!("Exported {}", file.display());
    Ok(())
}

async fn refresh(db: &Db, paths: &Paths, json: bool) -> Result<()> {
    let (tx, rx) = async_channel::unbounded();
    let logger = tokio::spawn(async move {
        let mut feeds = Vec::new();
        let mut totals = None;
        while let Ok(event) = rx.recv().await {
            match event {
                Progress::Started(n) if !json => println!("Refreshing {n} feeds"),
                Progress::Feed {
                    title,
                    error,
                    done,
                    total,
                } => {
                    if !json {
                        println!(
                            "[{done}/{total}] {title}: {}",
                            error.as_deref().unwrap_or("OK")
                        );
                    }
                    feeds.push(json!({ "title": title, "error": error }));
                }
                Progress::Finished { total, failed } => {
                    if !json {
                        println!("Finished: {total} feeds, {failed} failed");
                    }
                    totals = Some((total, failed));
                }
                _ => {}
            }
        }
        (feeds, totals)
    });
    let minutes = Settings::load(paths).refresh_minutes;
    Refresher::new()?
        .with_cache(paths.cache.clone())
        .refresh(db.clone(), true, minutes, tx)
        .await?;
    let (feeds, totals) = logger.await?;
    if json {
        let (total, failed) = totals.unwrap_or_default();
        print_json(&json!({ "total": total, "failed": failed, "feeds": feeds }))?;
    }
    Ok(())
}

async fn status(db: &Db) -> Result<()> {
    let lib = db.call(|s| s.library()).await?;
    println!(
        "{} feeds · {} folders · {} unread · {} starred",
        lib.feeds.len(),
        lib.folders.len(),
        lib.unread,
        lib.starred
    );
    for f in lib.feeds {
        if let Some(e) = f.error {
            println!("{}: {e}", f.title);
        }
    }
    Ok(())
}

async fn discover(url: &str, json: bool) -> Result<()> {
    let found = Refresher::new()?.discover(url).await?;
    if json {
        let feeds: Vec<_> = found
            .iter()
            .map(|f| json!({ "title": f.title, "url": f.url }))
            .collect();
        return print_json(&json!({ "feeds": feeds }));
    }
    for feed in found {
        println!("{}\t{}", feed.title, feed.url);
    }
    Ok(())
}

fn parse_scope(text: &str) -> Result<Scope, String> {
    let id = |n: &str| {
        n.parse::<i64>()
            .map_err(|_| format!("expected a number in '{text}'"))
    };
    match text {
        "unread" => Ok(Scope::Unread),
        "today" => Ok(Scope::Today),
        "starred" => Ok(Scope::Starred),
        "all" => Ok(Scope::All),
        _ => match text.split_once(':') {
            Some(("feed", n)) => Ok(Scope::Feed(id(n)?)),
            Some(("folder", n)) => Ok(Scope::Folder(id(n)?)),
            _ => Err(format!(
                "unknown scope '{text}'; use unread, today, starred, all, feed:ID or folder:ID"
            )),
        },
    }
}

/// Seconds in a duration such as `90m`, `24h`, `2d` or `1w`.
fn parse_duration(text: &str) -> Result<i64, String> {
    let bad = || format!("expected a duration like 90m, 24h, 2d or 1w, got '{text}'");
    let unit = text.chars().last().ok_or_else(bad)?;
    let count: i64 = text[..text.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| bad())?;
    let seconds = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return Err(bad()),
    };
    match count.checked_mul(seconds) {
        Some(total) if total > 0 => Ok(total),
        _ => Err(bad()),
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn iso(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0).map(|d| d.to_rfc3339())
}

fn summary(a: &Article) -> Value {
    json!({
        "id": a.id,
        "feed_id": a.feed_id,
        "feed": a.feed_title,
        "title": a.title,
        "url": a.url,
        "author": a.author,
        "published": iso(a.published),
        "read": a.read,
        "starred": a.starred,
    })
}

async fn feeds(db: &Db) -> Result<()> {
    let lib = db.call(|s| s.library()).await?;
    let folders: Vec<_> = lib
        .folders
        .iter()
        .map(|f| json!({ "id": f.id, "parent": f.parent, "name": f.name }))
        .collect();
    let feeds: Vec<_> = lib
        .feeds
        .iter()
        .map(|f| {
            json!({
                "id": f.id,
                "folder": f.folder,
                "title": f.title,
                "url": f.url,
                "site_url": f.site_url,
                "unread": f.unread,
                "error": f.error,
            })
        })
        .collect();
    print_json(&json!({
        "unread": lib.unread,
        "starred": lib.starred,
        "folders": folders,
        "feeds": feeds,
    }))
}

/// Newest-first articles, reading past one page when `limit` or `since` asks for more.
async fn articles(db: &Db, query: Query, limit: usize, since: Option<i64>) -> Result<()> {
    let mut found = Vec::new();
    let mut offset = query.offset;
    loop {
        let page_query = Query {
            offset,
            ..query.clone()
        };
        let page = db.call(move |s| s.articles(&page_query)).await?;
        let full = page.len() == PAGE_SIZE;
        // Pages are ordered by date, so one article older than the cutoff ends the search.
        let reached_cutoff = since.is_some_and(|c| page.last().is_some_and(|a| a.published < c));
        offset += page.len();
        found.extend(
            page.into_iter()
                .filter(|a| since.is_none_or(|c| a.published >= c)),
        );
        // One article beyond `limit` is enough to know the list was cut short.
        if !full || reached_cutoff || found.len() > limit {
            break;
        }
    }
    let truncated = found.len() > limit;
    let items: Vec<_> = found
        .iter()
        .take(limit)
        .map(|a| {
            let mut item = summary(a);
            item["preview"] = json!(a.preview);
            item
        })
        .collect();
    print_json(&json!({ "count": items.len(), "truncated": truncated, "articles": items }))
}

async fn article(db: &Db, id: i64, html: bool) -> Result<()> {
    let a = db
        .call(move |s| s.article(id))
        .await
        .map_err(|_| anyhow::anyhow!("No article with id {id}"))?;
    let mut item = summary(&a);
    if html {
        item["html"] = json!(a.html);
    } else {
        item["text"] = json!(plain(&a.html));
    }
    print_json(&item)
}

/// Apply `change` to every article, or to none if any ID does not exist.
async fn set_state(db: &Db, ids: Vec<i64>, change: fn(&Store, i64) -> Result<()>) -> Result<()> {
    let count = ids.len();
    db.call(move |s| {
        for &id in &ids {
            if s.article(id).is_err() {
                bail!("No article with id {id}");
            }
        }
        for &id in &ids {
            change(s, id)?;
        }
        Ok(())
    })
    .await?;
    print_json(&json!({ "updated": count }))
}

async fn subscribe(db: &Db, url: &str, folder: Option<i64>, title: Option<String>) -> Result<()> {
    let found = Refresher::new()?.discover(url).await?;
    let Some(pick) = found.first() else {
        bail!("No feed found at {url}");
    };
    let name = title.unwrap_or_else(|| pick.title.clone());
    let feed_url = pick.url.clone();
    let (add_name, add_url) = (name.clone(), feed_url.clone());
    let id = db
        .call(move |s| s.add_feed(&add_name, &add_url, folder))
        .await?;
    let others: Vec<_> = found[1..]
        .iter()
        .map(|c| json!({ "title": c.title, "url": c.url }))
        .collect();
    print_json(&json!({ "id": id, "title": name, "url": feed_url, "other_feeds_found": others }))
}

async fn unsubscribe(db: &Db, id: i64, yes: bool) -> Result<()> {
    let (title, url, total, starred) = db
        .call(move |s| {
            let Some(feed) = s.library()?.feeds.into_iter().find(|f| f.id == id) else {
                bail!("No feed with id {id}");
            };
            let (total, starred) = s.feed_article_counts(id)?;
            Ok((feed.title, feed.url, total, starred))
        })
        .await?;
    if !yes {
        bail!(
            "Not removed. This would permanently delete \"{title}\" and its {total} articles ({starred} starred). Run again with --yes to confirm."
        );
    }
    db.call(move |s| s.delete_feed(id)).await?;
    print_json(&json!({
        "removed": { "id": id, "title": title, "url": url, "articles": total, "starred": starred }
    }))
}
