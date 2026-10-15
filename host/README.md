# etherquic-host

Headless host for one player, or a client that connects to one. It does not capture the screen, encode video, or inject input. Session 0 has no desktop, so the service is not the GPUI console.

```powershell
etherquic-host.exe --client --peer 02:11:22:33:44:55
etherquic-host.exe --approve
etherquic-host.exe --deny
etherquic-host.exe --console
```

`--client` connects out and holds the session until Ctrl+C or the host closes it. It does not install a service. Its local MAC is a second locally administered address derived from the machine id, so it does not collide with the host service on the same PC. `--nic` and `--service` override the adapter and the service id (default 4433).

The Windows service `etherquic-service` (LocalSystem, delayed auto-start) listens on service **4433**. The service manager starts the exe with no arguments.

| `host.conf` `unattended` | Incoming player |
|--------------------------|-----------------|
| `no` (default) | Handshake waits until `etherquic-host --approve`, `--deny`, or 60 seconds |
| `yes` | Accepted immediately, no confirmation |

`--approve` and `--deny` write `control\approve` or `control\deny` next to the config. The service consumes the file for that one player. The installer grants Users modify on `control\` only, so a signed-in user can approve without changing `host.conf`.

The host MAC comes from `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid`. The installer writes it once. A later install keeps that MAC. It is not the console's built-in `02:00:00:00:00:01`. The player types the address from `%ProgramData%\etherquic\address.txt`.

| Path | Role |
|------|------|
| `%ProgramFiles%\etherquic\etherquic-host.exe` | service binary |
| `%ProgramFiles%\etherquic\cert.pem` and `key.pem` | certificate next to the exe |
| `%ProgramData%\etherquic\host.conf` | `mac`, `nic`, `service`, `unattended` |
| `%ProgramData%\etherquic\address.txt` | MAC the player types |
| `%ProgramData%\etherquic\control\` | `approve`, `deny`, and `pending.txt` |
| `%ProgramData%\etherquic\host.log` | accept log |

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

Install with [`etherquic-setup`](../setup/README.md). `build-installer.ps1` builds both and copies the payload.
