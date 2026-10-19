#!/bin/sh
# Cross-compile the Android codec jniLibs the Play APK packs beside
# libetherquic_android.so. Hosts can send PyroWave, Opus, H.264, H.265, AV1,
# OpenAPV, and JPEG XS; MJPEG and MJ2K already live in the Rust cdylib.
#
#   sh packaging/android/codecs.sh [OUTDIR]
#
# Default OUTDIR is packaging/android/jni/arm64-v8a, which
# etherquic-installers reads when building the APK. Override with
# ETHERQUIC_ANDROID_LIBS.
set -eu
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
out=${1:-"$here/jni/arm64-v8a"}
store=$(nix-build --no-out-link "$root/nix/android-codecs.nix")
mkdir -p "$out"
cp -a "$store/lib/arm64-v8a/." "$out/"
chmod -R u+w "$out"
ls -l "$out"
