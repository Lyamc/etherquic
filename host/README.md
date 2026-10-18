# etherquic-host

Headless host for one player, or a client that connects to one. The service process does not capture the screen, encode video, or inject input. Session 0 has no desktop, so the service is not the GPUI console. `--desk` is the console-session helper that captures, encodes, and injects. That helper keeps its own path, separate from the play session inside `etherquic.exe`.

```powershell
etherquic-host.exe --client --peer 02:11:22:33:44:55
etherquic-host.exe --approve
etherquic-host.exe --deny
etherquic-host.exe --console
```

`--client` connects out and holds the session until Ctrl+C or the host closes it. It does not install a service. Its local MAC is a second locally administered address derived from the machine id, so it does not collide with the host service on the same PC. `--nic` and `--service` override the adapter and the service id (default 4433).

The Windows service `etherquic-service` (LocalSystem, delayed auto-start) listens on service **4433**. On macOS the LaunchDaemon `dev.etherquic.host` does the same job as root, so `/dev/bpf*` opens. On UDP it binds that port, or the next free one when another program holds 4433, up to 16 ports later. When only 4433 is taken, the announced port is 4434. 4434 is also the audio lab tool's service. The service manager or launchd starts the exe with no arguments.

| `host.conf` `unattended` | Incoming player |
|--------------------------|-----------------|
| `no` (default) | Handshake waits until `etherquic-host --approve`, `--deny`, or 60 seconds |
| `yes` | Accepted immediately, no confirmation |

`--approve` and `--deny` write `control\approve` or `control\deny` next to the config. The service consumes the file for that one player. The installer grants Users modify on `control\` only, so a signed-in user can approve without changing `host.conf`.

The host MAC comes from `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` on Windows, and from `IOPlatformUUID` (or `/etc/machine-id`) on macOS. The installer writes it once. A later install keeps that MAC. It is not the console's built-in `02:00:00:00:00:01`. The player types the address from `%ProgramData%\etherquic\address.txt` or `/Library/Application Support/etherquic/address.txt`.

| Path | Role |
|------|------|
| `%ProgramFiles%\etherquic\etherquic-host.exe` | Windows service binary |
| `/Library/Application Support/etherquic/MacOS/etherquic-host` | macOS LaunchDaemon binary |
| `%ProgramFiles%\etherquic\cert.pem` and `key.pem` | Windows certificates next to the exe |
| `/Library/Application Support/etherquic/MacOS/cert.pem` | macOS certificates |
| `%ProgramData%\etherquic\host.conf` | Windows `mac`, `nic`, `service`, `unattended` |
| `/Library/Application Support/etherquic/host.conf` | macOS host.conf |
| `address.txt` beside host.conf | MAC the player types |
| `control/` beside host.conf | `approve`, `deny`, and `pending.txt` |
| `host.log` beside host.conf | accept log |
| `/Library/LaunchDaemons/dev.etherquic.host.plist` | macOS LaunchDaemon |

On macOS, `etherquic-host --install-service` (root) copies this binary and its Frameworks into `/Library/Application Support/etherquic` and loads the plist. `packaging/macos/make-pkg.sh` offers that as a selected-by-default choice. Dragging the `.app` from the DMG leaves the service off; the app then prompts. `--uninstall-service` unloads the daemon and keeps `host.conf`. `--purge` removes the support directory.

`nic` empty means the default adapter. NDIS (`\\.\MembraneNdis`) is tried first; Npcap is the fallback. Leave `service` at 4433 so `etherquic-ui` can connect.

Certificates are resolved from `ETHERQUIC_CERTS` or the directory of the exe. The service working directory is `System32`, so the host does not use `CARGO_MANIFEST_DIR`. `ETHERQUIC_CONFIG` overrides the config file path; the log and address are written beside that file.

The demo pair in `certs/` is already public. The player skips verification. Replace the two files next to the exe if this machine should not use that pair.

`--console` runs the same host loop in the foreground, including the confirmation gate. Ctrl+C stops it.

A missing adapter or certificate is logged and the process exits non-zero. The service failure action restarts it after 5 seconds, up to three times per day. The process does not spin on its own. A requested stop reports success, so it does not restart.

Building links Npcap. `RUSTFLAGS` replaces Cargo's rustflags:

```powershell
$env:RUSTFLAGS = '-L native=C:\Build\networking\borderless\third_party\npcap\lib'
cargo build --release --manifest-path host/Cargo.toml --bin etherquic-host
```

Install with [`etherquic-setup`](../setup/README.md), or with the MSI that `packaging/windows/build-msi.ps1` builds (it runs the same setup and the Membrane script).

## Idle, screen saver, and lock

The desk helper (`--desk`, SYSTEM on the console session) moves its capture and injection threads onto whichever desktop has input: `Default`, `Screen-saver`, or `Winlogon`. A thread left on `Default` while a screen saver or the secure desktop is up gets `BitBlt` win32 6, a frozen duplication, and `SendInput` win32 5. That combination looked like a black picture with no input.

When a player attaches, the service sends the helper `Wake`. The helper resets the display idle timer, ends a screen saver with a small pointer move that it puts back, and holds `ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED` until `Release` when the player leaves. Power settings are not changed. The idle timers start again once nobody is connected.

Capture rebuilds duplication when the input desktop changes, after about three seconds of only the held picture, and every five seconds while it is in GDI mode. A headless adapter's output can be missing at start and show up later. WASAPI loopback delivers nothing while no app renders sound, so the helper sends silence in its place, and it reopens the default output after ten seconds without a packet.

`host.log` notes `capture desktop NAME`, `input desktop NAME`, `wake: ...`, and `release: ...`. A capture error is logged once, then once a minute, instead of once a second.
