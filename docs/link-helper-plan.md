# Option C: privileged link helper

The GUI, GPU, capture, and codecs stay the logged-in user. One long-lived privileged process owns the Ethernet adapter and moves `0x88B6` frames. User processes talk to it over a local socket.

This is the implementation plan. Root `plan.md` stays the dirty-rectangle note.

## Goal

- Player and in-process host open L2 without root in this process.
- A crash of the helper restarts it on Linux, macOS, and Windows until someone uninstalls it or the machine is off.
- An upgrade (including one started from a remote desktop session on that machine) stops the helper, replaces the files, re-registers the unit, and starts it again as one elevated transaction.
- Uninstall stops the helper, unregisters it so it cannot come back, and deletes the copied binaries. `host.conf` stays unless `--purge`.

UDP join stays the fallback. Android stays UDP. A missing helper must not block UDP.

## Non-goals

- Elevating the GUI.
- Linux `setcap` on `etherquic` / `etherquic-ui` (Option A). Caps belong on the helper unit.
- macOS ChmodBPF.
- A Play Store Android L2 path.
- Changing EtherType, the Ethernet frame layout, or Quinn.
- Merging `etherquic-host --desk` into the play session (`docs/workspace-plan.md` item 4).

## Key decisions

1. **One privileged binary, one service identity.** The helper is `etherquic-host` in link-broker mode, not a second daemon. Windows already has `etherquic-service`. macOS already has `dev.etherquic.host`. Linux gets the same verbs (`--install-service` / `--uninstall-service`). One object to crash-restart, upgrade, and remove.

2. **The helper is the only process that opens the NIC.** `L2Net::open` in the GUI/player connects to the helper when the socket exists. In-process pcap remains for tests (`L2Net::memory`) and for a Linux process that already has `CAP_NET_RAW` (dev builds). Production desktop L2 goes through the helper.

3. **The helper injects and captures only EtherType `0x88B6`.** Same filter the BPF program already uses. Arbitrary raw frames are rejected. That is the privilege boundary.

4. **Quinn stays in the caller.** The helper does not terminate QUIC. A helper restart is datagram loss, not a new TLS session, if the caller reconnects the socket before idle timeout. A host-service upgrade still drops the hosted session because the host process is the helper.

5. **Install is upgrade.** `--install-service` and the MSI/pkg/setup path are idempotent: stop, replace, rewrite the unit with the current restart policy, start. They do not create a second service.

6. **Stop is the only way the helper stays down.** Crash, nonzero exit, and kill restart it. `SERVICE_CONTROL_STOP`, `launchctl bootout`, `systemctl stop` plus disable/uninstall stay down.

7. **Android never requires the helper.** `LinkIo` has a UDP backend. Rooted Magisk later can speak the same socket protocol; it is not in this work.

## Process split

```text
etherquic GUI / play / lab tools     user
        |  local socket / named pipe
etherquic-host (service)             root / LocalSystem / CAP_NET_RAW
        |  BPF + AF_NDRV / AF_PACKET / Membrane / Npcap
Ethernet 0x88B6
```

The service already accepts a player on 4433 and (on Windows) talks to `--desk` over `\\.\pipe\etherquic-desk`. This plan adds a second local endpoint for **frames**, used by any user process that wants L2.

When the service is installed, the GUI Host button does not open a second adapter. It uses the helper for L2 I/O, or it leaves hosting to the service that is already listening. Two BPF opens on the same NIC fight over frames.

`--desk` is unchanged. Capture and input stay in the console session. Npcap/Membrane stay in the service.

## Link protocol

### Endpoint

| OS | Path | ACL |
| --- | --- | --- |
| Linux | `/run/etherquic/link.sock` | `root:etherquic` `0660`, directory `0750`. Install creates group `etherquic` and adds the installing user. |
| macOS | `/var/run/etherquic/link.sock` | `root:wheel` `0660`. Interactive users on that Mac may connect. |
| Windows | `\\.\pipe\etherquic-link` | Local, `PIPE_REJECT_REMOTE_CLIENTS`, same pattern as the desk pipe. Interactive users may connect. |
| Android | none | UDP only. |

The helper unlinks a stale socket on start. The directory is created at install and removed on uninstall.

### Framing

Length-prefixed little-endian `u32` length, then a body. Max body 2048 bytes (one Ethernet frame plus a small header). Version in the hello. Unknown message types are an error, not ignored.

| Type | Who | Body |
| --- | --- | --- |
| `Hello` | client | protocol version (`1`), pid |
| `HelloOk` | helper | version, helper semver string, backend id (libpcap / Npcap / membrane / bpf) |
| `ListNics` | client | empty |
| `Nics` | helper | count, then `{name, description}` |
| `Open` | client | optional NIC name, EtherType (must be `0x88B6`) |
| `Opened` | helper | hardware MAC, adapter name, MTU, backend id |
| `Frame` | both | raw Ethernet bytes, EtherType already `0x88B6` |
| `Error` | helper | utf-8 reason |
| `Bye` | either | empty |

