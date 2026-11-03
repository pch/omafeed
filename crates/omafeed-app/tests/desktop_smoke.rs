//! Opt-in desktop verification on the process main thread.
fn main() {
    // Keep ordinary `cargo test` usable without a display, like an ignored libtest test.
    if !std::env::args().any(|arg| arg == "--ignored" || arg == "--include-ignored") {
        println!("desktop_smoke skipped; run with --ignored in a desktop session");
        return;
    }
    omafeed::test_support::desktop_smoke();
    println!("desktop_smoke passed");
}
