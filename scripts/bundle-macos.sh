#!/bin/bash
# Builds "target/release/Jira Desk.app" (the app and the jdesk command side by side in
# Contents/MacOS, the icon in Contents/Resources), signed ad hoc. Universal (Apple silicon + Intel) with --universal.
#
#   scripts/bundle-macos.sh [--universal]
#   cp -R "target/release/Jira Desk.app" /Applications/
#
# Needs Xcode with the Metal toolchain (GPUI compiles its shaders at build time):
#   xcodebuild -downloadComponent MetalToolchain
set -euo pipefail
cd "$(dirname "$0")/.."

if ! xcrun -f metal >/dev/null 2>&1; then
  echo "The Metal compiler is missing. With Xcode installed, run:" >&2
  echo "  sudo xcode-select -s /Applications/Xcode.app && xcodebuild -downloadComponent MetalToolchain" >&2
  exit 2
fi

APP="target/release/Jira Desk.app"
if [[ "${1:-}" == "--universal" ]]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --target "$t" -p jira-desk -p jdesk-core
  done
  BIN=target/release/universal
  mkdir -p "$BIN"
  for b in jira-desk jdesk; do
    lipo -create -output "$BIN/$b" \
      "target/aarch64-apple-darwin/release/$b" "target/x86_64-apple-darwin/release/$b"
  done
else
  cargo build --release -p jira-desk -p jdesk-core
  BIN=target/release
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp macos/Info.plist "$APP/Contents/Info.plist"
cp "$BIN/jira-desk" "$BIN/jdesk" "$APP/Contents/MacOS/"
# The icon (app/icons/jira-desk.svg, rendered by app/icons/render-icons.sh).
cp app/icons/macos/JiraDesk.icns "$APP/Contents/Resources/JiraDesk.icns"
# Finder and the Dock cache icons by bundle path; touching the bundle makes them look again.
touch "$APP"
# Sealed last, after every file is in place.
codesign --force --deep --sign - "$APP"
echo "Built $APP"