One client, one `Open`. A second `Open` is an error until `Bye`. Several clients may be connected at once. The helper is the single reader on the NIC and fans frames out.

### Multiplex

Each `Open` registers that client's source MAC after the first valid send (or the helper assigns the adapter MAC for a host). Recv: deliver a frame to a client when destination is broadcast/multicast or that client's MAC. Send: reject a frame whose EtherType is not `0x88B6`, or whose length is below the Ethernet + service header.

Discovery (service 4440) is just frames. The player still builds announces; they ride `Frame`.

### Caller API

New module `src/link_io.rs` (names can move, the surface is this):

- `LinkIo` trait: `send(&[u8])`, `recv() -> Option<Vec<u8>>`, `hw_mac()`, `adapter()`, `backend()`.
- Backends: `Memory` (today's `L2Net::memory`), `Direct` (today's `open_packet_io`, tests and cap-bearing dev), `Helper` (socket).
- `L2Net::open` tries `Helper` when the socket/pipe exists, else `Direct`, else the current permission error. Android stays `AndroidRoot` for L2 and uses UDP.

`l2_unavailable_reason()` becomes: helper missing **and** this process cannot open the NIC. When the helper is up, it returns `None` even if this process is unprivileged.

Reconnect: if the helper socket dies, `Helper` retries for a few seconds (the unit is coming back). Then it surfaces the error. Quinn's idle timer is the session-level limit.

## Service identity and restart

The helper does not loop on a missing NIC. It logs, exits nonzero, and the **supervisor** restarts it. That is already the Windows/macOS host rule (`host/README.md`). This plan makes the three supervisors match, and removes the Windows daily cap.

| | Windows | macOS | Linux (tarball / `--install-service`) | NixOS module |
| --- | --- | --- | --- | --- |
| Name | `etherquic-service` | `dev.etherquic.host` | `etherquic-host.service` | keep `services.etherquic` for the desktop host; add `services.etherquic-link` **or** point the existing unit at `etherquic-host` in service mode. Do not leave two units opening the same NIC. |
| Binary | `%ProgramFiles%\etherquic\etherquic-host.exe` | `/Library/Application Support/etherquic/MacOS/etherquic-host` | `/usr/local/libexec/etherquic/etherquic-host` (copied, root-owned) | store path |
| User | LocalSystem | root | root, or a dedicated user with `AmbientCapabilities=CAP_NET_RAW CAP_NET_ADMIN` | dedicated user + caps, same as today's `AmbientCapabilities` |
| Start | Automatic, delayed | `RunAtLoad` | `WantedBy=multi-user.target`, `After=network-online.target` | already `graphical.target` for the desktop host; the link unit is `multi-user.target` |
| Crash restart | `SC_ACTION_RESTART` every 5s. Reset period 10s so the action list never exhausts. `SERVICE_FAILURE_ACTIONS_FLAG` still treats nonzero `SERVICE_STOPPED` as failure. | `KeepAlive` true, `ThrottleInterval` 5 (already) | `Restart=on-failure`, `RestartSec=5`, `StartLimitIntervalSec=0` (no burst disable) | same restart keys on the link unit |
| Requested stop | exit 0, no restart (already) | `bootout` | `systemctl stop` then disable on uninstall | `systemctl stop` |
| Logs | `%ProgramData%\etherquic\host.log` | `launchd.out.log` / `err.log` | journald + `/var/log/etherquic/host.log` | journald |

Windows today: 3 restarts per 86400s (`setup/src/plan.rs` `FAILURE_RESTARTS`). After three crashes in a day the service stays dead. Change `FAILURE_RESET_SEC` to 10 and keep three `SC_ACTION_RESTART` entries (or one entry if the SCM allows). Result: unbounded restart with a 5s throttle, same as launchd.

Do not add a user-space watchdog. The OS supervisor is the watchdog.

## Install, upgrade, remote update

### Verbs

Same flags on every OS, implemented inside `etherquic-host` where they already exist on macOS, and inside `etherquic-setup` on Windows (GUI and the updater call setup, not a second copy of the SCM code).

```text
etherquic-host --install-service
etherquic-host --uninstall-service [--purge]
```

`--install-service` means **make this version the running service**:

1. Require the privilege (root / LocalSystem / polkit).
2. Stage the new tree next to the live tree (`…/etherquic.staging` or a temp dir on the same volume).
3. Stop the live service. Wait until the process is gone (Windows cannot overwrite a running image; macOS/Linux should not either).
4. Replace the live tree (copy/rename). On Windows, replace `%ProgramFiles%\etherquic\etherquic-host.exe` and the payload files `setup` already copies. On macOS, keep `collect_copies`. On Linux, copy the host binary, certs, and any bundled libs into `libexec`.
5. Write/refresh the unit: plist, systemd unit, or `CreateServiceW` + failure actions + delayed auto. Enable it.
6. Start. Print the address line the current installer prints.
7. Delete the staging dir.

