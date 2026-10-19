# etherquic-android

UDP player loaded by the EtherQuic Play APK (`dev.etherquic.play`).

The APK is a NativeActivity. `android_main` binds GPUI and opens the same
Desk the desktop window uses. A phone has no EtherType socket: Connect
dials the UDP port on the host card.

```bash
# Host (this crate, Linux): cargo test -p etherquic-android -- --nocapture
# Phone .so, once the NDK and aarch64-linux-android target are installed:
cargo build -p etherquic-android --release --target aarch64-linux-android
```

Pack `libetherquic_android.so` as `lib/arm64-v8a/libetherquic_android.so` in
the APK (`ETHERQUIC_ANDROID_SO` or `target/aarch64-linux-android/release/`).

Hosts can send PyroWave, MJPEG, MJ2K, H.264, H.265, AV1, JPEG XS, and OpenAPV,
plus Opus audio. MJPEG and MJ2K decode in this cdylib. The APK also needs
unversioned jniLibs the player dlopens or execs from `nativeLibraryDir`
(`current_exe()` on a NativeActivity is `/system/bin/app_process64`):

| File | Codec |
| --- | --- |
| `libopus.so` | Opus |
| `libpyrowave-shared.so` | PyroWave (Vulkan on the phone) |
| `libffmpeg.so` | H.264, HEVC, AV1 (dav1d), OpenAPV, JPEG XS |

Build those for `arm64-v8a` and leave them where the installer looks:

```bash
sh packaging/android/codecs.sh packaging/android/jni/arm64-v8a
# or: ETHERQUIC_ANDROID_LIBS=/path/to/arm64-v8a
cargo run --manifest-path packaging/Cargo.toml -- android play.apk 1 0.1.9
```

The package manager only extracts names matching `lib*.so` with no SONAME
suffix, so the codec script copies `libopus.so.0` as `libopus.so` and the
ffmpeg binary as `libffmpeg.so`. The manifest keeps `extractNativeLibs=true`
so that ffmpeg can be executed from the extracted directory.
