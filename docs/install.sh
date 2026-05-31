#!/bin/sh
# dbopt installer for Linux and macOS.
#   curl -fsSL https://dbopt.org/install.sh | sh
# Downloads the latest release binary for your OS/arch and installs it as `dbopt`.
set -eu

REPO="singhpratech/dbopt"
BIN="dbopt"
DEST="${DBOPT_BIN_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64|amd64) asset="sqlopt-linux-x86_64.tar.gz" ;;
      *) echo "dbopt: unsupported Linux arch '$arch'. See https://github.com/$REPO/releases" >&2; exit 1 ;;
    esac ;;
  Darwin)
    case "$arch" in
      arm64|aarch64) asset="sqlopt-macos-arm64.tar.gz" ;;
      *) echo "dbopt: macOS requires Apple Silicon (arm64); Intel Macs are not supported." >&2; exit 1 ;;
    esac ;;
  *)
    echo "dbopt: unsupported OS '$os'. On Windows use the PowerShell installer:" >&2
    echo "  irm https://dbopt.org/install.ps1 | iex" >&2
    exit 1 ;;
esac

url="https://github.com/$REPO/releases/latest/download/$asset"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "dbopt: downloading $asset ..."
curl -fsSL "$url" -o "$tmp/pkg.tar.gz"
tar -xzf "$tmp/pkg.tar.gz" -C "$tmp"

mkdir -p "$DEST"
mv "$tmp/sqlopt" "$DEST/$BIN"
chmod +x "$DEST/$BIN"

echo "dbopt: installed to $DEST/$BIN"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "dbopt: add $DEST to your PATH, e.g.  export PATH=\"$DEST:\$PATH\"" ;;
esac
echo "dbopt: run 'dbopt' and open http://127.0.0.1:3690"
