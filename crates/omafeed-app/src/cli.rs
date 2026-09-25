//! Command-line operations that run without opening a window.
use crate::DB_FILE;
use anyhow::{Context, Result, bail};
use omafeed_core::{
    Db, Paths, Settings, Store,
    article::plain,
    db::{Article, Folder, PAGE_SIZE, Query, Scope},
    fetch::{Progress, Refresher},
};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Article IDs from `articles`, for the commands that change many articles at once.
#[derive(clap::Args)]
pub struct Ids {
    #[arg(required = true)]
    ids: Vec<i64>,
    /// Print JSON instead of text
    #[arg(long)]
    json: bool,
}

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
    /// List folders, feeds, unread counts and feed errors
    Feeds {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// List articles, newest first
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
        /// Print JSON, with a preview of each article, instead of text
        #[arg(long)]
        json: bool,
    },
    /// Print one article's text (or sanitized HTML with --html)
    Article {
        id: i64,
        #[arg(long)]
        html: bool,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Mark articles read
    Read(Ids),
    /// Mark articles unread
    Unread(Ids),
    /// Star articles
    Star(Ids),
    /// Remove the star from articles
    Unstar(Ids),
    /// Subscribe to a website or feed URL; run `refresh` afterwards to fetch articles
    Subscribe {
        url: String,
        /// Folder ID from `feeds`
        #[arg(long)]
        folder: Option<i64>,
        /// Name to use instead of the feed's own title
        #[arg(long)]
        title: Option<String>,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Delete a feed and all its articles, starred ones included (requires --yes)
    Unsubscribe {
        id: i64,
        /// Confirm the permanent deletion
        #[arg(long)]
        yes: bool,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
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
            Command::Feeds { json } => feeds(&db, json).await,
            Command::Articles {
                scope,
                search,
                unread,
                since,
                limit,
                offset,
                json,
            } => {
                let query = Query {
                    scope,
                    search: search.unwrap_or_default(),
                    unread_only: unread,
                    offset,
                };
                let cutoff = since.map(|seconds| chrono::Utc::now().timestamp() - seconds);
                articles(&db, query, limit, cutoff, json).await
            }
            Command::Article { id, html, json } => article(&db, id, html, json).await,
            Command::Read(i) => {
                set_state(&db, i, "Marked {} read", |s, id| s.set_read(id, true)).await
            }
            Command::Unread(i) => {
                set_state(&db, i, "Marked {} unread", |s, id| s.set_read(id, false)).await
            }
            Command::Star(i) => {
                set_state(&db, i, "Starred {}", |s, id| s.set_starred(id, true)).await
            }
            Command::Unstar(i) => {
                set_state(&db, i, "Unstarred {}", |s, id| s.set_starred(id, false)).await
            }
            Command::Subscribe {
                url,
                folder,
                title,
                json,
            } => subscribe(&db, &url, folder, title, json).await,
            Command::Unsubscribe { id, yes, json } => unsubscribe(&db, id, yes, json).await,
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

/// Feed content on one line, so tabs and newlines cannot break tab-separated output.
fn line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn day(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// A folder's name with its parents, such as `Work/Blogs`; empty for no folder.
fn folder_path(folders: &[Folder], id: Option<i64>) -> String {
    let mut names = Vec::new();
    let mut next = id;
    while let Some(id) = next {
        let Some(folder) = folders.iter().find(|f| f.id == id) else {
            break;
        };
        names.push(line(&folder.name));
        next = folder.parent;
        if names.len() > folders.len() {
            break;
        }
    }
    names.reverse();
    names.join("/")
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
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

async fn feeds(db: &Db, json: bool) -> Result<()> {
    let lib = db.call(|s| s.library()).await?;
    if !json {
        println!(
            "{} feeds · {} folders · {} unread · {} starred",
            lib.feeds.len(),
            lib.folders.len(),
            lib.unread,
            lib.starred
        );
        if !lib.folders.is_empty() {
            // Sorted by full path, so a folder comes right before its subfolders.
            let mut paths: Vec<_> = lib
                .folders
                .iter()
                .map(|f| (folder_path(&lib.folders, Some(f.id)), f.id))
                .collect();
            paths.sort_by_key(|(path, id)| (path.to_lowercase(), *id));
            println!("\nID\tFOLDER");
            for (path, id) in paths {
                println!("{id}\t{path}");
            }
        }
        if !lib.feeds.is_empty() {
            println!("\nID\tUNREAD\tFEED\tFOLDER\tERROR");
            for f in &lib.feeds {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    f.id,
                    f.unread,
                    line(&f.title),
                    folder_path(&lib.folders, f.folder),
                    f.error.as_deref().map(line).unwrap_or_default()
                );
            }
        }
        return Ok(());
    }
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
async fn articles(
    db: &Db,
    query: Query,
    limit: usize,
    since: Option<i64>,
    json: bool,
) -> Result<()> {
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
    found.truncate(limit);
    if !json {
        for a in &found {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                a.id,
                if a.read { "read" } else { "unread" },
                if a.starred { "starred" } else { "-" },
                day(a.published),
                line(&a.feed_title),
                line(&a.title)
            );
        }
        if truncated {
            eprintln!("More articles match; raise --limit or use --offset.");
        }
        return Ok(());
    }
    let items: Vec<_> = found
        .iter()
        .map(|a| {
            let mut item = summary(a);
            item["preview"] = json!(a.preview);
            item
        })
        .collect();
    print_json(&json!({ "count": items.len(), "truncated": truncated, "articles": items }))
}

async fn article(db: &Db, id: i64, html: bool, json: bool) -> Result<()> {
    let a = db
        .call(move |s| s.article(id))
        .await
        .map_err(|_| anyhow::anyhow!("No article with id {id}"))?;
    if json {
        let mut item = summary(&a);
        if html {
            item["html"] = json!(a.html);
        } else {
            item["text"] = json!(plain(&a.html));
        }
        return print_json(&item);
    }
    let mut details = vec![line(&a.feed_title)];
    if !a.author.is_empty() {
        details.push(line(&a.author));
    }
    details.push(day(a.published));
    details.push(if a.read { "read" } else { "unread" }.into());
    if a.starred {
        details.push("starred".into());
    }
    println!("{}\n{}", line(&a.title), details.join(" · "));
    if !a.url.is_empty() {
        println!("{}", a.url);
    }
    println!("\n{}", if html { a.html } else { plain(&a.html) });
    Ok(())
}

/// Apply `change` to every article, or to none if any ID does not exist, then name what
/// changed. `message` says what happened, with `{}` standing for the count, such as
/// "Starred 2 articles".
async fn set_state(
    db: &Db,
    Ids { mut ids, json }: Ids,
    message: &str,
    change: fn(&Store, i64) -> Result<()>,
) -> Result<()> {
    ids.sort_unstable();
    ids.dedup();
    let changed = db
        .call(move |s| {
            let mut changed = Vec::new();
            for &id in &ids {
                match s.article(id) {
                    Ok(a) => changed.push((id, a.title)),
                    Err(_) => bail!("No article with id {id}"),
                }
            }
            for &(id, _) in &changed {
                change(s, id)?;
            }
            Ok(changed)
        })
        .await?;
    if json {
        let articles: Vec<_> = changed
            .iter()
            .map(|(id, title)| json!({ "id": id, "title": title }))
            .collect();
        return print_json(&json!({ "updated": changed.len(), "articles": articles }));
    }
    println!(
        "{}",
        message.replace("{}", &plural(changed.len(), "article"))
    );
    for (id, title) in &changed {
        println!("{id}\t{}", line(title));
    }
    Ok(())
}

async fn subscribe(
    db: &Db,
    url: &str,
    folder: Option<i64>,
    title: Option<String>,
    json: bool,
) -> Result<()> {
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
    if !json {
        println!(
            "Subscribed to \"{}\" (feed {id}). Run omafeed refresh to fetch its articles.",
            line(&name)
        );
        for other in &found[1..] {
            println!("Also found: {}\t{}", line(&other.title), other.url);
        }
        return Ok(());
    }
    let others: Vec<_> = found[1..]
        .iter()
        .map(|c| json!({ "title": c.title, "url": c.url }))
        .collect();
    print_json(&json!({ "id": id, "title": name, "url": feed_url, "other_feeds_found": others }))
}

async fn unsubscribe(db: &Db, id: i64, yes: bool, json: bool) -> Result<()> {
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
    if !json {
        println!(
            "Removed \"{}\" and its {total} articles ({starred} starred)",
            line(&title)
        );
        return Ok(());
    }
    print_json(&json!({
        "removed": { "id": id, "title": title, "url": url, "articles": total, "starred": starred }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_accept_seconds_through_weeks() {
        assert_eq!(parse_duration("45s"), Ok(45));
        assert_eq!(parse_duration("90m"), Ok(5_400));
        assert_eq!(parse_duration("24h"), Ok(86_400));
        assert_eq!(parse_duration("2d"), Ok(172_800));
        assert_eq!(parse_duration("1w"), Ok(604_800));
    }

    #[test]
    fn durations_reject_malformed_input_without_panicking() {
        for bad in [
            "",
            "h",
            "24",
            "0h",
            "-5h",
            "+",
            "1.5h",
            "24x",
            "24H",
            " 24h",
            "5é",
            "é",
            "99999999999999999999w",
            "9223372036854775807w",
        ] {
            let err = parse_duration(bad).unwrap_err();
            assert!(err.contains("expected a duration"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn scopes_parse_names_and_ids() {
        assert_eq!(parse_scope("unread"), Ok(Scope::Unread));
        assert_eq!(parse_scope("today"), Ok(Scope::Today));
        assert_eq!(parse_scope("starred"), Ok(Scope::Starred));
        assert_eq!(parse_scope("all"), Ok(Scope::All));
        assert_eq!(parse_scope("feed:12"), Ok(Scope::Feed(12)));
        assert_eq!(parse_scope("folder:3"), Ok(Scope::Folder(3)));
    }

    #[test]
    fn scopes_reject_unknown_names_and_bad_ids() {
        for bad in [
            "",
            "bogus",
            "Feed:1",
            "feed",
            "feed:",
            "feed:x",
            "folder:1.5",
            "feed:1:2",
        ] {
            assert!(parse_scope(bad).is_err(), "{bad:?} was accepted");
        }
        assert!(
            parse_scope("feed:x")
                .unwrap_err()
                .contains("expected a number")
        );
        assert!(parse_scope("bogus").unwrap_err().contains("unknown scope"));
    }

    fn folder(id: i64, parent: Option<i64>, name: &str) -> Folder {
        Folder {
            id,
            parent,
            name: name.into(),
        }
    }

    #[test]
    fn folder_paths_join_parents_and_survive_bad_data() {
        let folders = [
            folder(1, None, "Work"),
            folder(2, Some(1), "Blogs"),
            folder(3, Some(99), "Orphan"),
            folder(4, Some(5), "Loop A"),
            folder(5, Some(4), "Loop B"),
        ];
        assert_eq!(folder_path(&folders, None), "");
        assert_eq!(folder_path(&folders, Some(1)), "Work");
        assert_eq!(folder_path(&folders, Some(2)), "Work/Blogs");
        assert_eq!(folder_path(&folders, Some(3)), "Orphan");
        assert_eq!(folder_path(&folders, Some(404)), "");
        // A cycle cannot happen in a real library, but must not hang the command.
        assert!(!folder_path(&folders, Some(4)).is_empty());
    }

    #[test]
    fn feed_content_is_flattened_to_one_line() {
        assert_eq!(line("a\tb\n  c\r\nd"), "a b c d");
        assert_eq!(line("  "), "");
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(plural(1, "article"), "1 article");
        assert_eq!(plural(0, "article"), "0 articles");
        assert_eq!(plural(2, "article"), "2 articles");
    }
}
