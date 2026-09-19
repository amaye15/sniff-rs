#!/usr/bin/env bash
# Install sniff-rs from GitHub releases.
#
#   install.sh [VERSION] [--to DIR]
#
# VERSION defaults to the latest release (leading `v` optional).
# --to defaults to ~/.local/bin (must already be on your PATH).
# Needs only curl + tar (+ unzip for the Windows .zip asset).
set -eu

REPO="amaye15/sniff-rs"
VERSION="${1:-latest}"
DEST="$HOME/.local/bin"
if [ "${2:-}" = "--to" ]; then DEST="$3"; fi
if [ "$VERSION" = "--to" ]; then VERSION="latest"; DEST="$2"; fi

case "$(uname -s)" in
  Linux)  os="unknown-linux-gnu"; ext="tar.gz";;
  Darwin) os="apple-darwin";      ext="tar.gz";;
  MINGW*|MSYS*|CYGWIN*) os="pc-windows-msvc"; ext="zip";;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch="x86_64";;
  arm64|aarch64) arch="aarch64";;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 1;;
esac
TARGET="$arch-$os"

if [ "$VERSION" = "latest" ]; then
  # No jq needed: the /releases/latest URL redirects to /tag/vX.Y.Z.
  final="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")"
  VERSION="${final##*/v}"
fi
VERSION="${VERSION#v}"
ASSET="sniff-rs-$TARGET.$ext"
# SNIFF_RS_BASE overrides the download location (used to test this script
# against local files, e.g. SNIFF_RS_BASE="file:///tmp/fake-release").
BASE="${SNIFF_RS_BASE:-https://github.com/$REPO/releases/download/v$VERSION}"

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
curl -fsSLO "$BASE/$ASSET"
curl -fsSLO "$BASE/SHA256SUMS.txt"
# Match the exact asset line (two spaces separate hash and filename).
if command -v sha256sum >/dev/null; then
  grep -F "  $ASSET" SHA256SUMS.txt | sha256sum -c -
elif command -v shasum >/dev/null; then
  grep -F "  $ASSET" SHA256SUMS.txt | shasum -a 256 -c -
else
  echo "need sha256sum or shasum to verify the download" >&2; exit 1
fi

if [ "$ext" = "zip" ]; then
  command -v unzip >/dev/null || { echo "need unzip for the Windows asset" >&2; exit 1; }
  unzip -q "$ASSET"
else
  tar -xzf "$ASSET"
fi
exe=""; case "$TARGET" in *-windows-*) exe=".exe";; esac
mkdir -p "$DEST"
install -m 755 "sniff-rs-$TARGET/sniff-rs$exe" "$DEST/"
"$DEST/sniff-rs" --version
echo "installed to $DEST/sniff-rs"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "note: $DEST is not on your PATH - add it to use sniff-rs from any terminal" >&2;;
esac
