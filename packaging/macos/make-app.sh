#!/bin/sh
# Build etherquic.app and etherquic-<version>-macos-arm64.dmg.
# packaging/macos/make-pkg.sh wraps the same app in a .pkg that can also
# install LaunchDaemon dev.etherquic.host.
#
#   packaging/macos/make-app.sh [PYROWAVE_LIB_DIR] [OUT_DIR]
#
# The app carries the GUI (etherquic), the CLI player (etherquic-play), the
# host service binary (etherquic-host), demo certs, and cjxl for discovery
# thumbnails, with every Homebrew dylib they load. When
# PYROWAVE_LIB_DIR holds libpyrowave-shared (packaging/pyrowave/build-macos.sh)
# the app also carries PyroWave, the Vulkan loader, and Mesa's KosmicKrisp
# Vulkan driver with its ICD manifest, so PyroWave runs without Homebrew.
# MoltenVK is not bundled: PyroWave's encoder refuses it (see src/backend.rs).
#
# The bundle is signed ad hoc, not with a Developer ID, and not notarized.
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
pyrowave=${1:-}
out=${2:-$root/dist/macos}
# VERSION=0.1.7 overrides the root package version, like build-msi.ps1 -Version.
version=${VERSION:-$(awk '
  /^\[package\]/ { inpkg = 1; next }
  inpkg && /^\[/ { exit }
  inpkg && /^version = "/ {
    sub(/^version = "/, "")
    sub(/".*/, "")
    print
    exit
  }
' "$root/Cargo.toml")}
target=${CARGO_TARGET_DIR:-$root/target}
brew=${HOMEBREW_PREFIX:-/opt/homebrew}

(cd "$root/ui" && cargo build --release)
(cd "$root/play" && cargo build --release --bin etherquic-play)
(cd "$root/host" && cargo build --release --bin etherquic-host)

app=$out/etherquic.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources/licenses"
cp "$target/release/etherquic" "$app/Contents/MacOS/etherquic"
cp "$target/release/etherquic-play" "$app/Contents/MacOS/etherquic-play"
cp "$target/release/etherquic-host" "$app/Contents/MacOS/etherquic-host"
cp "$root/certs/cert.pem" "$app/Contents/MacOS/cert.pem"
cp "$root/certs/key.pem" "$app/Contents/MacOS/key.pem"
if [ -x "$brew/bin/cjxl" ]; then
    cp "$brew/bin/cjxl" "$app/Contents/MacOS/cjxl"
    for f in "$brew"/opt/jpeg-xl/LICENSE*; do
        [ -f "$f" ] && cp "$f" "$app/Contents/Resources/licenses/libjxl-$(basename "$f")"
    done
fi

# libopus (BSD) for Opus audio. The media code dlopens it from Frameworks.
if [ -f "$brew/opt/opus/lib/libopus.0.dylib" ]; then
    cp "$(readlink -f "$brew/opt/opus/lib/libopus.0.dylib")" "$app/Contents/Frameworks/libopus.0.dylib"
    cp "$brew/opt/opus/COPYING" "$app/Contents/Resources/licenses/LICENSE-opus.txt"
fi

if [ -n "$pyrowave" ]; then
    cp "$pyrowave/libpyrowave-shared.1.dylib" "$app/Contents/Frameworks/"
    ln -s libpyrowave-shared.1.dylib "$app/Contents/Frameworks/libpyrowave-shared.dylib"
    cp "$root/packaging/pyrowave/LICENSE" "$app/Contents/Resources/licenses/LICENSE-pyrowave.txt"
    loader=$(readlink -f "$brew/lib/libvulkan.1.dylib")
    cp "$loader" "$app/Contents/Frameworks/libvulkan.1.dylib"
    kk_json=$brew/share/vulkan/icd.d/kosmickrisp_mesa_icd.aarch64.json
    kk_lib=$(sed -n 's/.*"library_path": *"\(.*\)".*/\1/p' "$kk_json")
    cp "$kk_lib" "$app/Contents/Frameworks/libvulkan_kosmickrisp.dylib"
    mkdir -p "$app/Contents/Resources/vulkan/icd.d"
    api=$(sed -n 's/.*"api_version": *"\(.*\)".*/\1/p' "$kk_json")
    cat > "$app/Contents/Resources/vulkan/icd.d/kosmickrisp_icd.json" <<JSON
{
    "file_format_version": "1.0.1",
    "ICD": {
        "library_path": "../../../Frameworks/libvulkan_kosmickrisp.dylib",
        "api_version": "$api"
    }
}
JSON
    for f in "$brew"/opt/vulkan-loader/LICENSE* "$brew"/opt/mesa/license.rst; do
        [ -f "$f" ] && cp "$f" "$app/Contents/Resources/licenses/$(basename "$(dirname "$(readlink -f "$f")")")-$(basename "$f")"
    done
fi

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>etherquic</string>
    <key>CFBundleDisplayName</key><string>etherquic</string>
    <key>CFBundleIdentifier</key><string>dev.etherquic.app</string>
    <key>CFBundleExecutable</key><string>etherquic</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSLocalNetworkUsageDescription</key><string>etherquic finds and connects to computers on this network.</string>
    <key>NSMicrophoneUsageDescription</key><string>etherquic can send this Mac's sound to the other computer.</string>
</dict>
</plist>
PLIST

python3 "$here/bundle_dylibs.py" "$app"
codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app"

dmg=$out/etherquic-$version-macos-arm64.dmg
rm -f "$dmg"
stage=$(mktemp -d)
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname "etherquic $version" -srcfolder "$stage" -ov -format UDZO "$dmg"
rm -rf "$stage"
echo "$app"
echo "$dmg"
