#!/usr/bin/env bash
# Stage the CPU Vulkan fallback for the Windows MSI into OUT (default /tmp/eq-vulkan-cpu):
#   vulkan_lvp.dll    Mesa lavapipe, x64 MSVC build from pal1000/mesa-dist-win (MIT, static CRT)
#   vulkan-1.dll      lvp-shim/vulkan_shim.c built with MinGW: exports vkGetInstanceProcAddr
#                     and forwards to lavapipe beside it (Granite on Windows only loads vulkan-1.dll)
#   LICENSE-mesa.txt
# Then: build-msi.ps1 -VulkanCpu OUT. Needs nix, curl.
set -euo pipefail
MESA=26.2.4
SHA=351fc8c8b695878ffb3eaa044b3ead08672a48b1a045e3c3e3975811df0f6695
OUT=${1:-/tmp/eq-vulkan-cpu}
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$OUT"
curl -fsSL -o "$work/mesa.7z" "https://github.com/pal1000/mesa-dist-win/releases/download/$MESA/mesa3d-$MESA-release-msvc.7z"
echo "$SHA  $work/mesa.7z" | sha256sum -c -
nix-shell -p p7zip --run "7z e -y -o'$work' '$work/mesa.7z' x64/vulkan_lvp.dll" >/dev/null
cp "$work/vulkan_lvp.dll" "$OUT/"
nix-shell -p pkgsCross.mingwW64.buildPackages.gcc --run \
  "x86_64-w64-mingw32-gcc -O2 -shared -static-libgcc -s -o '$OUT/vulkan-1.dll' '$here/lvp-shim/vulkan_shim.c'"
curl -fsSL -o "$work/license.rst" "https://gitlab.freedesktop.org/mesa/mesa/-/raw/mesa-$MESA/docs/license.rst"
{
  echo "vulkan_lvp.dll is Mesa $MESA lavapipe, built by https://github.com/pal1000/mesa-dist-win"
  echo "(release $MESA, mesa3d-$MESA-release-msvc.7z, sha256 $SHA)."
  echo "It statically links LLVM (Apache License 2.0 with LLVM Exceptions)."
  echo "vulkan-1.dll is etherquic's shim (packaging/windows/lvp-shim), same licence as etherquic."
  echo
  cat "$work/license.rst"
} > "$OUT/LICENSE-mesa.txt"
ls -l "$OUT"
