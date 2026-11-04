## New

- **Packages for Debian, Ubuntu and Fedora** ([#32](https://github.com/pch/omafeed/pull/32), [#31](https://github.com/pch/omafeed/issues/31)). Each release now includes `.deb` and `.rpm` packages for x86_64 and ARM (aarch64), for Debian 13+, Ubuntu 26.04+ and Fedora 43+. They install the app with its dependencies and are checked by installing, running and removing them on each of those systems.
- **A prebuilt Arch Linux package.** Each release includes `omafeed-VERSION-1-x86_64.pkg.tar.zst`, built from the release's `PKGBUILD`, for `sudo pacman -U` without compiling.

## Changed

- **The x86_64 archive is built on Debian 13,** like the ARM one, so it runs on Debian, Ubuntu, Fedora and Arch alike (glibc 2.34 or newer, `libadwaita` 1.6 or newer).
- **Every release has written release notes** in `packaging/release-notes/`, which replace `CHANGELOG.md`. Notes for earlier releases have been added to their GitHub releases.
