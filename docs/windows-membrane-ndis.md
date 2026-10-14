# Membrane NDIS

Windows opens `\\.\MembraneNdis` first (`src/ndis_link.rs`). A missing device falls back to Npcap. Opening the device does not start the driver. Npcap stays installed either way.

| | |
| --- | --- |
| Component | `membrane_ndis_filter` |
| Service | `membrane_ndis`, demand start (3), not boot-start |
| Image | `windows/membrane_ndis.sys` → `%SystemRoot%\System32\drivers\membrane_ndis.sys` |
| Package | `windows/ndis_driver.inf`, `windows/membrane_ndis.cat`, `windows/MembraneTest.cer` |
| Device | `\\.\MembraneNdis` |

Run this from an elevated console on the machine. Binding touches every adapter and can drop the network for a moment.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\windows\install-membrane-ndis.ps1
```

Safe to run again.

- Already bound: demand-start, start, and open `\\.\MembraneNdis`.
- Not bound: trust the shipped certificate, `netcfg` bind, start, open. The WDK is not used. `pnputil /add-driver` does not bind an NDIS filter.
- Test signing was off: the script turns on test signing and sets `VulnerableDriverBlocklistEnable=0`, then exits `3010`. Reboot and run the same command. The driver cannot load on the boot where those flags were first set.

```powershell
-Preflight                         # change nothing
-NoStart                           # bind, leave stopped
-Reinstall -SysPath PATH\to.sys    # other image; WDK catalogs and signs it
-Uninstall
-Uninstall -Quarantine             # also rename the .sys so it cannot start
```

Needs 64-bit Windows 10 or 11, Secure Boot off, and Memory integrity off. `NetCfgInstanceId` is `{a1b2c3d4-e5f6-7890-abcd-ef1234567890}`.

The shipped image is 20,848 bytes, SHA-256 `d3884252d69a4994c978a0ba747035a3de45abee6669c4269c193610cf8343b8`, native PE subsystem 1. The certificate is `CN=MembraneTest`, thumbprint `F0279DCD81DA66DD01CC28404B1F2339BE1CA353`, expires 2027-09-25, public only.

A checkout is enough. The script copies the shipped sys, inf, and cat to `%ProgramData%\Membrane\ndis-package`, imports the certificate into `LocalMachine\Root` and `LocalMachine\TrustedPublisher`, and runs `netcfg`. It does not rewrite the shipped INF. `-SysPath` is the path that builds a new catalog. The script also looks beside itself, then in build-machine staging directories (`C:\td\kdrv2`, `C:\Build\networking\membrane-ndis`, `ndis-stage`, `membrane\ndis`). Driver source stays in that membrane tree. This repo does not build it.

## Check

```powershell
netcfg -s n | Select-String membrane
sc.exe qc membrane_ndis
sc.exe query membrane_ndis
```

`netcfg` lists `membrane_ndis_filter`. `sc qc` shows demand start and `\SystemRoot\System32\drivers\membrane_ndis.sys`. After start, `sc query` is `RUNNING` and an elevated process can open `\\.\MembraneNdis`. etherquic then reports backend `rust-ndis`. Access denied means the prompt is not elevated. The device uses the default kernel ACL.

## Remove

```powershell
sc.exe stop membrane_ndis
sc.exe config membrane_ndis start= disabled
netcfg -u membrane_ndis_filter
```

`start=` is one argument, with a space before `disabled`. If the link dies after the filter loads, run those three and rename the image:

```powershell
Rename-Item C:\Windows\System32\drivers\membrane_ndis.sys membrane_ndis.sys.disabled
```

A locked file waits for a reboot. The service is disabled, so it stays stopped. Rename before starting it again. Npcap is the L2 path while the device is down.

## Without the script

Elevated, same result:

1. Secure Boot off. Memory integrity off.
2. Test signing, then reboot. `bcdedit /enum {current}` shows both flags.

```powershell
bcdedit /set testsigning on
bcdedit /set nointegritychecks on
New-ItemProperty -LiteralPath HKLM:\SYSTEM\CurrentControlSet\Control\CI\Config -Name VulnerableDriverBlocklistEnable -PropertyType DWord -Value 0 -Force
```

3. Trust the public certificate. No private key.

```powershell
certutil -addstore -f Root .\windows\MembraneTest.cer
certutil -addstore -f TrustedPublisher .\windows\MembraneTest.cer
```

4. Copy `membrane_ndis.sys`, `ndis_driver.inf`, and `membrane_ndis.cat` unchanged into `%ProgramData%\Membrane\ndis-package`. A different `.sys` needs `Inf2Cat.exe` and `signtool.exe`.

5. Bind, force demand start, and start.

```powershell
netcfg -l "$env:ProgramData\Membrane\ndis-package\ndis_driver.inf" -c s -i membrane_ndis_filter
sc.exe config membrane_ndis start= demand
sc.exe start membrane_ndis
```

`netcfg` `0x8004a029` means the component is already installed. A missing `NetCfgInstanceId` is `0x800f0102`. `sc start` before `netcfg` fails inside `NdisFRegisterFilterDriver` because nothing is bound.
