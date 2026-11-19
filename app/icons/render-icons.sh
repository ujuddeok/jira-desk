#!/bin/bash
# Renders app/icons/jira-desk.svg into macos/JiraDesk.icns (16 to 512 pt, @1x and @2x) with
# macOS's own SVG renderer (AppKit) and iconutil: nothing to install beyond Xcode's tools.
#   app/icons/render-icons.sh
set -euo pipefail
cd "$(dirname "$0")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
swiftc -O render.swift -o "$work/render"
set_dir="$work/JiraDesk.iconset"
mkdir "$set_dir"
for pt in 16 32 128 256 512; do
  "$work/render" jira-desk.svg "$set_dir/icon_${pt}x${pt}.png" "$pt"
  "$work/render" jira-desk.svg "$set_dir/icon_${pt}x${pt}@2x.png" "$((pt * 2))"
done
iconutil -c icns "$set_dir" -o macos/JiraDesk.icns
echo "wrote app/icons/macos/JiraDesk.icns"
