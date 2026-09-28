mod cli;
mod dialogs;
mod ui;

use anyhow::Result;
use clap::Parser;
use gtk::prelude::*;
use omafeed_core::{Db, Paths, Settings, fetch::Refresher};
use std::{cell::RefCell, rc::Rc, sync::Arc};

pub const APP_ID: &str = "io.github.pch.Omafeed";
pub const DB_FILE: &str = "omafeed.db";

#[derive(Parser)]
#[command(
    name = "omafeed",
    version,
    about = "Omafeed — a local RSS reader. Run without a command to open the reader.",
    help_template = "{name} {version}\n{about}\n\n{usage-heading} {usage}\n\n{all-args}{after-help}",
    after_help = "OMAFEED_HOME overrides the data, config, and cache directories for testing."
)]
struct Args {
    #[command(subcommand)]
    command: Option<cli::Command>,
}

fn main() {
    let args = Args::parse();
    let result = match args.command {
        Some(command) => cli::run(command),
        None => run_gui(),
    };
    if let Err(e) = result {
        eprintln!("Omafeed: {e:#}");
        std::process::exit(1);
    }
}

fn run_gui() -> Result<()> {
    let paths = Paths::discover()?;
    let db = Db::open(paths.data.join(DB_FILE))?;
    let settings = Settings::load(&paths);
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let refresher = Refresher::new()?.with_cache(paths.cache.clone());
    let app = adw::Application::builder().application_id(APP_ID).build();
    // The application owns the window state; widget handlers only hold weak references.
    let main_ui: RefCell<Option<Rc<ui::Ui>>> = RefCell::default();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        *main_ui.borrow_mut() = Some(ui::Ui::build(
            app,
            db.clone(),
            paths.clone(),
            settings.clone(),
            runtime.clone(),
            refresher.clone(),
        ));
    });
    // Arguments were already handled by clap; GTK must not parse them again.
    app.run_with_args::<&str>(&[]);
    Ok(())
}
