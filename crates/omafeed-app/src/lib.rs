//! omafeed, the reader window and the `omafeed` command line. `main.rs` only calls [`main`].
mod account;
mod cli;
mod dialogs;
mod ui;

use anyhow::Result;
use clap::Parser;
use gtk::prelude::*;
use omafeed_core::{Db, Paths, Settings, ingest::Refresher};
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// The application ID, also the name of the desktop entry and icon.
pub const APP_ID: &str = "io.github.pch.Omafeed";
/// The library database's file name in the data directory.
pub const DB_FILE: &str = "omafeed.db";

/// `count` and `noun`, plural unless there is one: "1 feed", "2 feeds".
fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

#[derive(Parser)]
#[command(
    name = "omafeed",
    version,
    about = "omafeed — a local RSS reader. Run without a command to open the reader.",
    help_template = "{name} {version}\n{about}\n\n{usage-heading} {usage}\n\n{all-args}{after-help}",
    after_help = "OMAFEED_HOME overrides the data, config, and cache directories for testing."
)]
struct Args {
    #[command(subcommand)]
    command: Option<cli::Command>,
}

/// Run the command line when given a command, or the reader window, and exit on failure.
pub fn main() {
    let args = Args::parse();
    let result = match args.command {
        Some(command) => {
            // Like other command-line tools, stop quietly when the reader of our output
            // goes away (`omafeed articles | head -1`) instead of panicking in `println!`.
            // SAFETY: called before any other thread exists. The standard library offers no
            // safe way to restore the default action, which it sets to "ignore" at startup.
            #[allow(unsafe_code)]
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL)
            };
            cli::run(command)
        }
        None => run_gui(),
    };
    if let Err(e) = result {
        eprintln!("omafeed: {e:#}");
        std::process::exit(1);
    }
}

fn run_gui() -> Result<()> {
    let paths = Paths::discover()?;
    let db = Db::open(paths.data.join(DB_FILE))?;
    let settings = Settings::load(&paths);
    db.set_filters(settings.filters.clone());
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

/// Entry points for tests outside this crate. Not part of any interface.
#[doc(hidden)]
#[cfg(feature = "test-support")]
pub mod test_support {
    /// Drive the real window through its main features; needs a display.
    pub fn desktop_smoke() {
        crate::ui::tests::desktop_smoke();
    }
}
