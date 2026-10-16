#!/bin/sh
# Wraps a pitstop-app binary in "Piaz Switch.app", so it opens from Finder, sits in
# Login Items, and keeps one name in keychain prompts. On a Mac, from the
# repository root:
#
#   cargo build --release -p pitstop-app
#   crates/pitstop-app/macos/bundle.sh target/release/pitstop-app
#
# Piaz Switch.app is written next to the binary, and its path printed.
set -eu

bin=${1:-target/release/pitstop-app}
here=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../../Cargo.toml" | head -n 1)
app="$(dirname "$bin")/Piaz Switch.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/pitstop-app"
cp "$here/../icons/icon.icns" "$app/Contents/Resources/icon.icns"

# LSUIElement: a menu bar app with no Dock icon until its window opens.
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Piaz Switch</string>
  <key>CFBundleDisplayName</key><string>Piaz Switch</string>
  <key>CFBundleIdentifier</key><string>io.github.benpiaz.pitstop</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleExecutable</key><string>pitstop-app</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

# An ad-hoc signature, so the keychain can remember "Always Allow" for it.
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || true
echo "$app"
