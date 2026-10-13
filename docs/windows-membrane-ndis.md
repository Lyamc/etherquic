# Installing Membrane NDIS on Windows

Membrane NDIS is the Windows L2 adapter etherquic opens first. The userspace
path is `\\.\MembraneNdis` (`src/ndis_link.rs`). When that device is absent,
etherquic uses Npcap. Npcap stays installed either way.

The driver is an NDIS lightweight filter:

| Piece | Value |
| --- | --- |
| Component | `membrane_ndis_filter` |
| Service | `membrane_ndis` |
| Image in this repo | `windows/membrane_ndis.sys` |
| Installed image | `%SystemRoot%\System32\drivers\membrane_ndis.sys` |
| Device | `\\.\MembraneNdis` |
| INF | `windows/ndis_driver.inf` |
| Catalog | `windows/membrane_ndis.cat` |
| Certificate | `windows/MembraneTest.cer` (public test cert) |
| Start | demand (3). The service does not load at boot. |

Run the installer from an elevated local console. The filter attaches to the
machine's adapters. Do this at the PC, on a session that can survive a short
network blip.

## One shot

From an elevated PowerShell, in a checkout of this repo:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1
```

That script is safe to run again.

- The filter is already bound: it sets the service to demand start, starts it,
  and checks that `\\.\MembraneNdis` opens.
- The filter is not bound: it turns on test signing if needed, trusts the
  shipped certificate, binds `membrane_ndis_filter`, starts the service, and
  opens the device. The Windows Driver Kit is not used.
- Test signing was off: it enables test signing and
  `VulnerableDriverBlocklistEnable=0`, then exits `3010`. Reboot, and run the
  same command again. The driver cannot load on the boot where those flags
  were first set.

See what it would do, without changing the machine:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1 -Preflight
```

Other switches:

```powershell
# Bind, and leave the service stopped
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1 -NoStart

# Replace the bound filter with a chosen image
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1 -Reinstall -SysPath C:\td\kdrv2\membrane_ndis.sys

# Stop, disable, and unbind
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1 -Uninstall

# Same, and rename the .sys so it cannot be started
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1 -Uninstall -Quarantine
```

## What the script needs

- 64-bit Windows 10 or 11, elevated.
- Secure Boot off. Test-signed drivers do not load while it is on.
- Memory integrity (HVCI) off. The script warns when it is on.
- The signed package shipped next to the script. `pnputil /add-driver` does
  not bind an NDIS filter. `netcfg` does.
- `NetCfgInstanceId` is `{a1b2c3d4-e5f6-7890-abcd-ef1234567890}`. `StartType`
  in the INF is `3`.

The package in this repo is:

| File | Role |
| --- | --- |
| [`windows/membrane_ndis.sys`](../windows/membrane_ndis.sys) | Signed driver, 20,848 bytes, SHA-256 `d3884252d69a4994c978a0ba747035a3de45abee6669c4269c193610cf8343b8`. Native PE subsystem 1. |
| [`windows/ndis_driver.inf`](../windows/ndis_driver.inf) | INF covered by the catalog. |
| [`windows/membrane_ndis.cat`](../windows/membrane_ndis.cat) | Catalog produced by Inf2Cat for that sys and INF, then signed. |
| [`windows/MembraneTest.cer`](../windows/MembraneTest.cer) | Public test certificate `CN=MembraneTest`, thumbprint `F0279DCD81DA66DD01CC28404B1F2339BE1CA353`, expires 2027-09-25. No private key. |

Another computer clones this repo and runs the script. The script copies those
files into `%ProgramData%\Membrane\ndis-package`, imports the public
certificate into `LocalMachine\Root` and `LocalMachine\TrustedPublisher`, and
binds the filter. It does not build a catalog and it does not need the WDK
or `signtool`. The WDK is only involved when `-SysPath` points at a different
image, because that image needs a new catalog.

Image search order:

