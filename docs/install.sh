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
      x86_64|amd64) asset="dbopt-linux-x86_64.tar.gz" ;;
      *) echo "dbopt: unsupported Linux arch '$arch'. See https://github.com/$REPO/releases" >&2; exit 1 ;;
    esac ;;
  Darwin)
    case "$arch" in
      arm64|aarch64) asset="dbopt-macos-arm64.tar.gz" ;;
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
# The archive carries both shipped binaries: `dbopt` is the linter and `dbopt-backend`
# is the local app. Older archives contained only the app, named `dbopt` — fall back
# to that so this installer keeps working against an older release.
mv "$tmp/dbopt" "$DEST/$BIN"
chmod +x "$DEST/$BIN"
if [ -f "$tmp/dbopt-backend" ]; then
  mv "$tmp/dbopt-backend" "$DEST/dbopt-backend"
  chmod +x "$DEST/dbopt-backend"
  echo "dbopt: installed to $DEST/$BIN (linter) and $DEST/dbopt-backend (app)"
  echo "dbopt: try  dbopt lint ./db   ·  or run dbopt-backend and open http://127.0.0.1:3690"
else
  echo "dbopt: installed to $DEST/$BIN"
fi

# Desktop integration on Linux so dbopt is launchable from the apps menu
# (not just a terminal). macOS uses the .app/.dmg instead, so skip it there.
if [ "$os" = "Linux" ]; then
  apps_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
  mkdir -p "$apps_dir" "$icon_dir"
  # Brand icon (best-effort; the menu entry still works without it).
  curl -fsSL "https://dbopt.org/logo.svg" -o "$icon_dir/dbopt.svg" 2>/dev/null || true
  cat > "$apps_dir/dbopt.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=dbopt
Comment=Local-first database performance optimizer
Exec=$DEST/$BIN
Icon=dbopt
Terminal=true
Categories=Development;Database;
Keywords=sql;database;performance;index;
DESKTOP
  update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
  echo "dbopt: added a desktop menu entry (search 'dbopt' in your apps)."
fi

case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "dbopt: add $DEST to your PATH, e.g.  export PATH=\"$DEST:\$PATH\"" ;;
esac
echo "dbopt: run 'dbopt' and open http://127.0.0.1:3690"
