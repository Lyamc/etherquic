# etherquic-play

`etherquic-play host` shares the desktop and its audio, and applies the player's keyboard, mouse, and gamepad. `etherquic-play connect` opens the view. Focus that window to send the keyboard and the mouse. A local gamepad sends while the view is open. The `etherquic` window calls this library inside `etherquic.exe`: Host shares the desktop, and Connect opens the view.

Service **4433** on Ethernet. Host MAC `02:00:00:00:00:01`, player MAC `02:00:00:00:00:02`. Do not run another listener on 4433 (`etherquic-video-recv` included). On UDP, play binds 4433, or the next free port when something else holds it, up to 16 ports later. When only 4433 is taken, that port is 4434, and discovery announces it. 4434 is also the audio lab tool's service.

| Stream | Magic | Priority | Direction |
| --- | --- | --- | --- |
| input | `EINP` | 20 | player to host |
| audio | `EAUD` | 10 | host to player |
| video | `EVID` | 0 | host to player |

Each record is a little-endian `u32` length, then the payload. The live path negotiates PyroWave, MJPEG, or H.264. H.265 and AV1 hardware are in the same preference list, ahead of those three, and CPU PyroWave is last. The picture is scaled to `--max-width` (default 1280, both sides even) at `--fps` (default 10). The original `EVID` payload is still a JPEG or an `EINT` intra frame. When both ends agree on `etherquic/2`, the picture stream is `EVI2` and audio is `EAU2`. Original `EAUD` is interleaved `i16` PCM with a rate and a channel count. Input is an EQIN document, the same bytes as [`etherquic-input`](../input/README.md).

```bash
cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- host --nic IFACE
cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- connect --nic IFACE --peer-mac 02:00:00:00:00:01
```

`nix-shell nix/shell.nix` provides libpcap and the desktop libraries. `ETHERQUIC_CERTS` defaults to `etherquic/certs`. The player does not check the certificate.

Linux capture lists monitors through X11, then takes the picture from the Wayland portal, GNOME Shell, or wlroots. A portal can ask. Keyboard injection is a Wayland virtual keyboard. The pointer tries KWin fake-input (version 3 or newer), then a virtual pointer the size of the screen, then libei from the remote-desktop portal. An X11-only session gets none of those events. Gamepads appear as `etherquic-play pad N` via `/dev/uinput`, or as an Xbox 360 controller when ViGEmBus is installed. Windows audio is the default output opened as loopback. Linux audio prefers an input whose name contains `monitor` or `loopback`.
