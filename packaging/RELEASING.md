# Releasing omafeed

A stable `vMAJOR.MINOR.PATCH` tag on `main` that matches `Cargo.toml` builds and publishes a
GitHub release (`.github/workflows/release.yml`). Nothing is published unless CI passes on the
tagged commit, every package installs and runs in its checks, and the release notes exist.

## Downloads

| System | Asset | Requirements |
| --- | --- | --- |
| Omarchy | `omafeed` from the Omarchy package repository | Updated by Omarchy's bot after each release |
| Arch Linux x86_64 | `omafeed-VERSION-1-x86_64.pkg.tar.zst` | Current Arch packages |
| Arch Linux (any architecture) | `PKGBUILD`, and `omafeed-VERSION-arch-recipe.tar.gz` with its `.SRCINFO` | Builds the tagged source with `makepkg -si` |
| Debian / Ubuntu x86_64 and ARM | `omafeed_VERSION_amd64.deb`, `omafeed_VERSION_arm64.deb` | Debian 13+, Ubuntu 26.04+ |
| Fedora x86_64 and ARM | `omafeed-VERSION-1.x86_64.rpm`, `omafeed-VERSION-1.aarch64.rpm` | Fedora 43+ |
| Linux archive | `omafeed-VERSION-x86_64-linux.tar.gz`, `omafeed-VERSION-aarch64-linux.tar.gz` | glibc 2.34+, `gtk4`, `libadwaita` 1.6+, `webkitgtk-6.0` |

`SHA256SUMS` lists every asset. Never rename these assets: the README, the Omarchy recipe and
users' scripts download them by name.

The archives are built on Debian 13 (`packaging/linux/build-archive.sh`, with a pinned Rust
toolchain), the oldest system the packages support, and hold the `/usr` tree `make install`
writes. [native-packages](https://github.com/crmne/native-packages), configured in
`native-packages.yaml`, turns them into the DEB and RPM packages; their dependencies come from
the libraries the binary links, through the `libraries` map. A new linked library fails the
build until it is mapped. `.github/workflows/packaging.yml` builds both and installs, runs and
removes the packages in clean Debian 13, Ubuntu 26.04, Fedora 43 and Fedora 44 containers on
x86_64 and ARM (`packaging/linux/verify.sh`). Pull requests that touch packaging or
dependencies run it too.

The Arch package is built in an Arch container from the tag's source archive with
`packaging/omafeed/PKGBUILD`, its checksum filled in, then installed, run and removed. There is
no ARM Arch package, as Arch has no ARM container image; ARM users build the `PKGBUILD`.

## Omarchy

Omarchy builds `omafeed` from
[`pkgbuilds/omafeed`](https://github.com/omacom/omarchy-pkgs/tree/master/pkgbuilds/omafeed) in
omacom/omarchy-pkgs, from the tag's source archive. Its `.omarchy/package.json` watches this
repository's releases, so Omarchy's bot opens and merges each update within hours of a
release. Keep the `vX.Y.Z` tags and the source archive URL stable. A packaging change at the
same version, such as a new dependency, needs a pull request there that bumps `pkgrel`; keep
its `depends` in step with `packaging/omafeed/PKGBUILD`.

## Making a release

1. Bump `version` under `[workspace.package]` in `Cargo.toml`, update `Cargo.lock`
   (`cargo check`), and add a `<release>` entry to `data/io.github.pch.Omafeed.metainfo.xml`.
2. Rename `packaging/release-notes/unreleased.md` to `vX.Y.Z.md` and finish it (see below).
   Start a new, empty `unreleased.md`.
3. Commit as `Release X.Y.Z`, push `main`, check the commit is on `origin/main`, then push an
   annotated tag: `git tag -a vX.Y.Z -m "omafeed X.Y.Z" && git push origin vX.Y.Z`. Never put
   `[skip ci]` in a release commit: GitHub applies it to the tag push too.
4. Wait for the Release workflow. Its last job downloads every published asset and checks it
   against `SHA256SUMS`. Read the published release page.
5. Check that Omarchy's bot picks up the release in omacom/omarchy-pkgs.

A release is done when the page, its downloads and the Omarchy package are all at the new
version.

## Rehearsing and rebuilding

Run **Release** from Actions on `main` with an existing tag. With **publish** off (the
default) it builds, checks and keeps every package as an Actions artifact without touching
the release. With **publish** on it then attaches the packages to the tag's release,
replacing assets with the same names (including `SHA256SUMS`) and keeping others, and
replaces the description with the notes file. A manual run builds the tag's exact source but
uses the workflows, packaging scripts and release notes of the branch it runs on, so packaging
can be fixed, or notes written, for an existing tag without moving it. `verify.sh` expects the
files `make install` writes since 0.2.4.

## Writing release notes

Release notes are written and reviewed, not generated from commits. The workflow refuses a
tag whose `packaging/release-notes/vX.Y.Z.md` is missing or empty, and publishes the file
verbatim. Pull requests add their user-visible changes to `unreleased.md` as they land; the
release commit turns that draft into the release's notes.

1. Read the changes from the previous tag to the release commit, including diffs and pull
   requests, so reverted or partial work is not announced.
2. Open with a **TL;DR:** of the main changes and what they mean for someone using omafeed,
   then a **Download:** line linking that release's assets directly.
3. Group the rest under **New**, **Improved**, **Fixed**, **Security**, **Changed** or
   **Removed**, only the sections that apply. Lead each item with a bold result, then a short
   explanation with menu paths and pull request or issue links. Leave out refactors and CI
   work unless they change something for users.
4. Add **Upgrade notes** for schema versions older releases refuse, settings or command-line
   changes, and **Thanks** for contributors and reporters, when there are any.
5. End with **Requirements** and a **Full changelog** link comparing the tags.

`packaging/release-notes/v0.2.4.md` shows the structure. Keep notes proportional to the
change, and check every claim against the tagged code.

To correct a published description without rebuilding:

```sh
gh release edit vX.Y.Z --notes-file packaging/release-notes/vX.Y.Z.md
```
