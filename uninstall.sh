#!/usr/bin/env bash
set -euo pipefail
for base in "$HOME/.local" /usr/local; do
  S=""; [[ $base == /usr/local ]] && S=sudo
  $S rm -f "$base/bin/kindlyterm" \
           "$base/share/applications/kindlyterm.desktop" \
           "$base/share/icons/hicolor/scalable/apps/kindlyterm.svg" \
           "$base"/share/icons/hicolor/*/apps/kindlyterm.png 2>/dev/null || true
done
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
echo "kindlyTerm removed (config in ~/.config/kindlyterm kept)."