If step 6 fails, leave the new files in place (they are the version we meant to run) and return an error. Do not silently boot the old binary from a second path.

`--install-service` on a machine that never had the service creates it. On a machine that has it, it is an upgrade. `host.conf`, the MAC, and unattended mode are kept (same as today's reinstall).

### Privilege prompt (first install)

Rustdesk-style button in the UI: **Enable Ethernet**. It runs the existing elevation path:

- macOS: `osascript` `do shell script … --install-service with administrator privileges` (`ui/src/macos_service.rs`).
- Windows: UAC on `etherquic-setup.exe` (already elevated for MSI custom action / setup).
- Linux: `pkexec` / polkit on `etherquic-host --install-service`.

After success, retry `L2Net::open`. Settings can still Remove service.

### Upgrade while you are remoted in

The hosted session **will drop** when the helper/host process stops. The upgrade must not wait for a click on that desktop after the stop.

In-app update (`ui/src/update.rs`) today:

- Windows: `msiexec /i` with a UI, no `/qn`.
- macOS: `open` the pkg, user finishes it.
- Linux: `tar` into a writable dir, **does not** touch a systemd unit.

Change the update path when a service is installed:

| OS | What the updater runs |
| --- | --- |
| Windows | `msiexec /i <msi> /qn INSTALLSERVICE=1` with `ACCESS` omitted so the saved mode stays. The deferred custom action already stops/replaces/starts via `etherquic-setup`. |
| macOS | `installer -pkg <pkg> -target /` as root (osascript/polkit), host component selected. `postinstall` already calls `--install-service` (bootout, copy, bootstrap, `kickstart -k`). |
| Linux | unpack into staging, `pkexec etherquic-host --install-service` from that tree so the new binary copies itself into `libexec` and `systemctl restart`s. |

The GUI fires that command and does not require a later wizard page. The player on the other side reconnects when 4433 is listening again. Add a player-side reconnect loop (bounded, with the existing idle timeout) so a 5–15s helper restart is a blip instead of a dead tab.

If the service is **not** installed, keep today's app-only update (MSI `INSTALLSERVICE=0` is wrong here: leave the service flag unchanged. MSI already: empty ACCESS keeps saved mode; `INSTALLSERVICE=1` is the default including quiet).

Npcap exit 3010 still means: do not start the service, do not reboot (existing setup rule).

### File locks

Stop `--desk` as part of Windows service stop (already tied to the service). Stop the GUI only if it has the install dir locked; it should not, once L2 is a socket. Do not kill the GUI as a matter of course; the user may be the remote viewer on a different machine.

## Uninstall

`--uninstall-service` (and MSI/pkg remove, and Settings → Remove):

1. Stop the service.
2. Disable it so a reboot does not start it (`bootout` + delete plist; `DeleteService`; `systemctl disable --now` + remove unit file + `daemon-reload`).
3. Remove the copied binaries (Windows `%ProgramFiles%\etherquic` host tree; macOS `MacOS/` and `Frameworks/`; Linux `libexec/etherquic`).
4. Remove the link socket and its directory.
5. Remove the Linux `etherquic` group only if we created it and it has no leftover files.
6. Keep `host.conf`, `address.txt`, and logs. `--purge` removes the config directory (`%ProgramData%\etherquic`, `/Library/Application Support/etherquic`, `/var/lib/etherquic`).
7. Return success if the service was already gone (idempotent).

After uninstall, `l2_unavailable_reason()` is the OS permission string again. The Enable Ethernet button comes back. UDP still works.

MSI `INSTALLSERVICE=0` on a reinstall already runs `etherquic-setup --uninstall` and keeps `host.conf`. Keep that. A full product uninstall runs `--uninstall` without `--purge` unless the user ticks remove settings (if we add that checkbox later; today's MSI keeps host.conf).

Do not leave `KeepAlive` plist, a disabled Windows service with an old `binPath`, or a systemd unit in `/etc/systemd/system` pointing at a missing binary.

## UI

- When L2 open fails with permission / helper missing: button **Enable Ethernet** next to the existing L2 reason (`ui/src/view.rs` Host card and Settings network part). Same control on Linux and Windows, not only the macOS “install host service” prompt.
- Copy: the GUI does not become administrator. The helper does.
- Settings still shows Host service installed / not, plus Install / Remove.
- `skip_host_service_prompt` applies to the Enable Ethernet banner the same way.
- Codec table, path budget, and other Settings rows are unchanged.

## Android

No helper, no `--install-service`, no BPF. The APK keeps `INTERNET` and `ACCESS_NETWORK_STATE`. `LinkIo` is unused on the join path. A later rooted helper can implement the Unix socket; the join path must still succeed when that socket is absent.

## Packaging changes

- **macOS pkg:** already installs the LaunchDaemon. Confirm `postinstall` is `--install-service` (upgrade). Uninstall script must `bootout` and delete the plist; today `--uninstall-service` does. Product uninstall of the `.app` without the pkg must not leave the daemon; document that Remove in Settings (or the pkg uninstall) is required if the user dragged the app to Trash.
- **Windows MSI:** quiet update as above. Failure actions rewritten on every install so old 3/day machines pick up unbounded restart.
- **Linux tarball:** add `etherquic-host` to the archive, a unit file, and a polkit policy. `make-tarball.sh` currently ships GUI + play only.
- **NixOS:** `services.etherquic` today runs the **desktop GUI host** as the user with `CAP_NET_RAW`. Split: a `etherquic-host` system unit (this helper/host) plus the user GUI without caps. Migrating a running NixOS box is a module change; do not silently drop capture. Spell the module split in the PR that adds Linux `--install-service`.

## Tests

- Protocol encode/decode and version mismatch (`host` or `src`, no NIC).
- Helper rejects a non-`0x88B6` frame.
- `L2Net::memory` still loops two peers (existing tests).
- `l2_unavailable_reason`: helper socket present ⇒ `None`; absent + no caps ⇒ current strings; Android ⇒ `"Android needs root"`.
- macOS plist still has `KeepAlive` / `ThrottleInterval` 5.
- Windows `setup` plan: failure reset 10s, restart actions present, uninstall deletes the service name and the previous name `etherquic`.
- Linux unit text: `Restart=on-failure`, `StartLimitIntervalSec=0`.
- Install is idempotent: second `--install-service` is upgrade (unit tests on the plan/steps, like `setup/src/plan.rs`).
- Uninstall is idempotent: second call is success.

Do not require a live adapter in CI.

## Implementation order

Do these as separate, mergeable slices. Each one should compile and leave UDP working.

1. **`LinkIo` + protocol + memory/direct.** No service change. Tests for the codec. `L2Net::open` still Direct.
2. **Helper listen in `etherquic-host`.** Unix socket on macOS/Linux, named pipe on Windows. Fan-in/fan-out on the existing NIC open. `--console` also binds the socket so a dev can try without install.
3. **`L2Net::open` prefers Helper.** GUI/player use it when the socket exists. Permission error if neither helper nor Direct works.
4. **Enable Ethernet button** on all three desktops, wired to `--install-service` / setup.
5. **Restart policy.** Windows unbounded 5s restart; Linux unit with `StartLimitIntervalSec=0`; macOS already KeepAlive. Tests on the rendered unit/plan.
6. **Linux `--install-service` / `--uninstall-service`.** tarball, polkit, group, `libexec`. NixOS module split in the same PR or the next one if the module change is large.
7. **In-app update** runs the silent install/upgrade when the service is present. Player reconnect after L2/helper loss.
8. **Uninstall completeness** pass: plist, SCM, systemd, socket dir, binaries, `--purge`. Docs: `host/README.md`, `setup/README.md`, `Architecture.md` (one sentence that GUI L2 uses the helper), `ui/README.md`.

## Files (expected)

| Area | Paths |
| --- | --- |
| Protocol / L2 | `src/link_io.rs`, `src/l2_endpoint.rs`, `src/pcap_link.rs` (`l2_unavailable_reason`), `src/lib.rs` |
| Helper | `host/src/main.rs`, `host/src/cli.rs`, `host/src/run.rs`, new `host/src/link_broker.rs`; Windows pipe next to `host/src/link.rs` (desk pipe stays) |
| macOS install | `host/src/macos_service.rs` (plist unchanged except we already have KeepAlive), `ui/src/macos_service.rs`, `packaging/macos/service-scripts/postinstall` |
| Windows install | `setup/src/plan.rs` (`FAILURE_RESET_SEC`), `setup/src/win.rs`, `packaging/windows/build-msi.ps1` / silent update args |
| Linux install | new `host/src/linux_service.rs`, `packaging/linux/etherquic-host.service`, polkit file, `packaging/linux/make-tarball.sh` |
| NixOS | `nix/module.nix` |
| UI | `ui/src/view.rs`, `ui/src/settings.rs`, `ui/src/update.rs` |
| Docs | this file, `Architecture.md`, `host/README.md`, `setup/README.md`, `README.md`, `ui/README.md` |

## Open questions

None that block the first PR. Two product calls already made in this document: one service identity (not a separate `linkd`), and unbounded supervisor restart (not 3/day). Revisit a dedicated `etherquic-linkd` only if the host process crash rate makes shared fate a problem; the protocol does not care which binary binds the socket.
