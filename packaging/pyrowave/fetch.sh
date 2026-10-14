#!/bin/sh
# Build Themaister/pyrowave and stage the shared library for the installer.
# The codec is MIT, so the built library is bundled. Upstream publishes no
# binary. The notice in LICENSE must ship next to the library.
set -eu

pin=a0d2e71e98b85f6a23f62407df21a39e9745b875
granite=fb178c8080d163419e8d20f10715c61c53c1ec9b
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
src=$here/src
stage=$here/dist
jobs=${JOBS:-1}

for tool in git cmake ninja c++; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "pyrowave build needs $tool on PATH" >&2
        exit 1
    fi
done
# Granite's Release tree is large. Stop before a small disk fills up.
free_kb=$(df -Pk "$here" | awk 'NR==2 {print $4}')
if [ "$free_kb" -lt 8388608 ]; then
    echo "pyrowave build needs about 8 GiB free; $here has ${free_kb} KiB" >&2
    exit 1
fi

if [ ! -d "$src/.git" ]; then
    git clone https://github.com/Themaister/pyrowave "$src"
fi
git -C "$src" fetch origin
git -C "$src" checkout "$pin"

if [ ! -d "$src/Granite/.git" ]; then
    git clone https://github.com/Themaister/Granite "$src/Granite"
fi
git -C "$src/Granite" fetch origin
git -C "$src/Granite" checkout "$granite"
git -C "$src/Granite" submodule sync third_party/volk third_party/khronos/vulkan-headers
git -C "$src/Granite" submodule update --init third_party/volk third_party/khronos/vulkan-headers

cmake -S "$src" -B "$src/build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$src/build/output"
cmake --build "$src/build" --parallel "$jobs"
cmake --install "$src/build"

mkdir -p "$stage"
cp "$here/LICENSE" "$stage/LICENSE-pyrowave.txt"
found=0
for name in \
    libpyrowave-shared.so \
    libpyrowave-shared.so.1 \
    libpyrowave-shared.so.1.0.0 \
    libpyrowave-shared.dll \
    libpyrowave-shared-1.dll
do
    for dir in "$src/build/output/lib" "$src/build/output/bin" "$src/build"; do
        if [ -f "$dir/$name" ]; then
            cp "$dir/$name" "$stage/$name"
            found=1
        fi
    done
done
if [ "$found" -ne 1 ]; then
    echo "pyrowave shared library was not installed" >&2
    exit 1
fi
echo "staged in $stage"
