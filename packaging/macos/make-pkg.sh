#!/bin/sh
# Build etherquic-<version>-macos-arm64.pkg from the .app.
#
#   packaging/macos/make-pkg.sh [PYROWAVE_LIB_DIR] [OUT_DIR]
#
# The product has two choices. The app is required. The host LaunchDaemon is
# selected by default (same question the Windows MSI asks). Clearing that
# choice installs the app only; the app then prompts to install the service.
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
pyrowave=${1:-}
out=${2:-$root/dist/macos}

"$here/make-app.sh" "$pyrowave" "$out"

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

app=$out/etherquic.app
stage=$(mktemp -d)
mkdir -p "$stage/Applications"
cp -R "$app" "$stage/Applications/"

scripts=$(mktemp -d)
cp "$here/service-scripts/postinstall" "$scripts/postinstall"
chmod +x "$scripts/postinstall"

app_pkg=$out/etherquic-app.pkg
svc_pkg=$out/etherquic-host-service.pkg
pkgbuild --root "$stage" \
    --identifier dev.etherquic.app \
    --version "$version" \
    --install-location / \
    "$app_pkg"

pkgbuild --nopayload \
    --identifier dev.etherquic.host \
    --version "$version" \
    --scripts "$scripts" \
    "$svc_pkg"

dist=$(mktemp)
sed "s/@VERSION@/$version/g" "$here/distribution.xml" > "$dist"
productbuild --distribution "$dist" --package-path "$out" \
    "$out/etherquic-$version-macos-arm64.pkg"

rm -rf "$stage" "$scripts" "$dist"
rm -f "$app_pkg" "$svc_pkg"
echo "$out/etherquic-$version-macos-arm64.pkg"
