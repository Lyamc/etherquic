#!/bin/sh
# Build libpyrowave-shared for macOS arm64 from the revisions nix/pyrowave.nix
# pins, with the two patches macOS needs (packaging/pyrowave/macos/):
#   granite-macos.patch  no clock_nanosleep on macOS; skip the shader compiler;
#                        load a Vulkan loader shipped beside the library first
#   volk-macos.patch     the same loader search for volk
#
#   packaging/pyrowave/build-macos.sh [WORK_DIR]
#
# Needs git, cmake, ninja, and Xcode's clang. Prints the library directory.
set -eu
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
work=${1:-$HOME/Build/pyrowave-mac}
src=$work/src
get() { # dir url rev
    mkdir -p "$1"
    git -C "$1" init -q
    git -C "$1" remote add origin "$2" 2>/dev/null || true
    git -C "$1" fetch -q --depth 1 origin "$3"
    git -C "$1" checkout -q -f FETCH_HEAD
}
get "$src" https://github.com/Themaister/pyrowave a0d2e71e98b85f6a23f62407df21a39e9745b875
get "$src/Granite" https://github.com/Themaister/Granite fb178c8080d163419e8d20f10715c61c53c1ec9b
get "$src/Granite/third_party/volk" https://github.com/zeux/volk 47cddf7ed97b94118a08aacb548a411188e016cc
get "$src/Granite/third_party/khronos/vulkan-headers" https://github.com/KhronosGroup/Vulkan-Headers 6802bb4733b63ed5efd3adb308a6c885ef180ea1
git -C "$src/Granite" apply "$here/macos/granite-macos.patch"
git -C "$src/Granite/third_party/volk" apply "$here/macos/volk-macos.patch"
cmake -S "$src" -B "$src/build" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DPYROWAVE_DEVEL=OFF -DPYROWAVE_UTILS=OFF -DCMAKE_OSX_ARCHITECTURES=arm64 >/dev/null
cmake --build "$src/build" --parallel "${JOBS:-4}"
echo "$src/build"
