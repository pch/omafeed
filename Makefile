PREFIX ?= $(HOME)/.local

.PHONY: build install uninstall check
build:
	cargo build --release --locked
install: build
	install -Dm755 target/release/omafeed $(DESTDIR)$(PREFIX)/bin/omafeed
	install -Dm644 data/io.github.pch.Omafeed.desktop $(DESTDIR)$(PREFIX)/share/applications/io.github.pch.Omafeed.desktop
	install -Dm644 data/io.github.pch.Omafeed.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/io.github.pch.Omafeed.svg
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/omafeed/LICENSE
uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/omafeed
	rm -f $(DESTDIR)$(PREFIX)/share/applications/io.github.pch.Omafeed.desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/io.github.pch.Omafeed.svg
	rm -f $(DESTDIR)$(PREFIX)/share/licenses/omafeed/LICENSE
check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo test --workspace --locked
