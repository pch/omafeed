//! Command-line operations that run without opening a window.
mod json;

use crate::{DB_FILE, account};
use anyhow::{Context, Result, bail};
use omafeed_core::{
    Db, Error, Paths, Settings,
    content::{one_line, readable},
    ingest::{Progress, Refresher},
    model::{ArticleId, FeedId, FolderId, Library, ParseScopeError, Query, Scope, StatePatch},
    reading,
    store::PAGE_SIZE,
    subscriptions::{self, Removal},
    sync::{self, miniflux::Client},
};
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

/// How `articles` and `search` filter and print their list.
#[derive(clap::Args)]
pub struct List {
    /// Disable typo, plural and prefix expansion
    #[arg(long)]
    exact: bool,
    /// Only unread articles
    #[arg(long)]
    unread: bool,
    /// Only articles published this recently, e.g. 90m, 24h, 2d or 1w
    #[arg(long, value_parser = parse_duration)]
    since: Option<i64>,
    #[arg(long, default_value_t = 50, value_parser = parse_limit)]
    limit: usize,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Print JSON, with a preview of each article, instead of text
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
        /// Search within the scope, as `search` does
        #[arg(long)]
        search: Option<String>,
        #[command(flatten)]
        list: List,
    },
    /// Search articles for words, e.g. omafeed search "agentic engineering"
    Search {
        /// Words to find, quoted or not; an article must match all of them, in any order.
        /// Like the app, a word also matches close misspellings and its plural or singular,
        /// and the last word matches longer words that start with it; --exact turns this off.
        /// Put `--` before a word that starts with a dash: omafeed search -- -word
        #[arg(required = true, value_parser = parse_query)]
        query: Vec<String>,
        /// unread, today, starred, all, feed:ID or folder:ID
        #[arg(long, default_value = "all", value_parser = parse_scope)]
        scope: Scope,
        #[command(flatten)]
        list: List,
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
    /// Sync with the connected Miniflux account now, as `refresh` does while one is connected
    Sync {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Connect a Miniflux account, show it, or disconnect it
    #[command(subcommand)]
    Account(AccountCommand),
}

#[derive(clap::Subcommand)]
pub enum AccountCommand {
    /// Mirror a Miniflux account, reading its API key from standard input
    ///
    /// Miniflux then fetches the feeds and decides which feeds and folders there are, and
    /// omafeed syncs read and star state with it. Your feeds Miniflux does not have are
    /// subscribed there, and articles it has too keep their read and star state. The API key
    /// (in Miniflux: Settings, API Keys) is read from standard input, or from
    /// OMAFEED_MINIFLUX_TOKEN, and kept in the keyring.
    Connect {
        /// The server's address, e.g. https://miniflux.example.org
        server: String,
    },
    /// Show the connected account and its last sync
    Status,
    /// Stop mirroring the account; its feeds stay, as local ones
    ///
    /// Its feeds, folders and articles stay, with their read and star state, and omafeed
    /// fetches the feeds itself again. Changes not yet sent are sent first if the server can
    /// be reached.
    Disconnect,
}

pub fn run(command: Command) -> Result<()> {
    let paths = Paths::discover()?;
    let db = Db::open(paths.data.join(DB_FILE))?;
    let settings = Settings::load(&paths);
    db.set_filters(settings.filters);
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        match command {
            Command::Import { file } => import(&db, &file).await,
            Command::Export { file } => export(&db, &file).await,
            Command::Refresh { json } => refresh(&db, &paths, settings.refresh_minutes, json).await,
            Command::Status => status(&db).await,
            Command::Discover { url, json } => discover(&url, json).await,
            Command::Feeds { json } => feeds(&db, json).await,
            Command::Articles {
                scope,
                search,
                list,
            } => articles(&db, scope, search.unwrap_or_default(), list).await,
            Command::Search { query, scope, list } => {
                articles(&db, scope, query.join(" "), list).await
            }
            Command::Article { id, html, json } => article(&db, id, html, json).await,
            Command::Read(i) => set_state(&db, i, "Marked {} read", StatePatch::read(true)).await,
            Command::Unread(i) => {
                set_state(&db, i, "Marked {} unread", StatePatch::read(false)).await
            }
            Command::Star(i) => set_state(&db, i, "Starred {}", StatePatch::starred(true)).await,
            Command::Unstar(i) => {
                set_state(&db, i, "Unstarred {}", StatePatch::starred(false)).await
            }
            Command::Subscribe {
                url,
                folder,
                title,
                json,
            } => subscribe(&db, &url, folder, title, json).await,
            Command::Unsubscribe { id, yes, json } => unsubscribe(&db, id, yes, json).await,
            Command::Sync { json } => sync_now(&db, json).await,
            Command::Account(AccountCommand::Connect { server }) => connect(&db, &server).await,
            Command::Account(AccountCommand::Status) => account_status(&db).await,
            Command::Account(AccountCommand::Disconnect) => disconnect(&db).await,
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

async fn refresh(db: &Db, paths: &Paths, minutes: u32, json: bool) -> Result<()> {
    if let Some(client) = account::client(db).await? {
        return sync_as_refresh(db, &client, json).await;
    }
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
                    feeds.push(json::Refreshed { title, error });
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
    Refresher::new()?
        .with_cache(paths.cache.clone())
        .refresh(db.clone(), omafeed_core::ingest::Which::All, minutes, tx)
        .await?;
    let (feeds, totals) = logger.await?;
    if json {
        let (total, failed) = totals.unwrap_or_default();
        json::print(&json::Refresh {
            total,
            failed,
            feeds,
        })?;
    }
    Ok(())
}

/// `refresh` while an account is connected: a sync, reported as `refresh` reports, with
/// each feed's state as Miniflux last fetched it.
async fn sync_as_refresh(db: &Db, client: &Client, json: bool) -> Result<()> {
    let report = sync::sync(db, client).await?;
    let lib = db.call(|s| s.library()).await?;
    if !json {
        print_report(&report);
        for f in &lib.feeds {
            if let Some(e) = &f.error {
                println!("{}: {e}", f.title);
            }
        }
        return Ok(());
    }
    let feeds: Vec<json::Refreshed> = lib
        .feeds
        .into_iter()
        .map(|f| json::Refreshed {
            title: f.title,
            error: f.error,
        })
        .collect();
    json::print(&json::Refresh {
        total: feeds.len(),
        failed: feeds.iter().filter(|f| f.error.is_some()).count(),
        feeds,
    })
}

fn print_report(report: &sync::Report) {
    println!(
        "Synced {} with Miniflux: {}, {} sent, {} read or starred on the server",
        plural(report.feeds, "feed"),
        plural(report.new_articles, "new article"),
        plural(report.sent, "change"),
        plural(report.state_changes, "article")
    );
    if report.subscribed > 0 {
        println!(
            "Subscribed Miniflux to {}",
            plural(report.subscribed, "feed")
        );
    }
    for (title, reason) in &report.refused {
        eprintln!("{title}: {reason}");
    }
}

async fn sync_now(db: &Db, json: bool) -> Result<()> {
    let Some(client) = account::client(db).await? else {
        bail!("No Miniflux account is connected; connect one with `omafeed account connect`");
    };
    let report = sync::sync(db, &client).await?;
    if json {
        return json::print(&json::Synced::from(&report));
    }
    print_report(&report);
    Ok(())
}

/// The API key from the environment, or else a line of standard input, asking for it first if
/// someone is typing.
fn read_token() -> Result<String> {
    use std::io::{IsTerminal, Write};
    if let Ok(token) = std::env::var(account::TOKEN_VAR)
        && !token.trim().is_empty()
    {
        return Ok(token);
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("Miniflux API key: ");
        std::io::stderr().flush()?;
    }
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let token = line.trim().to_owned();
    if token.is_empty() {
        bail!(
            "No API key given; pipe it in, or set {}",
            account::TOKEN_VAR
        );
    }
    Ok(token)
}

async fn connect(db: &Db, server: &str) -> Result<()> {
    let token = read_token()?;
    let client = Client::new(server, &token)?;
    let user = client.me().await?;
    account::save_token(client.server(), &token).await?;
    let report = match sync::connect(db, &client).await {
        Ok(report) => report,
        Err(e) => {
            // A first sync that failed leaves the account connected, and needs the key.
            if db.call(|s| s.account()).await?.is_none() {
                let _ = account::forget_token(client.server()).await;
            }
            return Err(e);
        }
    };
    println!("Connected to {} as {}", client.server(), user.username);
    print_report(&report);
    Ok(())
}

async fn account_status(db: &Db) -> Result<()> {
    let Some(account) = db.call(|s| s.account()).await? else {
        println!("No Miniflux account is connected");
        return Ok(());
    };
    println!("Connected to {} as {}", account.server, account.username);
    let last = account.last_sync.map_or_else(
        || "never".to_owned(),
        |t| {
            chrono::DateTime::from_timestamp(t, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default()
        },
    );
    println!("Last sync: {last}");
    if let Some(error) = account.error {
        println!("Last sync failed: {error}");
    }
    if account.unsent > 0 {
        println!(
            "Not sent yet: {}",
            plural(account.unsent as usize, "change")
        );
    }
    Ok(())
}

async fn disconnect(db: &Db) -> Result<()> {
    let Some(connected) = db.call(|s| s.account()).await? else {
        bail!("No Miniflux account is connected");
    };
    // Without the key, or with the server gone, unsent changes are dropped.
    let client = account::client(db).await.ok().flatten();
    let dropped = sync::disconnect(db, client.as_ref()).await?;
    if let Err(e) = account::forget_token(&connected.server).await {
        eprintln!("omafeed: could not remove the API key from the keyring: {e:#}");
    }
    println!(
        "Disconnected from {}; its feeds are local now, and omafeed fetches them itself",
        connected.server
    );
    if dropped > 0 {
        println!(
            "Dropped {} not sent to Miniflux",
            plural(dropped as usize, "change")
        );
    }
    Ok(())
}

async fn status(db: &Db) -> Result<()> {
    let lib = db.call(|s| s.library()).await?;
    println!("{}", totals(&lib));
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
        return json::print(&json::Discovered {
            feeds: found.iter().map(json::Found::from).collect(),
        });
    }
    for feed in found {
        println!("{}\t{}", feed.title, feed.url);
    }
    Ok(())
}

fn parse_scope(text: &str) -> Result<Scope, String> {
    text.parse().map_err(|e: ParseScopeError| e.to_string())
}

/// A search needs at least one word; an empty one would match every article.
fn parse_query(text: &str) -> Result<String, String> {
    let words = one_line(text);
    if words.is_empty() {
        return Err("search needs at least one word".into());
    }
    Ok(words)
}

/// A page size of at least one article.
fn parse_limit(text: &str) -> Result<usize, String> {
    match text.parse() {
        Ok(0) => Err("must be at least 1".into()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("`{text}` is not a whole number")),
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

/// The local date, matching `--scope today` and the reader's own dates.
fn day(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// A folder's name with its parents, such as `Work/Blogs`; empty for no folder.
fn folder_path(lib: &Library, id: Option<FolderId>) -> String {
    lib.folder_path(id)
        .iter()
        .map(|f| one_line(&f.name))
        .collect::<Vec<_>>()
        .join("/")
}

/// The first line of `status` and `feeds`.
fn totals(lib: &Library) -> String {
    format!(
        "{} feeds · {} folders · {} unread · {} starred",
        lib.feeds.len(),
        lib.folders.len(),
        lib.unread,
        lib.starred
    )
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

async fn feeds(db: &Db, json: bool) -> Result<()> {
    let lib = db.call(|s| s.library()).await?;
    if !json {
        println!("{}", totals(&lib));
        if !lib.folders.is_empty() {
            // Sorted by full path, so a folder comes right before its subfolders.
            let mut paths: Vec<_> = lib
                .folders
                .iter()
                .map(|f| (folder_path(&lib, Some(f.id)), f.id))
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
                    one_line(&f.title),
                    folder_path(&lib, f.folder),
                    f.error.as_deref().map(one_line).unwrap_or_default()
                );
            }
        }
        return Ok(());
    }
    json::print(&json::Feeds::from(&lib))
}

/// Newest-first articles, reading past one page when `limit` or `since` asks for more.
async fn articles(db: &Db, scope: Scope, search: String, list: List) -> Result<()> {
    let List {
        exact,
        unread,
        since,
        limit,
        offset,
        json,
    } = list;
    let query = Query {
        scope,
        search,
        unread_only: unread,
        exact,
        offset,
    };
    let since = since.map(|seconds| chrono::Utc::now().timestamp() - seconds);
    // All pages come from one snapshot, so the reader's writes cannot shift them.
    let mut found = db
        .call(move |s| {
            s.snapshot(|s| {
                let mut found = Vec::new();
                let mut offset = query.offset;
                loop {
                    let page = s.articles(&Query {
                        offset,
                        ..query.clone()
                    })?;
                    let full = page.len() == PAGE_SIZE;
                    // Pages are ordered by date, so one article older than the cutoff ends it.
                    let reached_cutoff =
                        since.is_some_and(|c| page.last().is_some_and(|a| a.published < c));
                    offset += page.len();
                    found.extend(
                        page.into_iter()
                            .filter(|a| since.is_none_or(|c| a.published >= c)),
                    );
                    // One article beyond `limit` is enough to know the list was cut short.
                    if !full || reached_cutoff || found.len() > limit {
                        return Ok(found);
                    }
                }
            })
        })
        .await?;
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
                one_line(&a.feed_title),
                one_line(&a.title)
            );
        }
        if found.is_empty() {
            eprintln!("No articles match.");
        }
        if truncated {
            eprintln!("More articles match; raise --limit or use --offset.");
        }
        return Ok(());
    }
    json::print(&json::Articles {
        count: found.len(),
        truncated,
        articles: found.iter().map(json::ArticleItem::listed).collect(),
    })
}

async fn article(db: &Db, id: i64, html: bool, json: bool) -> Result<()> {
    let id = ArticleId(id);
    let a = db
        .call(move |s| s.article_if_exists(id))
        .await?
        .ok_or(Error::ArticleNotFound(id))?;
    if json {
        let item = if html {
            json::ArticleItem::with_html(&a)
        } else {
            json::ArticleItem::with_text(&a, readable(&a.html))
        };
        return json::print(&item);
    }
    let mut details = vec![one_line(&a.feed_title)];
    if !a.author.is_empty() {
        details.push(one_line(&a.author));
    }
    details.push(day(a.published));
    details.push(if a.read { "read" } else { "unread" }.into());
    if a.starred {
        details.push("starred".into());
    }
    println!("{}\n{}", one_line(&a.title), details.join(" · "));
    if !a.url.is_empty() {
        println!("{}", a.url);
    }
    println!("\n{}", if html { a.html } else { readable(&a.html) });
    Ok(())
}

/// Update every requested article atomically, then name the articles changed.
async fn set_state(
    db: &Db,
    Ids { ids, json }: Ids,
    message: &str,
    patch: StatePatch,
) -> Result<()> {
    let ids: Vec<ArticleId> = ids.into_iter().map(ArticleId).collect();
    let changed = reading::change(db, ids, patch).await?;
    if json {
        return json::print(&json::Updated::from(changed.as_slice()));
    }
    println!(
        "{}",
        message.replace("{}", &plural(changed.len(), "article"))
    );
    for (id, title) in &changed {
        println!("{id}\t{}", one_line(title));
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
    // Say so before looking the feed up.
    if db.call(|s| s.account()).await?.is_some() {
        return Err(Error::Managed.into());
    }
    let found = Refresher::new()?.discover(url).await?;
    let Some(pick) = found.first() else {
        bail!("No feed found at {url}");
    };
    let name = title.unwrap_or_else(|| pick.title.clone());
    let feed_url = pick.url.clone();
    let id =
        subscriptions::subscribe(db, name.clone(), feed_url.clone(), folder.map(FolderId)).await?;
    if !json {
        println!(
            "Subscribed to \"{}\" (feed {id}). Run omafeed refresh to fetch its articles.",
            one_line(&name)
        );
        for other in &found[1..] {
            println!("Also found: {}\t{}", one_line(&other.title), other.url);
        }
        return Ok(());
    }
    json::print(&json::Subscribed {
        id,
        title: name,
        url: feed_url,
        other_feeds_found: found[1..].iter().map(json::Found::from).collect(),
    })
}

async fn unsubscribe(db: &Db, id: i64, yes: bool, json: bool) -> Result<()> {
    let removal = subscriptions::unsubscribe(db, FeedId(id), yes).await?;
    let Removal {
        title,
        articles: total,
        starred,
        ..
    } = &removal;
    if !yes {
        bail!(
            "Not removed. This would permanently delete \"{title}\" and its {total} articles ({starred} starred). Run again with --yes to confirm."
        );
    }
    if !json {
        println!(
            "Removed \"{}\" and its {total} articles ({starred} starred)",
            one_line(title)
        );
        return Ok(());
    }
    json::print(&json::Unsubscribed::from(removal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use omafeed_core::model::Folder;

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
    fn a_search_needs_a_word() {
        assert_eq!(parse_query("agentic"), Ok("agentic".into()));
        assert_eq!(
            parse_query("  agentic \t engineering\n"),
            Ok("agentic engineering".into())
        );
        for empty in ["", " ", "\t\n"] {
            assert!(
                parse_query(empty)
                    .unwrap_err()
                    .contains("at least one word")
            );
        }
    }

    #[test]
    fn scopes_parse_names_and_ids() {
        assert_eq!(parse_scope("unread"), Ok(Scope::Unread));
        assert_eq!(parse_scope("today"), Ok(Scope::Today));
        assert_eq!(parse_scope("starred"), Ok(Scope::Starred));
        assert_eq!(parse_scope("all"), Ok(Scope::All));
        assert_eq!(parse_scope("feed:12"), Ok(Scope::Feed(FeedId(12))));
        assert_eq!(parse_scope("folder:3"), Ok(Scope::Folder(FolderId(3))));
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
            id: FolderId(id),
            parent: parent.map(FolderId),
            name: name.into(),
        }
    }

    #[test]
    fn folder_paths_join_parents_and_survive_bad_data() {
        let folders = Library {
            folders: vec![
                folder(1, None, "Work"),
                folder(2, Some(1), "Blogs"),
                folder(3, Some(99), "Orphan"),
                folder(4, Some(5), "Loop A"),
                folder(5, Some(4), "Loop B"),
            ],
            ..Default::default()
        };
        assert_eq!(folder_path(&folders, None), "");
        assert_eq!(folder_path(&folders, Some(FolderId(1))), "Work");
        assert_eq!(folder_path(&folders, Some(FolderId(2))), "Work/Blogs");
        assert_eq!(folder_path(&folders, Some(FolderId(3))), "Orphan");
        assert_eq!(folder_path(&folders, Some(FolderId(404))), "");
        // A cycle cannot happen in a real library, but must not hang the command.
        assert!(!folder_path(&folders, Some(FolderId(4))).is_empty());
    }

    #[test]
    fn feed_content_is_flattened_to_one_line() {
        assert_eq!(one_line("a\tb\n  c\r\nd"), "a b c d");
        assert_eq!(one_line("  "), "");
        // `day` prints the local date, so derive the timestamp from a local date
        // instead of asserting the UTC date of epoch 0.
        let local = chrono::Local
            .with_ymd_and_hms(2026, 1, 2, 3, 0, 0)
            .single()
            .expect("an unambiguous local time");
        assert_eq!(day(local.timestamp()), "2026-01-02");
        assert_eq!(plural(1, "article"), "1 article");
        assert_eq!(plural(0, "article"), "0 articles");
        assert_eq!(plural(2, "article"), "2 articles");
    }
}
