#!/usr/bin/env bash
# Build a universal (arm64 + x86_64) "Downloader.app" and zip it into dist/.
set -euo pipefail
VERSION=$(grep -m1 '^version' gui/Cargo.toml | cut -d '"' -f2)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin
mkdir -p dist/universal
for b in downloader dlx downloader-host; do
  lipo -create -output dist/universal/$b \
    target/aarch64-apple-darwin/release/$b target/x86_64-apple-darwin/release/$b
done
lipo -info dist/universal/downloader

APP="dist/Downloader.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp dist/universal/downloader dist/universal/dlx dist/universal/downloader-host "$APP/Contents/MacOS/"
cp scripts/Downloader.icns "$APP/Contents/Resources/"
cp docs/desktop-controls.zh-TW.md "$APP/Contents/Resources/"
cp docs/validation-0.4.1.md docs/small-window-validation.zh-TW.md "$APP/Contents/Resources/"
sed "s/__VERSION__/$VERSION/g" scripts/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
# ad-hoc signature so Gatekeeper reports "unidentified developer" rather than "damaged"
codesign --force --deep --sign - "$APP" || true

cd dist
ditto -c -k --sequesterRsrc --keepParent "Downloader.app" downloader-macos-universal-app.zip
mkdir -p downloader-macos-universal
cp -r ../extension downloader-macos-universal/
cp universal/downloader universal/dlx universal/downloader-host ../README.md ../LICENSE downloader-macos-universal/
cp ../docs/desktop-controls.zh-TW.md downloader-macos-universal/
cp ../docs/validation-0.4.1.md ../docs/small-window-validation.zh-TW.md downloader-macos-universal/
tar czf downloader-macos-universal.tar.gz downloader-macos-universal
ls -la
