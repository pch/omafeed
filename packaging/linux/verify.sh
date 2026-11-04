#!/usr/bin/env bash
# Install, run and remove the DEB or RPM in a clean Debian, Ubuntu or Fedora
# container, with the packages mounted at /packages. VERSION is the version
# they were built for. The checks expect the layout `make install` has had
# since 0.2.4; a rebuild of an older tag may need them relaxed.
set -euo pipefail
: "${VERSION:?set VERSION}"

if command -v apt-get >/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y --no-install-recommends /packages/*.deb
  remove=(apt-get remove -y omafeed)
else
  dnf install -y --setopt=install_weak_deps=False /packages/*.rpm
  remove=(dnf remove -y omafeed)
fi

test "$(omafeed --version)" = "omafeed $VERSION"
if ldd /usr/bin/omafeed | grep 'not found'; then exit 1; fi
test -f /usr/share/applications/io.github.pch.Omafeed.desktop
test -f /usr/share/metainfo/io.github.pch.Omafeed.metainfo.xml
test -f /usr/share/icons/hicolor/scalable/apps/io.github.pch.Omafeed.svg
test -f /usr/share/licenses/omafeed/LICENSE

# The command line opens (and creates) a library without a display.
OMAFEED_HOME=$(mktemp -d) omafeed status

# Removing the package leaves the user's library alone.
mkdir -p /root/.local/share/omafeed
echo preserve > /root/.local/share/omafeed/packaging-test
"${remove[@]}"
test "$(cat /root/.local/share/omafeed/packaging-test)" = preserve
test ! -e /usr/bin/omafeed
