# etherquic-ui

Host and player console for an etherquic play session. This is the low-latency path, closer to Parsec than to a general remote-desktop client: one machine hosts, one player connects, and input is carried ahead of audio and video.

The window uses the Lyamc GPUI fork at `interfaces/gpui` (`default-features = false`, so Wayland, X11, and `windows-manifest` stay off). `gpui_http_client` is patched to the sibling crate at `interfaces/gpui_http_client`, which installs the Graviola TLS provider `Application::new` expects.

The taskbar button is the host. It is a normal window button (`WS_EX_APPWINDOW`), not a notification-area icon. Idle and player mode use `WS_EX_TOOLWINDOW`, so connecting does not take a taskbar slot. The button is turned on only after the host endpoint exists. A missing adapter, Npcap, or certificate leaves it off and the error stays in the window.

The host listens on service **4433** (the play session, the same id video uses). One listener, one pcap socket. Audio **4434** and input **4435** are the other lanes; they are not separate listeners in this window. On a shared connection Quinn sends a higher stream priority first:

| Lane | Priority | Service |
|------|----------|---------|
| input | 20 | 4435 |
| audio | 10 | 4434 |
| video | 0 | 4433 |

Hosting uses local MAC `02:00:00:00:00:01`. Playing uses `02:00:00:00:00:02`, so two copies on one machine do not collide. The peer field starts as the host address. The host takes one player at a time. This window does not capture the desktop, encode video, or inject keyboard, mouse, or gamepad input.

From the `etherquic` directory. The binary links Npcap the same way as the other L2 tools. `RUSTFLAGS` replaces Cargo's rustflags, so pass the library directory explicitly:

```powershell
$env:RUSTFLAGS = '-L native=C:\Build\networking\borderless\third_party\npcap\lib'
cargo run --manifest-path ui/Cargo.toml --bin etherquic-ui
```

`ETHERQUIC_CERTS` selects the server certificate directory (`cert.pem` and `key.pem`). The default is `etherquic/certs`.
