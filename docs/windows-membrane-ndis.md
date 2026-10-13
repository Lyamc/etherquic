# Installing Membrane NDIS on Windows

Membrane NDIS is the Windows L2 adapter etherquic opens first. The userspace
path is `\\.\MembraneNdis` (`src/ndis_link.rs`). When that device is absent,
etherquic uses Npcap. Npcap stays installed either way.

The driver is an NDIS lightweight filter:

| Piece | Value |
| --- | --- |
| Component | `membrane_ndis_filter` |
| Service | `membrane_ndis` |
| Image | `%SystemRoot%\System32\drivers\membrane_ndis.sys` |
| Device | `\\.\MembraneNdis` |
| INF | `windows/ndis_driver.inf` |
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
- The filter is not bound: it turns on test signing if needed, signs the
  package, binds `membrane_ndis_filter`, starts the service, and opens the device.
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
- A native-subsystem `membrane_ndis.sys` (PE subsystem 1, not a renamed DLL).
- Windows SDK `signtool.exe` (x64) and WDK `Inf2Cat.exe`. `pnputil /add-driver`
  does not bind an NDIS filter. `netcfg` does, and it rejects a catalog that
  was not produced by Inf2Cat.
- The INF shipped next to the script. `NetCfgInstanceId` is
  `{a1b2c3d4-e5f6-7890-abcd-ef1234567890}`. `StartType` in that file is `3`.
  The script rewrites a staged copy to `3` if another INF still says boot start.

Image search order:

1. `-SysPath`
2. `-PackageDir\membrane_ndis.sys`
3. `membrane_ndis.sys` next to the script
4. `C:\td\kdrv2\membrane_ndis.sys` (the image installed on the build PC; same size and timestamp as the driver in `System32\drivers`)
5. `C:\Build\networking\membrane-ndis\install_staging\membrane_ndis.sys`
6. `C:\Build\networking\ndis-stage\membrane_ndis.sys`
7. `C:\Build\networking\membrane\ndis\install_staging\membrane_ndis.sys`

An INF beside the `.sys` or in `-PackageDir` wins over the copy in this repo.
The script stages into `%ProgramData%\Membrane\ndis-package`, signs that copy,
and leaves the source tree alone.

On a machine that does not already have `CN=MembraneTest` in
`Cert:\LocalMachine\My` with a private key, the script creates a three-year
code-signing certificate and copies the public certificate into
`LocalMachine\Root` and `LocalMachine\TrustedPublisher`. The build PC already
has that certificate (`F0279DCD81DA66DD01CC28404B1F2339BE1CA353`, expires
2027-09-25). A `.sys` signed on one PC loads on another only after that other
PC trusts the signing certificate. Running the script on the target re-signs
with the target's own test certificate.

This document does not build the driver. The driver tree on the build PC is
`C:\Build\networking\membrane-ndis`.

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

3. Create the test certificate once. Skip this when `CN=MembraneTest` is
   already in `Cert:\LocalMachine\My` and has a private key.

```powershell
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=MembraneTest' `
    -CertStoreLocation Cert:\LocalMachine\My -HashAlgorithm SHA256 -KeyLength 2048 `
    -NotAfter (Get-Date).AddYears(3)
$blob = $cert.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert)
foreach ($name in 'Root', 'TrustedPublisher') {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($name, 'LocalMachine')
    $store.Open('ReadWrite')
    $store.Add((New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 -ArgumentList (, $blob)))
    $store.Close()
}
```

4. Stage `membrane_ndis.sys` and `windows/ndis_driver.inf` in one directory.
   Confirm the INF says `StartType = 3`. Build the catalog and sign both files.
   `Inf2Cat.exe` is under the WDK (on the build PC,
   `%USERPROFILE%\.nuget\packages\microsoft.windows.wdk.x64\*\c\bin\*\x86\Inf2Cat.exe`).
   `signtool.exe` is under
   `C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\`.

```powershell
$stage = "$env:ProgramData\Membrane\ndis-package"
Inf2Cat.exe /driver:$stage /os:10_X64
signtool.exe sign /sm /s My /n CN=MembraneTest /fd SHA256 `
    "$stage\membrane_ndis.sys" "$stage\membrane_ndis.cat"
signtool.exe verify /pa "$stage\membrane_ndis.sys"
```

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
