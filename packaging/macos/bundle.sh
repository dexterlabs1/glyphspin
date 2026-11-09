#!/bin/bash
# Build the universal Glyphspin.app and its zip. Run on macOS from the repo root:
#   packaging/macos/bundle.sh            -> dist/Glyphspin.app, dist/glyphspin-<version>-macos-universal.zip
# Needs the aarch64-apple-darwin and x86_64-apple-darwin Rust targets and python3 (for the icon).
set -euo pipefail
export MACOSX_DEPLOYMENT_TARGET=13.0
version=$(cargo metadata --no-deps --format-version 1 | python3 -c \
  'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="glyphspin"))')
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin
app=dist/Glyphspin.app
rm -rf "$app" && mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/glyphspin" \
  target/aarch64-apple-darwin/release/glyphspin target/x86_64-apple-darwin/release/glyphspin
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
python3 tools/brand.py --icns "$app/Contents/Resources/Glyphspin.icns"
# ad-hoc signature (no Developer ID): Gatekeeper asks once on first launch, see README
codesign --force --deep --sign - "$app"
codesign --verify --verbose "$app"
lipo -info "$app/Contents/MacOS/glyphspin"
zip="dist/glyphspin-$version-macos-universal.zip"
rm -f "$zip"
(cd dist && ditto -c -k --keepParent Glyphspin.app "$(basename "$zip")")
echo "ZIP=$zip"
