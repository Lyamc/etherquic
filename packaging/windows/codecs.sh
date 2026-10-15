#!/bin/sh
# Collect the Windows codec DLLs for build-msi.ps1 -Codecs, built from source with nix.
# Run on Linux with nix:  sh packaging/windows/codecs.sh OUTDIR
#   libopus-0.dll   libopus (BSD-3-Clause), self-contained (KERNEL32 and msvcrt only)
# PyroWave for Windows is not built here yet: nix/pyrowave.nix does not link
# for MinGW (no static winpthread). setup still copies libpyrowave-shared-1.dll
# and its MinGW runtime when a payload has them, and the host only announces
# PyroWave when a real encode works.
set -eu
out=${1:?usage: codecs.sh OUTDIR}
mkdir -p "$out"
opus=$(nix-build --no-out-link '<nixpkgs>' -A pkgsCross.mingwW64.libopus)
cp -f "$opus/bin/libopus-0.dll" "$out/"
chmod u+w "$out/libopus-0.dll"
tar -xOf "$(nix-build --no-out-link '<nixpkgs>' -A pkgsCross.mingwW64.libopus.src)" --wildcards '*/COPYING' > "$out/LICENSE-opus.txt"
ls -l "$out"
