# etherquic-setup

Installer for the etherquic host service. Copy the payload folder to another Windows PC and run it elevated. It does not prompt, and it does not reboot the machine.

To connect out instead of installing, run `etherquic-host.exe --client --peer MAC`. That does not create a service.

The setup exe does not link Npcap or etherquic. The host exe does.

## Payload

`packaging/windows/build-msi.ps1` builds both binaries, stages this payload in `dist/msi-stage/` and wraps it in `dist/etherquic-<version>-windows-x64.msi` (see "MSI" below):

- `etherquic-setup.exe`
- `etherquic-host.exe`
- `cert.pem`
- `key.pem`

Those certificates are the public demo pair. The player skips verification.

## Install

```powershell
# Service. Each player waits for: etherquic-host.exe --approve
etherquic-setup.exe

# Service, unattended. A player connects with no confirmation.
etherquic-setup.exe --unattended
```

`--unattended`, `/S`, `/quiet`, and `--quiet` install unattended access. `--attended` switches an existing install back to confirmation. Leaving the flag off on a reinstall keeps the saved mode. The installer never asks a question. On success it prints a line a script can capture:

```text
etherquic address 02:..:..:..:..:.. service 4433
```

That address is also `%ProgramData%\etherquic\address.txt`. Type it into `etherquic-ui` or pass it to `etherquic-host --client --peer`. It is not the console default `02:00:00:00:00:01`.

A second line says either `confirmation required` or `unattended: a player connects with no confirmation`. Approval files live in `%ProgramData%\etherquic\control`. Users can write those files. They cannot rewrite `host.conf`.

The service name is `etherquic-service`, display name `etherquic service`. It runs as LocalSystem so it can open Npcap when Npcap is restricted to administrators. An install also removes the previous service name `etherquic` so the two do not share the adapter. Start type is automatic, delayed. The binary path is passed to `CreateServiceW` already quoted. `sc.exe` is not used, because it splits a path at the first space.

Reinstalling keeps an existing valid MAC and only changes `nic` when `--nic` is passed. `--no-start` installs the service and does not start it.

Npcap is not downloaded and not bundled. If the machine needs it and you already have the installer:

```powershell
etherquic-setup.exe --unattended --npcap D:\npcap-setup.exe
```

That runs `npcap-setup.exe /S`. Exit 0 continues. Exit 3010 means Npcap asked for a reboot: this setup prints that and exits 3010 without rebooting and without starting the service. Any other Npcap exit code fails the install. When the `npcap` service exists afterwards, the etherquic service depends on it. An NDIS-only machine has no such dependency.

| Exit | Meaning |
|------|---------|
| 0 | installed |
| 3010 | Npcap asked for a reboot; etherquic was not started and the PC was not rebooted |
| 2 | bad arguments |
| 1 | install failed |

## Remove

```powershell
etherquic-setup.exe --uninstall
etherquic-setup.exe --uninstall --purge
```

`--uninstall` stops and deletes the service and removes `%ProgramFiles%\etherquic`. Configuration, the log, and the address stay. `--purge` also removes `%ProgramData%\etherquic`.

## Flags

```text
--install-dir DIR    default %ProgramFiles%\etherquic
--payload DIR        default the directory of etherquic-setup.exe
--attended           this service requires etherquic-host --approve
--unattended         this service accepts a player with no confirmation (/S, /quiet)
--nic NAME           adapter name stored in host.conf
--no-start           do not start the service
--npcap SETUP.exe    run that Npcap installer with /S
--uninstall
--purge              with --uninstall, also remove ProgramData\etherquic
```

## MSI

`packaging/windows/build-msi.ps1 [-Version 0.1.0] [-Codecs DIR] [-VulkanCpu DIR] [-Cjxl DIR]` needs WiX 5 (`dotnet tool install --global wix --version 5.0.2`) and Npcap's `wpcap.lib`. The script builds opus, PyroWave, `pyrowave_d3d11.dll`, and `cjxl` from the git submodules, stages lavapipe, and always puts that set in the MSI. The directory switches only add files. The per-machine MSI puts the payload, the Membrane driver package (`membrane\`), and the `etherquic` app (Start Menu shortcut) in `%ProgramFiles%\etherquic-setup`. A deferred custom action then runs `msi-setup.ps1` as LocalSystem:

1. Membrane (`MEMBRANE=1`, the default): skipped when `membrane_ndis` is already running; `MEMBRANE=force` reinstalls; `MEMBRANE=0` skips. msiexec removes SeSystemEnvironmentPrivilege from a custom action's token, so bcdedit fails there. The script therefore runs as a one-shot SYSTEM scheduled task that is removed afterwards. Exit 3010 (test signing just turned on) is logged; the MSI never reboots. A failure (for example Secure Boot on) is logged and does not stop the install, but the host needs Membrane or Npcap for L2.
2. `etherquic-setup.exe --payload <that dir>`, plus `--unattended` or `--attended` when `ACCESS=unattended|attended`; empty keeps the saved mode.

Uninstall runs `etherquic-setup --uninstall` (host.conf is kept). Logs: `%ProgramData%\etherquic\msi-setup.log` and `membrane-install.log`.

```powershell
msiexec /i etherquic-0.1.0-windows-x64.msi /qn ACCESS=unattended
```
