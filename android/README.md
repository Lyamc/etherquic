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
