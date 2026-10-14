# etherquic-play

`etherquic-play host` shares the desktop and its audio, and applies the player's keyboard, mouse, and gamepad. `etherquic-play connect` opens the view. Focus that window to send the keyboard and the mouse. A local gamepad sends while the view is open. The `etherquic` window runs this same session: Host shares the desktop, and Connect opens the view.

Service **4433**. Host MAC `02:00:00:00:00:01`, player MAC `02:00:00:00:00:02`. Do not run another listener on 4433 (`etherquic-video-recv` included).

| Stream | Magic | Priority | Direction |
| --- | --- | --- | --- |
| input | `EINP` | 20 | player to host |
| audio | `EAUD` | 10 | host to player |
| video | `EVID` | 0 | host to player |

Each record is a little-endian `u32` length, then the payload. Video is JPEG, scaled to `--max-width` (default 1280, both sides even), quality 40, `--fps` (default 10). Audio is interleaved `i16` PCM with a rate and a channel count. Input is an EQIN document, the same bytes as [`etherquic-input`](../input/README.md).

```bash
cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- host --nic IFACE
cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- connect --nic IFACE --peer-mac 02:00:00:00:00:01
```

`nix-shell nix/shell.nix` provides libpcap and the desktop libraries. `ETHERQUIC_CERTS` defaults to `etherquic/certs`. The player does not check the certificate.

Linux capture lists monitors through X11, then takes the picture from the Wayland portal, GNOME Shell, or wlroots. A portal can ask. Keyboard injection is a Wayland virtual keyboard. The pointer tries KWin fake-input (version 3 or newer), then a virtual pointer the size of the screen, then libei from the remote-desktop portal. An X11-only session gets none of those events. Gamepads appear as `etherquic-play pad N` via `/dev/uinput`, or as an Xbox 360 controller when ViGEmBus is installed. Windows audio is the default output opened as loopback. Linux audio prefers an input whose name contains `monitor` or `loopback`.
