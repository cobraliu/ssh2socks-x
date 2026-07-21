#!/usr/bin/env bash
# Build the Fyne desktop client (ssh2socks-x/desktop).
#
# Requires: Go >= 1.26, a C compiler (CGO), and — on Linux — the GL/X11 dev
# headers Fyne links against (Debian/Ubuntu: libgl1-mesa-dev xorg-dev).
#
# By default builds a host-native binary via `go build`. If the `fyne` CLI is
# installed it instead produces a packaged bundle (.app / .exe / tarball) for
# the host OS. For cross-OS bundles use fyne-cross separately.
set -euo pipefail

cd "$(dirname "$0")/../desktop"

export CGO_ENABLED=1
# The dev machine / some CI images export GOFLAGS=-mod=mod, which is rejected in
# go.work workspace mode. Clear it so the workspace's readonly default applies.
export GOFLAGS=

if command -v fyne >/dev/null 2>&1; then
	echo ">> packaging with fyne CLI"
	fyne package --name ssh2socks --app-id local.ssh2socks.desktop "$@"
	echo ">> packaged in $(pwd)"
else
	out="ssh2socks-desktop"
	case "${GOOS:-$(go env GOOS)}" in windows) out="$out.exe" ;; esac
	echo ">> fyne CLI not found — plain go build (install: go install fyne.io/fyne/v2/cmd/fyne@latest)"
	go build -o "$out" .
	echo ">> built $(pwd)/$out"
fi
