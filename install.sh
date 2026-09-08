#!/usr/bin/env bash
# Build kindterm in release mode and install it for the current user:
#   binary   -> ~/.local/bin/kindterm
#   icon     -> ~/.local/share/icons/hicolor/scalable/apps/kindterm.svg
#   launcher -> ~/.local/share/applications/kindterm.desktop
# Pass --system to install under /usr/local instead (needs sudo).
set -euo pipefail
cd "$(dirname "$0")"

if [[ "${1:-}" == "--uninstall" ]]; then exec ./uninstall.sh; fi
if [[ "${1:-}" == "--system" ]]; then
  PREFIX=/usr/local
  SUDO=sudo
  APPS=/usr/local/share/applications
  ICONS=/usr/local/share/icons/hicolor
else
  PREFIX="$HOME/.local"
  SUDO=""
  APPS="$HOME/.local/share/applications"
  ICONS="$HOME/.local/share/icons/hicolor"
fi

echo "==> building release binary"
cargo build --release

echo "==> installing to $PREFIX"
$SUDO install -Dm755 target/release/kindterm "$PREFIX/bin/kindterm"
$SUDO install -Dm644 assets/kindterm.svg "$ICONS/scalable/apps/kindterm.svg"
# Point Exec at the absolute path so it works even if ~/.local/bin is not on PATH.
sed "s|^Exec=kindterm|Exec=$PREFIX/bin/kindterm|; s|^TryExec=kindterm|TryExec=$PREFIX/bin/kindterm|" \
  assets/kindterm.desktop > /tmp/kindterm.desktop.$$
$SUDO install -Dm644 /tmp/kindterm.desktop.$$ "$APPS/kindterm.desktop"
rm -f /tmp/kindterm.desktop.$$

# Optional PNG sizes for launchers that don't scale SVG.
if command -v rsvg-convert >/dev/null 2>&1; then
  for sz in 48 64 128 256; do
    tmp=/tmp/kindterm-$sz.png
    rsvg-convert -w $sz -h $sz assets/kindterm.svg -o "$tmp"
    $SUDO install -Dm644 "$tmp" "$ICONS/${sz}x${sz}/apps/kindterm.png"
    rm -f "$tmp"
  done
fi

echo "==> refreshing desktop database and icon cache"
command -v update-desktop-database >/dev/null 2>&1 && $SUDO update-desktop-database "$APPS" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && $SUDO gtk-update-icon-cache -f -t "$ICONS" 2>/dev/null || true

echo
echo "Installed. Press Super and type 'term' — kindterm shows up in GNOME search."
echo "Uninstall with: ./install.sh --uninstall"
