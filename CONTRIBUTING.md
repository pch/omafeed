# Contributing

Thanks for helping with omafeed. This file covers how to build and test it, and the rules that
keep existing users' scripts, settings and libraries working. [ARCHITECTURE.md](ARCHITECTURE.md)
explains how the code is laid out.

## Setup

You need Rust (the version in `rust-version` in `Cargo.toml`, or newer), a C compiler,
`pkg-config`, GTK 4, libadwaita 1.6 or newer, and WebKitGTK 6.0. On Arch Linux:

```sh
sudo pacman -S --needed rust gtk4 libadwaita webkitgtk-6.0 pkgconf
cargo run -p omafeed                 # the window
cargo run -p omafeed -- feeds        # a command
```

Set `OMAFEED_HOME=/tmp/omafeed-test` to keep your own library, settings and cache out of the way.

## Checks

```sh
make check
```

This runs `cargo fmt --check`, Clippy with warnings as errors, every test, and the
documentation build, all with every feature. CI runs the same, plus:

- the desktop smoke test, headless:
  `cargo test -p omafeed --features test-support --test desktop_smoke -- --ignored`
  (needs a display; in a container use `xvfb-run` and see `.github/workflows/ci.yml`);
- a build with the minimum supported Rust version;
- an Arch package build, and `cargo deny` for licenses and advisories.

Lints are shared in `[workspace.lints]` in the root `Cargo.toml`. Prefer fixing a lint to
allowing it; an `#[allow]` needs a reason next to it. `omafeed-core` forbids `unsafe` code and
requires documentation on every public item.

## Tests

Tests use temporary databases and local HTTP servers, never the internet.

- **Core behaviour** goes in `crates/omafeed-core/tests/` (or a unit test next to the code).
- **Command behaviour** goes in `crates/omafeed-app/tests/cli.rs`, which runs the real binary.
- **Window behaviour** goes in `crates/omafeed-app/src/ui/view.rs` and `ui/changes.rs` when it
  can be tested without GTK, and in the desktop smoke test (`ui/tests.rs`) when it needs it.

Existing tests are part of the contract. Move or adapt them; do not delete coverage.

## Compatibility rules

### Command-line output

Scripts and LLM agents parse what `omafeed` prints. Its stdout, stderr, exit codes, help text
and JSON must stay byte for byte the same unless a change is intended and noted in the
[release notes](packaging/release-notes/unreleased.md).

`crates/omafeed-app/tests/cli_output.rs` compares every command with a transcript in
`tests/snapshots/`. If one fails, your change altered output. If that was the point, rewrite the
transcripts and review the diff before committing:

```sh
OMAFEED_BLESS=1 cargo test -p omafeed --test cli_output
git diff crates/omafeed-app/tests/snapshots
```

JSON is built from the types in `crates/omafeed-app/src/cli/json.rs` and printed through
`serde_json::Value`, which sorts keys. Do not print those types any other way, and do not
enable serde_json's `preserve_order` feature.

### Settings

`settings.toml` keeps its flat keys and value forms. New settings need a default, and an
invalid or out-of-range value must fall back or be clamped rather than reject the file.
`crates/omafeed-core/tests/files.rs` checks files written by earlier versions; the fixtures
in `tests/fixtures/settings/` are frozen.

### The library database

Migrations live in `crates/omafeed-core/src/store/schema.rs`. Append new ones; never change,
reorder or remove an existing one, since libraries in the wild have already run it. A database
from a newer version is refused, so adding a migration means older omafeed versions will no
longer open the library: say so in the release notes.

Migrations that recompute text call `content::plain`. If you change what `plain` produces,
copy the current version into the migration module and make the existing migrations call
that frozen copy, so upgrading an old library still gives the text those versions expected.

The database fixtures in `crates/omafeed-core/tests/fixtures/` are what earlier versions
wrote. Never edit them; add a new one for a new schema version.

## Pull requests

- One change per pull request. Keep moving files apart from changing behaviour.
- Commit messages are imperative and say what changed for the user or the code, for example
  "Find imported folders by their trimmed names".
- Update README.md, ARCHITECTURE.md, doc comments and
  [packaging/release-notes/unreleased.md](packaging/release-notes/unreleased.md) (user-visible
  changes, as described in [packaging/RELEASING.md](packaging/RELEASING.md#writing-release-notes))
  in the same pull request.
- CI must pass before merging.
