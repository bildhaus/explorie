#!/usr/bin/env bash
set -euo pipefail

repository=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=$(node -p "require('$repository/package.json').version")
target=${EXPLORIE_MACOS_TARGET:-aarch64-apple-darwin}
: "${EXPLORIE_PLUGIN_CATALOG:?Set the official plugin catalog used to build this application}"
node "$repository/scripts/package-plugins.mjs" --verify-catalog "$EXPLORIE_PLUGIN_CATALOG" --target "$target"
identity=${APPLE_SIGNING_IDENTITY:--}
team_id=${APPLE_TEAM_ID:-}
assets="$repository/apps/desktop/native-assets"
binary="$repository/target/$target/release/explorie-gpui"
bundle_root="$repository/target/release/bundle"
app="$bundle_root/macos/explorie.app"
dmg_root="$bundle_root/dmg-root"
dmg="$bundle_root/dmg/explorie_${version}_${target}.dmg"
rclone="$assets/binaries/rclone-$target"
sevenzip="$assets/binaries/7zip-$target/7zz"
helper="$assets/macos/build/explorie-mountd"
icon_source="$assets/icons/icon.png"
iconset="$bundle_root/explorie.iconset"
generated_icon="$bundle_root/explorie.icns"

case "$app:$dmg_root" in
  "$repository"/target/release/bundle/*:"$repository"/target/release/bundle/*) ;;
  *) echo "Package paths escaped the release bundle root." >&2; exit 1 ;;
esac

case "$target" in
  aarch64-apple-darwin) architecture=arm64 ;;
  x86_64-apple-darwin) architecture=x86_64 ;;
  *) echo "Unsupported macOS package target: $target" >&2; exit 1 ;;
esac

test -x "$binary"
test -x "$rclone"
test -x "$sevenzip"
test -f "$icon_source"
test -f "$assets/resources/rclone-COPYING"
test -f "$assets/resources/pixelarticons-LICENSE.txt"
test -f "$assets/resources/assimp-LICENSE.txt"
test -f "$assets/resources/7zip-LICENSE.txt"
test -f "$assets/resources/7zip-COPYING.txt"
test -f "$assets/resources/7zip-NOTICE.txt"
test -f "$assets/macos/MountDaemon.m"
test -f "$assets/macos/com.omershatz.explorie.mountd.plist"

mkdir -p "$(dirname "$helper")"
xcrun clang \
  -arch "$architecture" \
  -fobjc-arc \
  -fblocks \
  -mmacosx-version-min=13.0 \
  -framework Foundation \
  -framework Security \
  "-DEXPLORIE_TEAM_ID=\"$team_id\"" \
  "$assets/macos/MountDaemon.m" \
  -o "$helper"

rm -rf "$app" "$dmg_root" "$iconset"
rm -f "$generated_icon"
mkdir -p \
  "$app/Contents/MacOS" \
  "$app/Contents/Resources/licenses" \
  "$app/Contents/Resources/7zip" \
  "$app/Contents/Library/LaunchDaemons" \
  "$bundle_root/dmg" \
  "$dmg_root" \
  "$iconset"

make_icon() {
  local filename=$1
  local pixels=$2
  sips -s format png -z "$pixels" "$pixels" "$icon_source" \
    --out "$iconset/$filename" >/dev/null
}

make_icon icon_16x16.png 16
make_icon icon_16x16@2x.png 32
make_icon icon_32x32.png 32
make_icon icon_32x32@2x.png 64
make_icon icon_128x128.png 128
make_icon icon_128x128@2x.png 256
make_icon icon_256x256.png 256
make_icon icon_256x256@2x.png 512
make_icon icon_512x512.png 512
make_icon icon_512x512@2x.png 1024
iconutil -c icns "$iconset" -o "$generated_icon"
test -s "$generated_icon"

install -m 755 "$binary" "$app/Contents/MacOS/explorie-gpui"
install -m 755 "$rclone" "$app/Contents/MacOS/rclone"
install -m 755 "$sevenzip" "$app/Contents/Resources/7zip/7zz"
node "$repository/scripts/package-plugins.mjs" --verify-catalog "$EXPLORIE_PLUGIN_CATALOG" --target "$target" --stage-directory "$app/Contents/Resources/plugins"
for notice in 7zip-LICENSE.txt 7zip-COPYING.txt 7zip-NOTICE.txt; do
  install -m 644 "$assets/resources/$notice" "$app/Contents/Resources/licenses/$notice"
done
node "$repository/scripts/prepare-7zip.mjs" --stage-source "$app/Contents/Resources/licenses"
node "$repository/scripts/smoke-7zip.mjs" "$app/Contents/Resources/7zip/7zz"
install -m 755 "$helper" "$app/Contents/Resources/explorie-mountd"
install -m 644 "$generated_icon" "$app/Contents/Resources/icon.icns"
install -m 644 "$assets/resources/rclone-COPYING" "$app/Contents/Resources/licenses/rclone-COPYING"
install -m 644 \
  "$assets/resources/pixelarticons-LICENSE.txt" \
  "$app/Contents/Resources/licenses/pixelarticons-LICENSE.txt"
install -m 644 \
  "$assets/resources/assimp-LICENSE.txt" \
  "$app/Contents/Resources/licenses/assimp-LICENSE.txt"
install -m 644 \
  "$assets/macos/com.omershatz.explorie.mountd.plist" \
  "$app/Contents/Library/LaunchDaemons/com.omershatz.explorie.mountd.plist"
rm -rf "$iconset"
rm -f "$generated_icon"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>explorie</string>
  <key>CFBundleExecutable</key><string>explorie-gpui</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>CFBundleIdentifier</key><string>com.omershatz.explorie</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>explorie</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Folder</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array><string>public.folder</string></array>
    </dict>
  </array>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Library/LaunchDaemons/com.omershatz.explorie.mountd.plist"

sign() {
  local identifier=$1
  local path=$2
  local arguments=(--force --options runtime --identifier "$identifier" --sign "$identity")
  if [ "$identity" != "-" ]; then arguments+=(--timestamp); fi
  codesign "${arguments[@]}" "$path"
}

sign com.omershatz.explorie.mountd "$app/Contents/Resources/explorie-mountd"
sign com.omershatz.explorie.rclone "$app/Contents/MacOS/rclone"
sign com.omershatz.explorie.sevenzip "$app/Contents/Resources/7zip/7zz"
sign com.omershatz.explorie "$app/Contents/MacOS/explorie-gpui"
sign com.omershatz.explorie "$app"
codesign --verify --deep --strict --verbose=2 "$app"

ditto "$app" "$dmg_root/explorie.app"
ln -s /Applications "$dmg_root/Applications"
rm -f "$dmg"
hdiutil create \
  -volname explorie \
  -srcfolder "$dmg_root" \
  -ov \
  -format UDZO \
  "$dmg"
test -s "$dmg"
disk_image_signing=(--force --sign "$identity")
if [ "$identity" != "-" ]; then disk_image_signing+=(--timestamp); fi
codesign "${disk_image_signing[@]}" "$dmg"
codesign --verify --verbose=2 "$dmg"
printf '%s\n' "$dmg"
