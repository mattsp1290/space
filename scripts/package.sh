#!/usr/bin/env bash
# Package an already-built native development binary. Never signs or publishes.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:?Usage: scripts/package.sh TARGET}
case "$(uname -s):$target" in
  Linux:x86_64-unknown-linux-gnu) platform=linux ;;
  Darwin:aarch64-apple-darwin|Darwin:x86_64-apple-darwin) platform=macos ;;
  *) echo "Unsupported host/target pair: $(uname -s):$target" >&2; exit 2 ;;
esac
binary="target/$target/release/space"
test -x "$binary" || { echo "Build $binary first" >&2; exit 1; }
# Refuse to package without a resolved dependency inventory.
test -f "target/dependency-inventory-$target.json"
mkdir -p dist
stage=$(mktemp -d "${TMPDIR:-/tmp}/space-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT
revision=$(git rev-parse HEAD)
name="space-$target-g0"
root="$stage/$name"
mkdir -p "$root/docs"
cp LICENSE "$root/LICENSE"
cp docs/{platform-support,dependencies,development}.md "$root/docs/"
cp "target/dependency-inventory-$target.json" "$root/dependency-inventory.json"
{
  echo "Space G0 development package; not a validated release"
  echo "Target: $target"
  echo "Revision: $revision"
  echo "Signing: no distribution signing or notarization performed"
  rustc -Vv
  "$binary" --version
} > "$root/BUILD-INFO.txt"
if [[ "$platform" = linux ]]; then
  mkdir -p "$root/bin"
  cp "$binary" "$root/bin/space"
  chmod 755 "$root/bin/space"
  ldd "$binary" > "$root/linked-libraries.txt"
  if grep -q 'not found' "$root/linked-libraries.txt"; then
    echo "Missing runtime libraries; cannot package" >&2; exit 1
  fi
  readelf --version-info "$binary" > "$root/elf-version-info.txt"
  tar -czf "dist/$name.tar.gz" -C "$stage" "$name"
else
  app="$root/Space.app/Contents"
  mkdir -p "$app/MacOS" "$app/Resources"
  cp "$binary" "$app/MacOS/space"
  chmod 755 "$app/MacOS/space"
  cat > "$app/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Space</string>
<key>CFBundleDisplayName</key><string>Space</string>
<key>CFBundleIdentifier</key><string>app.space.disk-explorer.g0</string>
<key>CFBundleExecutable</key><string>space</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
  plutil -lint "$app/Info.plist"
  otool -L "$binary" > "$root/linked-libraries.txt"
  # No Developer ID signature: linker's possible ad-hoc signature is not distribution signing.
  ditto -c -k --sequesterRsrc --keepParent "$root" "dist/$name.zip"
fi
# Only non-executable evidence is eligible for upload. Archives stay local until
# a reviewed third-party license/NOTICE bundle permits binary distribution.
evidence="target/package-evidence-$target"
mkdir -p "$evidence"
cp "$root/BUILD-INFO.txt" "$root/linked-libraries.txt" "$evidence/"
if [[ "$platform" = linux ]]; then cp "$root/elf-version-info.txt" "$evidence/"; fi
printf 'Created local-only development archive (distribution blocked): %s\n'  "dist/$name"
