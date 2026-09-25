mod dialogs;
mod ui;
use anyhow::{Context, Result};
use gtk::prelude::*;
use omafeed_core::{
    Db, Paths, Settings,
    fetch::{Progress, Refresher},
};

fn main() {
    if let Err(e) = run() {
        eprintln!("Omafeed: {e:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let paths = Paths::discover()?;
    let db = Db::open(paths.data.join("omafeed.db"))?;
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(async {
            match args[1].as_str() {
                "import"=>{let text=std::fs::read_to_string(args.get(2).context("Usage: omafeed import FILE.opml")?)?;let r=db.call(move|s|s.import(&text)).await?;println!("Imported {}, skipped {}, invalid {}",r.added,r.skipped,r.invalid.len());for e in r.invalid{eprintln!("{e}");}}
                "export"=>{let path=args.get(2).context("Usage: omafeed export FILE.opml")?;std::fs::write(path,db.call(|s|s.export()).await?)?;println!("Exported {path}");}
                "refresh"=>{let (tx,rx)=async_channel::unbounded();let logger=tokio::spawn(async move{while let Ok(event)=rx.recv().await{match event{Progress::Started(n)=>println!("Refreshing {n} feeds"),Progress::Feed{title,error,done,total}=>println!("[{done}/{total}] {title}: {}",error.unwrap_or_else(||"OK".into())),Progress::Finished{total,failed}=>println!("Finished: {total} feeds, {failed} failed")}}});Refresher::new()?.with_cache(paths.cache.clone()).refresh(db.clone(),true,Settings::load(&paths)?.refresh_minutes,tx).await?;logger.await?;}
                "status"=>{let lib=db.call(|s|s.library()).await?;println!("{} feeds · {} folders · {} unread · {} starred",lib.feeds.len(),lib.folders.len(),lib.unread,lib.starred);for f in lib.feeds{if let Some(e)=f.error{println!("{}: {e}",f.title);}}}
                "--version"=>println!("Omafeed {}",env!("CARGO_PKG_VERSION")),
                "--help"|"-h"=>println!("Omafeed — a local RSS reader\n\n omafeed   Open reader\n omafeed import FILE.opml\n omafeed export FILE.opml\n omafeed refresh\n omafeed status\n\nOMAFEED_HOME overrides data/config/cache directories for testing."),
                _=>anyhow::bail!("Unknown command. Run omafeed --help"),
            } Ok(())
        });
    }
    let settings = Settings::load(&paths)?;
    let runtime = std::sync::Arc::new(tokio::runtime::Runtime::new()?);
    let refresher = Refresher::new()?.with_cache(paths.cache.clone());
    let app = adw::Application::builder()
        .application_id("io.github.pch.Omafeed")
        .build();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        ui::Ui::build(
            app,
            db.clone(),
            paths.clone(),
            settings.clone(),
            runtime.clone(),
            refresher.clone(),
        );
    });
    app.run_with_args::<&str>(&[]);
    Ok(())
}
