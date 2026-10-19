#!/bin/sh
# Build dist/etherquic-<version>-x86_64-linux.tar.gz
#
#   nix-shell nix/shell.nix --run packaging/linux/make-tarball.sh
#
# VERSION=0.1.10 overrides the root package version.
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
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
out=${1:-$root/dist}
target=${CARGO_TARGET_DIR:-$root/target}

if ! command -v pkg-config >/dev/null 2>&1; then
    echo "run this from nix-shell nix/shell.nix" >&2
    exit 1
fi

(cd "$root" && cargo build --release --manifest-path ui/Cargo.toml --bin etherquic)
(cd "$root" && cargo build --release --manifest-path play/Cargo.toml --bin etherquic-play)

libdir=
old_ifs=$IFS
IFS=:
for d in ${LIBRARY_PATH-} ${LD_LIBRARY_PATH-}; do
    if [ -n "$d" ] && [ -f "$d/libpyrowave-shared.so.1" ]; then
        libdir=$d
        break
    fi
    if [ -n "$d" ] && [ -f "$d/libpyrowave-shared.so" ]; then
        libdir=$d
        break
    fi
done
IFS=$old_ifs
if [ -z "$libdir" ]; then
    echo "libpyrowave-shared.so was not on LIBRARY_PATH" >&2
    exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$target/release/etherquic" "$stage/etherquic"
cp "$target/release/etherquic-play" "$stage/etherquic-play"
# Release profile keeps debuginfo for local builds. The tarball is stripped.
strip "$stage/etherquic" "$stage/etherquic-play"
cp "$root/ui/assets/etherquic.svg" "$stage/etherquic.svg"
cp "$root/ui/assets/etherquic-host.svg" "$stage/etherquic-host.svg"
cp "$root/ui/assets/etherquic-player.svg" "$stage/etherquic-player.svg"
cp "$here/etherquic.desktop" "$stage/etherquic.desktop"
cp "$here/etherquic-autostart.desktop" "$stage/etherquic-autostart.desktop"
cp "$root/certs/cert.pem" "$stage/cert.pem"
cp "$root/certs/key.pem" "$stage/key.pem"
if [ -f "$libdir/libpyrowave-shared.so.1.0.0" ]; then
    cp "$libdir/libpyrowave-shared.so.1.0.0" "$stage/libpyrowave-shared.so.1.0.0"
    ln -s libpyrowave-shared.so.1.0.0 "$stage/libpyrowave-shared.so.1"
    ln -s libpyrowave-shared.so.1.0.0 "$stage/libpyrowave-shared.so"
elif [ -f "$libdir/libpyrowave-shared.so.1" ]; then
    cp -P "$libdir/libpyrowave-shared.so.1" "$stage/"
    ln -s libpyrowave-shared.so.1 "$stage/libpyrowave-shared.so"
else
    cp "$libdir/libpyrowave-shared.so" "$stage/"
fi
cp "$root/packaging/pyrowave/LICENSE" "$stage/LICENSE-pyrowave.txt"

mkdir -p "$out"
archive=$out/etherquic-$version-x86_64-linux.tar.gz
tar -C "$stage" -czf "$archive" .
echo "wrote $archive"
ls -l "$archive"
