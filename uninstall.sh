#!/usr/bin/env bash
set -euo pipefail
for base in "$HOME/.local" /usr/local; do
  S=""; [[ $base == /usr/local ]] && S=sudo
  $S rm -f "$base/bin/kindterm" \
           "$base/share/applications/kindterm.desktop" \
           "$base/share/icons/hicolor/scalable/apps/kindterm.svg" \
           "$base"/share/icons/hicolor/*/apps/kindterm.png 2>/dev/null || true
done
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
echo "kindterm removed (config in ~/.config/kindterm kept)."
