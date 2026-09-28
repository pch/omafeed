//! Opt-in desktop verification on the process main thread.
#![allow(dead_code)]
#[path = "../src/dialogs/mod.rs"]
mod dialogs;
#[path = "../src/ui/mod.rs"]
mod ui;

pub const APP_ID: &str = "io.github.pch.Omafeed";

fn main() {
    // Keep ordinary `cargo test` usable without a display, like an ignored libtest test.
    if !std::env::args().any(|arg| arg == "--ignored" || arg == "--include-ignored") {
        println!("desktop_smoke skipped; run with --ignored in a desktop session");
        return;
    }
    ui::tests::desktop_smoke();
    println!("desktop_smoke passed");
}