1. `-SysPath`
2. `-PackageDir\membrane_ndis.sys`
3. `windows\membrane_ndis.sys` next to the script (the copy in this repo)
4. `C:\td\kdrv2\membrane_ndis.sys` (build PC only)
5. `C:\Build\networking\membrane-ndis\install_staging\membrane_ndis.sys` (build PC only)
6. `C:\Build\networking\ndis-stage\membrane_ndis.sys` (build PC only)
7. `C:\Build\networking\membrane\ndis\install_staging\membrane_ndis.sys` (build PC only)

An INF beside a different `.sys`, or in `-PackageDir`, is used only when the
image hash is not the shipped one. That path rewrites `StartType` to `3`,
runs Inf2Cat, and signs with a `CN=MembraneTest` certificate created in
`Cert:\LocalMachine\My` when that store has no matching private key. The
shipped package leaves the repo files unchanged and does not create a key.

Rebuilding the driver is separate from installing it. The source tree on the
build PC is `C:\Build\networking\membrane-ndis`. A machine that only needs the
filter uses the `.sys` in this repo.

## Check

```powershell
netcfg -s n | Select-String membrane
sc.exe qc membrane_ndis
sc.exe query membrane_ndis
```

`netcfg` lists `membrane_ndis_filter`. `sc qc` shows `START_TYPE : 3 DEMAND_START`
and `BINARY_PATH_NAME : \SystemRoot\System32\drivers\membrane_ndis.sys`. After
a start, `sc query` shows `RUNNING`, and an elevated process can open
`\\.\MembraneNdis`. etherquic then reports the L2 backend as `rust-ndis`.

Open the etherquic receiver from an elevated prompt if `CreateFile` on that
device returns access denied. The driver creates the device with
`IoCreateDevice` and the default kernel ACL.

## Remove

```powershell
sc.exe stop membrane_ndis
sc.exe config membrane_ndis start= disabled
netcfg -u membrane_ndis_filter
```

`start=` is one argument, with a space before `disabled`. If a start hangs or
the machine drops off the network after the filter loads, run those three
commands and rename the image:

```powershell
Rename-Item C:\Windows\System32\drivers\membrane_ndis.sys membrane_ndis.sys.disabled
```

If the file is locked, reboot (the service is disabled, so it stays stopped)
and rename it before starting the service again. Npcap is still the L2 path
while this device is down.

## Manual install

Use this when you want the same result without the script. Do it from an
elevated prompt.

1. Turn Secure Boot off in firmware. Turn Memory integrity off
   (Windows Security, Device security, Core isolation).
2. Enable test signing and allow the test-signed image:

```powershell
bcdedit /set testsigning on
bcdedit /set nointegritychecks on
New-ItemProperty -LiteralPath HKLM:\SYSTEM\CurrentControlSet\Control\CI\Config -Name VulnerableDriverBlocklistEnable -PropertyType DWord -Value 0 -Force
```

Reboot. `bcdedit /enum {current}` then shows `testsigning Yes` and
`nointegritychecks Yes`.

3. Trust the public certificate shipped in the repo. This does not need a
   private key.

```powershell
certutil -addstore -f Root .\windows\MembraneTest.cer
certutil -addstore -f TrustedPublisher .\windows\MembraneTest.cer
```

4. Copy the signed package into one directory. Leave the bytes unchanged.
   The catalog covers `windows/membrane_ndis.sys` and `windows/ndis_driver.inf`.

```powershell
$stage = "$env:ProgramData\Membrane\ndis-package"
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item .\windows\membrane_ndis.sys, .\windows\ndis_driver.inf, .\windows\membrane_ndis.cat $stage
```

A different `.sys` is not covered by that catalog. Building a new one needs
`Inf2Cat.exe` from the WDK and `signtool.exe` from the Windows SDK.

5. Bind the filter, force demand start, and start it:

```powershell
netcfg -l "$stage\ndis_driver.inf" -c s -i membrane_ndis_filter
sc.exe config membrane_ndis start= demand
sc.exe start membrane_ndis
sc.exe query membrane_ndis
```

`netcfg` error `0x8004a029` means the component is already installed. A missing
`NetCfgInstanceId` fails with `0x800f0102`. `sc start` without this `netcfg`
bind returns a failure from `NdisFRegisterFilterDriver`, because the filter
has no bindings yet.
