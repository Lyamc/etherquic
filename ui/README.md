# etherquic

One host, one player. `host` shares this machine. `connect` joins one. The taskbar button is on only while hosting. Alt-Tab still lists the window. On Windows the notification area shows `assets/etherquic.svg` on the taskbar for as long as the app is running. Closing the window hides it; Quit on that icon exits. A Start Menu launch has no console. `--console` opens one. Starting the program from a terminal prints to that terminal. On macOS the Host card prompts to install the host LaunchDaemon when it is missing, and Settings can install or remove it.

```text
etherquic
etherquic host [--nic IFACE]
etherquic connect --peer-mac MAC [--nic IFACE]
```

Service **4433**, host MAC `02:00:00:00:00:01`, player MAC `02:00:00:00:00:02`. Host shares this desktop, its audio, and takes keyboard, mouse, and gamepad from one player. Connect opens that desktop in its own window. Focus the picture window to send the keyboard and the mouse. A local gamepad sends while the picture is open. That session is the [`play`](../play/README.md) library, called inside `etherquic.exe`. `audio/`, `video/`, and `input/` are lab tools, and they are not this session. The video lab tool also uses service 4433.

On open, and on Refresh, the window announces on the LAN (service 4440). A host answers with the MAC and service to dial, the adapter's IP addresses, and that service port. The window times the replies and shows latency, jitter, min, average, max, standard deviation, and a small graph. Click a host to fill the connect field.

The window lists this PC's hostname, the L2 address and service to dial, and the selected adapter's IPv4 and IPv6 addresses. The host card shows the same facts as a QR code. A phone that scans it reads the name, the MAC, the service, the port, and the addresses.

GPUI comes from `interfaces/gpui` with default features off. The workspace root patches `gpui_http_client` to `interfaces/gpui_http_client` (Graviola) and `xattr` to `vendor/xattr`. On Linux the `ui` build enables the Wayland feature.

```powershell
$env:RUSTFLAGS = '-L native=PATH\TO\npcap\lib'
cargo run --manifest-path ui/Cargo.toml --bin etherquic -- connect --peer-mac 02:00:00:00:00:01 --nic IFACE
```

`ETHERQUIC_CERTS` is the directory with `cert.pem` and `key.pem`. The default is `etherquic/certs`. A missing adapter, Npcap library, or certificate leaves the taskbar button off and shows the error in the window.
