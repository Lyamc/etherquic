#!/bin/sh
# Collect the Windows codec DLLs for build-msi.ps1 -Codecs, built from source with nix.
# Run on Linux with nix:  sh packaging/windows/codecs.sh OUTDIR
#   libopus-0.dll                 libopus (BSD-3-Clause)
#   libpyrowave-shared-1.dll      nix/pyrowave.nix cross-built for MinGW (MIT)
#   libgcc_s_seh-1.dll, libstdc++-6.dll   GCC runtime (GPL-3 with the GCC runtime exception)
#   libmcfgthread-2.dll           mcfgthread (LGPL-3 with the GCC runtime exception)
set -eu
out=${1:?usage: codecs.sh OUTDIR}
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
# pyrowave.nix lists Linux only; the MinGW build works.
export NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM=1
nb() { nix-build --no-out-link "$@"; }
opus=$(nb '<nixpkgs>' -A pkgsCross.mingwW64.libopus)
pyro=$(nb -E "with import <nixpkgs> {}; pkgsCross.mingwW64.callPackage $root/nix/pyrowave.nix {}")
cp -f "$opus/bin/libopus-0.dll" "$pyro/bin/libpyrowave-shared-1.dll" "$out/"
for dll in libgcc_s_seh-1.dll libstdc++-6.dll libmcfgthread-2.dll; do
    found=""
    for path in "$pyro" $(nix-store -qR "$pyro") $(nix-store -qR "$(nix-store -qd "$pyro")" 2>/dev/null | grep -v '\.drv$' || true); do
        hit=$(find -L "$path" -name "$dll" -type f 2>/dev/null | head -n1)
        if [ -n "$hit" ]; then found=$hit; break; fi
    done
    [ -n "$found" ] || { echo "codecs.sh: $dll not found" >&2; exit 1; }
    cp -fL "$found" "$out/$dll"
done
chmod u+w "$out"/*
# Licence texts from each source.
tar -xOf "$(nb '<nixpkgs>' -A pkgsCross.mingwW64.libopus.src)" --wildcards '*/COPYING' > "$out/LICENSE-opus.txt"
cp -f "$root/packaging/pyrowave/LICENSE" "$out/LICENSE-pyrowave.txt"
gcc=$(nb '<nixpkgs>' -A pkgsCross.mingwW64.stdenv.cc.cc.src)
{ tar -xOf "$gcc" --wildcards '*/COPYING.RUNTIME'; echo; tar -xOf "$gcc" --wildcards '*/COPYING3'; } > "$out/LICENSE-gcc-runtime.txt"
mcf=$(nb '<nixpkgs>' -A pkgsCross.mingwW64.windows.mcfgthreads.src)
if [ -d "$mcf" ]; then
    cat "$mcf"/LICENSE* "$mcf"/COPYING* 2>/dev/null > "$out/LICENSE-mcfgthread.txt"
else
    tar -xOf "$mcf" --wildcards '*/LICENSE*' '*/COPYING*' > "$out/LICENSE-mcfgthread.txt" 2>/dev/null || true
fi
[ -s "$out/LICENSE-mcfgthread.txt" ] || { echo "codecs.sh: no mcfgthread licence" >&2; exit 1; }
ls -l "$out"
