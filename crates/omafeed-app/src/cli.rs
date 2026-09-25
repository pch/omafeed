//! Command-line operations that run without opening a window.
use crate::DB_FILE;
use anyhow::{Context, Result};
use omafeed_core::{
    Db, Paths, Settings,
    fetch::{Progress, Refresher},
};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Import subscriptions from an OPML file
    Import { file: PathBuf },
    /// Export subscriptions to an OPML file
    Export { file: PathBuf },
    /// Refresh every feed now
    Refresh,
    /// Show library totals and feed errors
    Status,
    /// List the feeds a website advertises
    Discover { url: String },
}

pub fn run(command: Command) -> Result<()> {
    let paths = Paths::discover()?;
    let db = Db::open(paths.data.join(DB_FILE))?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        match command {
            Command::Import { file } => import(&db, &file).await,
            Command::Export { file } => export(&db, &file).await,
            Command::Refresh => refresh(&db, &paths).await,
            Command::Status => status(&db).await,
            Command::Discover { url } => discover(&url).await,
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

async fn refresh(db: &Db, paths: &Paths) -> Result<()> {
    let (tx, rx) = async_channel::unbounded();
    let logger = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                Progress::Started(n) => println!("Refreshing {n} feeds"),
                Progress::Feed {
                    title,
                    error,
                    done,
                    total,
                } => println!(
                    "[{done}/{total}] {title}: {}",
                    error.as_deref().unwrap_or("OK")
                ),
                Progress::Finished { total, failed } => {
                    println!("Finished: {total} feeds, {failed} failed")
                }
            }
        }
    });
    let minutes = Settings::load(paths).refresh_minutes;
    Refresher::new()?
        .with_cache(paths.cache.clone())
        .refresh(db.clone(), true, minutes, tx)
        .await?;
    logger.await?;
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

async fn discover(url: &str) -> Result<()> {
    for feed in Refresher::new()?.discover(url).await? {
        println!("{}\t{}", feed.title, feed.url);
    }
    Ok(())
}
