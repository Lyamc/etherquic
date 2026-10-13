#Requires -Version 5.1
#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Install and start the Membrane NDIS L2 filter, or remove it.

.DESCRIPTION
    Binds the lightweight filter membrane_ndis_filter so etherquic can open
    \\.\MembraneNdis. The service stays demand-start. Test signing is turned
    on when it is off, which takes a reboot before the driver can load.

    Run this from an elevated PowerShell on the machine itself. Binding the
    filter attaches it to the machine's adapters.

.PARAMETER PackageDir
    Directory that contains membrane_ndis.sys. An ndis_driver.inf in that
    directory is used when present. Otherwise the INF next to this script is used.

.PARAMETER SysPath
    Full path to membrane_ndis.sys. Overrides the search path.

.PARAMETER Reinstall
    Unbind an existing filter and install the packaged image again.

.PARAMETER Uninstall
    Stop the driver, set it to disabled, and unbind the filter.

.PARAMETER Quarantine
    With -Uninstall, rename the installed membrane_ndis.sys so it cannot load.

.PARAMETER NoStart
    Install the filter and leave the service stopped.

.PARAMETER Preflight
    Print the checks and the image that would be installed. Change nothing.
#>
[CmdletBinding()]
param(
    [string]$PackageDir,
    [string]$SysPath,
    [switch]$Reinstall,
    [switch]$Uninstall,
    [switch]$Quarantine,
    [switch]$NoStart,
    [switch]$Preflight
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ComponentId = 'membrane_ndis_filter'
$ServiceName = 'membrane_ndis'
$StageRoot = Join-Path $env:ProgramData 'Membrane\ndis-package'
$NetCfg = Join-Path $env:SystemRoot 'System32\netcfg.exe'
$BcdEdit = Join-Path $env:SystemRoot 'System32\bcdedit.exe'
$Script:BundledInf = Join-Path $PSScriptRoot 'ndis_driver.inf'

function Write-Step {
    param([string]$Message)
    Write-Host $Message
}

function Get-BcdFlag {
    param([string]$Name)
    $out = & $BcdEdit /enum '{current}'
    if ($LASTEXITCODE -ne 0) {
        throw "bcdedit /enum failed with exit $LASTEXITCODE"
    }
    $pattern = '^\s*' + [regex]::Escape($Name) + '\s+'
    $line = @($out | Where-Object { $_ -match $pattern } | Select-Object -First 1)
    if ($line.Count -eq 0) {
        return $false
    }
    return [bool]($line[0] -match '\sYes\s*$')
}

function Set-BcdFlagOn {
    param([string]$Name)
    & $BcdEdit /set $Name on | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "bcdedit /set $Name on failed with exit $LASTEXITCODE"
    }
}

function Test-SecureBootOn {
    try {
        return [bool](Confirm-SecureBootUEFI)
    } catch {
        return $false
    }
}

function Test-MemoryIntegrityOn {
    $dg = Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard -ErrorAction SilentlyContinue
    if (-not $dg) {
        return $false
    }
    $prop = $dg.PSObject.Properties['SecurityServicesRunning']
    if (-not $prop) {
        return $false
    }
    return @($prop.Value) -contains 2
}

function Get-BlocklistValue {
    $key = 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Config'
    if (-not (Test-Path -LiteralPath $key)) {
        return $null
    }
    $item = Get-ItemProperty -LiteralPath $key -Name VulnerableDriverBlocklistEnable -ErrorAction SilentlyContinue
    if (-not $item) {
        return $null
    }
    return [int]$item.VulnerableDriverBlocklistEnable
}

function Set-BlocklistOff {
    $key = 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Config'
    if (-not (Test-Path -LiteralPath $key)) {
        New-Item -Path $key -Force | Out-Null
    }
    New-ItemProperty -LiteralPath $key -Name VulnerableDriverBlocklistEnable -PropertyType DWord -Value 0 -Force | Out-Null
}

function Test-FilterBound {
    $listed = & $NetCfg -s n | Out-String
    return $listed -match [regex]::Escape($ComponentId)
}

function Test-ServiceExists {
    & sc.exe query $ServiceName 1>$null 2>$null
    return $LASTEXITCODE -eq 0
}

function Set-DemandStart {
    # sc.exe treats "start=" as one argument. The space before the value is required.
    & sc.exe config $ServiceName start= demand | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "sc.exe config $ServiceName start= demand failed with exit $LASTEXITCODE"
    }
}

function Stop-MembraneService {
    if (-not (Test-ServiceExists)) {
        return
    }
    & sc.exe stop $ServiceName | Out-Null
    for ($i = 0; $i -lt 20; $i++) {
        $query = & sc.exe query $ServiceName | Out-String
        if ($query -match 'STOPPED') {
            return
        }
        Start-Sleep -Milliseconds 250
    }
    throw 'membrane_ndis did not stop.'
}

function Find-KitExe {
    param(
        [string]$FileName,
        [string]$Arch
    )
    $roots = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'),
        (Join-Path $env:ProgramFiles 'Windows Kits\10\bin')
    )
    $hits = @()
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) {
            continue
        }
        $hits += @(Get-ChildItem -LiteralPath $root -Recurse -Filter $FileName -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match [regex]::Escape("\$Arch\") })
    }
    if ($hits.Count -gt 0) {
        $best = $hits | Sort-Object {
            if ($_.FullName -match '10\.0\.(\d+)\.(\d+)') {
                [version]"10.0.$($Matches[1]).$($Matches[2])"
            } else {
                [version]'0.0.0.0'
            }
        } -Descending | Select-Object -First 1
        return $best.FullName
    }
    return $null
}

function Find-Inf2Cat {
    $kit = Find-KitExe -FileName 'Inf2Cat.exe' -Arch 'x86'
    if ($kit) {
        return $kit
    }
    $nugetRoots = @(
        (Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windows.wdk.x64'),
        'C:\Users\Lyam\Build\wdk-nuget'
    )
    foreach ($root in $nugetRoots) {
        if (-not (Test-Path -LiteralPath $root)) {
            continue
        }
        $found = Get-ChildItem -LiteralPath $root -Recurse -Filter 'Inf2Cat.exe' -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($found) {
            return $found.FullName
        }
    }
    return $null
}

function Find-MembraneSys {
    $candidates = @()
    if ($SysPath) {
        $candidates += $SysPath
    }
    if ($PackageDir) {
        $candidates += (Join-Path $PackageDir 'membrane_ndis.sys')
    }
    $candidates += @(
        (Join-Path $PSScriptRoot 'membrane_ndis.sys'),
        'C:\td\kdrv2\membrane_ndis.sys',
        'C:\Build\networking\membrane-ndis\install_staging\membrane_ndis.sys',
        'C:\Build\networking\ndis-stage\membrane_ndis.sys',
        'C:\Build\networking\membrane\ndis\install_staging\membrane_ndis.sys'
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    return $null
}

function Find-SourceInf {
    param([string]$Sys)
    if ($PackageDir) {
        $besidePackage = Join-Path $PackageDir 'ndis_driver.inf'
        if (Test-Path -LiteralPath $besidePackage) {
            return (Resolve-Path -LiteralPath $besidePackage).Path
        }
    }
    $besideSys = Join-Path (Split-Path -Parent $Sys) 'ndis_driver.inf'
    if (Test-Path -LiteralPath $besideSys) {
        return (Resolve-Path -LiteralPath $besideSys).Path
    }
    if (Test-Path -LiteralPath $Script:BundledInf) {
        return (Resolve-Path -LiteralPath $Script:BundledInf).Path
    }
    return $null
}

function Assert-NativeDriver {
    param([string]$Path)
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 0x200 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "$Path is not a PE image."
    }
    $lfanew = [BitConverter]::ToInt32($bytes, 0x3C)
    $subsystem = [BitConverter]::ToUInt16($bytes, $lfanew + 24 + 68)
    $characteristics = [BitConverter]::ToUInt16($bytes, $lfanew + 22)
    if ($subsystem -ne 1) {
        throw "$Path has PE subsystem $subsystem. A kernel driver must be subsystem 1 (NATIVE)."
    }
    if (($characteristics -band 0x2000) -ne 0) {
        throw "$Path is a DLL. Rename of membrane_ndis.dll to .sys will not load."
    }
}

function Get-MembraneCert {
    $existing = @(Get-ChildItem -LiteralPath 'Cert:\LocalMachine\My' |
        Where-Object { $_.Subject -eq 'CN=MembraneTest' -and $_.HasPrivateKey } |
        Sort-Object NotAfter -Descending)
    if ($existing.Count -gt 0) {
        $cert = $existing[0]
    } else {
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=MembraneTest' `
            -CertStoreLocation 'Cert:\LocalMachine\My' -HashAlgorithm SHA256 -KeyLength 2048 `
            -NotAfter (Get-Date).AddYears(3)
        Write-Step "Created test certificate CN=MembraneTest $($cert.Thumbprint)"
    }
    $blob = $cert.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert)
    foreach ($storeName in @('Root', 'TrustedPublisher')) {
        $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($storeName, 'LocalMachine')
        try {
            $store.Open('ReadWrite')
            $have = @($store.Certificates | Where-Object { $_.Thumbprint -eq $cert.Thumbprint })
            if ($have.Count -eq 0) {
                $publicCert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 -ArgumentList (, $blob)
                $store.Add($publicCert)
                Write-Step "Trusted CN=MembraneTest in LocalMachine\$storeName"
            }
        } finally {
            $store.Close()
        }
    }
    return $cert
}

function Publish-Package {
    param(
        [string]$SourceSys,
        [string]$SourceInf
    )
    New-Item -ItemType Directory -Force -Path $StageRoot | Out-Null
    $destSys = Join-Path $StageRoot 'membrane_ndis.sys'
    $destInf = Join-Path $StageRoot 'ndis_driver.inf'
    Copy-Item -LiteralPath $SourceSys -Destination $destSys -Force
    $infText = [System.IO.File]::ReadAllText($SourceInf)
    # Boot start loads this filter before sign-in. The installed service stays demand-start.
    $infText = [regex]::Replace($infText, '(?m)^(\s*StartType\s*=\s*)\d+', '${1}3')
    if ($infText -notmatch '(?m)^\s*StartType\s*=\s*3\s*$') {
        throw "INF $SourceInf has no StartType line."
    }
    if ($infText -notmatch 'membrane_ndis_filter') {
        throw "INF $SourceInf does not install membrane_ndis_filter."
    }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($destInf, $infText, $utf8)
    return @{ Sys = $destSys; Inf = $destInf }
}

function Protect-CatalogAndSign {
    param(
        $Package,
        $Cert,
        [string]$Inf2Cat,
        [string]$SignTool
    )
    $prev = Get-Location
    try {
        Set-Location -LiteralPath $StageRoot
        & $Inf2Cat /driver:$StageRoot /os:10_X64
        if ($LASTEXITCODE -ne 0) {
            throw "Inf2Cat failed with exit $LASTEXITCODE"
        }
    } finally {
        Set-Location $prev
    }
    $cat = Join-Path $StageRoot 'membrane_ndis.cat'
    if (-not (Test-Path -LiteralPath $cat)) {
        throw "Inf2Cat did not write $cat"
    }
    & $SignTool sign /sm /s My /sha1 $Cert.Thumbprint /fd SHA256 $Package.Sys $cat
    if ($LASTEXITCODE -ne 0) {
        throw "signtool sign failed with exit $LASTEXITCODE"
    }
    & $SignTool verify /pa $Package.Sys
    if ($LASTEXITCODE -ne 0) {
        throw "signtool verify failed with exit $LASTEXITCODE"
    }
}

function Install-Filter {
    param($Package)
    $output = & $NetCfg -l $Package.Inf -c s -i $ComponentId | Out-String
    Write-Host $output
    $already = $output -match '8004a029'
    if ($LASTEXITCODE -ne 0 -and -not $already) {
        throw "netcfg install failed with exit ${LASTEXITCODE}: $output"
    }
    if (-not (Test-FilterBound)) {
        throw "netcfg did not list $ComponentId. Output: $output"
    }
}

function Start-MembraneService {
    $output = & sc.exe start $ServiceName | Out-String
    Write-Host $output
    $alreadyRunning = $output -match '1056'
    if ($LASTEXITCODE -ne 0 -and -not $alreadyRunning) {
        throw "sc.exe start $ServiceName failed with exit ${LASTEXITCODE}: $output"
    }
    for ($i = 0; $i -lt 20; $i++) {
        $query = & sc.exe query $ServiceName | Out-String
        if ($query -match 'RUNNING') {
            return
        }
        Start-Sleep -Milliseconds 250
    }
    throw "membrane_ndis did not reach RUNNING."
}

function Test-MembraneDevice {
    $probe = Join-Path $env:TEMP 'membrane-ndis-probe.ps1'
    $outLog = Join-Path $env:TEMP 'membrane-ndis-probe.out'
    $errLog = Join-Path $env:TEMP 'membrane-ndis-probe.err'
    @'
$ErrorActionPreference = "Stop"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class MembraneProbe {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr h);
}
"@
$access = [Convert]::ToUInt32('C0000000', 16)
$handle = [MembraneProbe]::CreateFileW('\\.\MembraneNdis', $access, [uint32]3, [IntPtr]::Zero, [uint32]3, [uint32]0, [IntPtr]::Zero)
if ($handle.ToInt64() -eq -1) {
    $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    Write-Output "OPEN_FAIL $err"
    exit 2
}
[void][MembraneProbe]::CloseHandle($handle)
Write-Output "OPEN_OK"
exit 0
'@ | Set-Content -LiteralPath $probe -Encoding ASCII
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $proc = Start-Process -FilePath $powershell -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $probe
    ) -PassThru -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    if (-not $proc.WaitForExit(8000)) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        return 'OPEN_HUNG'
    }
    $text = ''
    if (Test-Path -LiteralPath $outLog) {
        $text = (Get-Content -LiteralPath $outLog -Raw)
    }
    if (-not $text) {
        return "OPEN_FAIL exit=$($proc.ExitCode)"
    }
    return $text.Trim()
}

function Disable-Membrane {
    Stop-MembraneService
    if (Test-ServiceExists) {
        & sc.exe config $ServiceName start= disabled | Out-Null
    }
    if (Test-FilterBound) {
        $output = & $NetCfg -u $ComponentId | Out-String
        Write-Host $output
        if ($LASTEXITCODE -ne 0) {
            throw "netcfg -u $ComponentId failed with exit ${LASTEXITCODE}: $output"
        }
    }
    if ($Quarantine -and $Uninstall) {
        $installed = Join-Path $env:SystemRoot 'System32\drivers\membrane_ndis.sys'
        if (Test-Path -LiteralPath $installed) {
            $dest = "$installed.disabled"
            if (Test-Path -LiteralPath $dest) {
                Remove-Item -LiteralPath $dest -Force
            }
            Rename-Item -LiteralPath $installed -NewName 'membrane_ndis.sys.disabled'
            Write-Step "Renamed $installed to membrane_ndis.sys.disabled"
        }
    }
    if ($Uninstall) {
        Write-Step 'Membrane NDIS filter is unbound and the service is disabled.'
    }
}

if ($Uninstall) {
    if ($Preflight) {
        Write-Step 'Preflight: would stop membrane_ndis, set start= disabled, and unbind membrane_ndis_filter.'
        exit 0
    }
    Disable-Membrane
    exit 0
}

$reboot = $false
$secureBoot = Test-SecureBootOn
$testSigning = Get-BcdFlag -Name 'testsigning'
$noIntegrity = Get-BcdFlag -Name 'nointegritychecks'
$blocklist = Get-BlocklistValue
$bound = Test-FilterBound
$sys = Find-MembraneSys

Write-Step "Secure Boot: $(if ($secureBoot) { 'On' } else { 'Off' })"
Write-Step "testsigning: $(if ($testSigning) { 'Yes' } else { 'No' })"
Write-Step "nointegritychecks: $(if ($noIntegrity) { 'Yes' } else { 'No' })"
Write-Step "VulnerableDriverBlocklistEnable: $blocklist"
Write-Step "membrane_ndis_filter bound: $bound"
if ($sys) {
    Write-Step "driver image: $sys"
} else {
    Write-Step 'driver image: not found'
}
if (Test-MemoryIntegrityOn) {
    Write-Warning 'Memory integrity (HVCI) is on. Turn it off before a test-signed driver can load, then reboot.'
}

if ($Preflight) {
    if (-not $bound -or $Reinstall) {
        $inf2 = Find-Inf2Cat
        $sign = Find-KitExe -FileName 'signtool.exe' -Arch 'x64'
        Write-Step "Inf2Cat: $inf2"
        Write-Step "signtool: $sign"
        if ($sys) {
            $inf = Find-SourceInf -Sys $sys
            Write-Step "INF: $inf"
        }
    }
    if ($secureBoot) {
        Write-Step 'Preflight: Secure Boot is on. Turn it off in firmware before installing.'
        exit 1
    }
    Write-Step 'Preflight only. Nothing was changed.'
    exit 0
}

if ($secureBoot) {
    throw 'Secure Boot is on. Turn it off in firmware, reboot, and run this script again.'
}

if (-not $testSigning) {
    Set-BcdFlagOn -Name 'testsigning'
    Write-Step 'Set testsigning on.'
    $reboot = $true
}
if (-not $noIntegrity) {
    Set-BcdFlagOn -Name 'nointegritychecks'
    Write-Step 'Set nointegritychecks on.'
    $reboot = $true
}
if ($blocklist -ne 0) {
    Set-BlocklistOff
    Write-Step 'Set VulnerableDriverBlocklistEnable to 0.'
    $reboot = $true
}
if ($reboot) {
    Write-Step 'Reboot this PC, then run this script again. Test signing is not active until then.'
    exit 3010
}

if ($bound -and -not $Reinstall) {
    Set-DemandStart
    if (-not $NoStart) {
        Start-MembraneService
        $opened = Test-MembraneDevice
        Write-Step $opened
        if ($opened -eq 'OPEN_HUNG') {
            Stop-MembraneService
            & sc.exe config $ServiceName start= disabled | Out-Null
            throw 'Opening \\.\MembraneNdis hung. The service was stopped and set to disabled. The filter is still bound. Unbind with: netcfg -u membrane_ndis_filter'
        }
        if ($opened -notmatch '^OPEN_OK') {
            throw "\\.\MembraneNdis did not open ($opened). The service was left demand-start."
        }
    }
    Write-Step 'Membrane NDIS is installed. Service start type is demand.'
    exit 0
}

if (-not $sys) {
    throw @"
membrane_ndis.sys was not found. Copy it next to this script, or pass -SysPath / -PackageDir.
Looked beside the script and in:
  C:\td\kdrv2\membrane_ndis.sys
  C:\Build\networking\membrane-ndis\install_staging\membrane_ndis.sys
  C:\Build\networking\ndis-stage\membrane_ndis.sys
  C:\Build\networking\membrane\ndis\install_staging\membrane_ndis.sys
"@
}

$sourceInf = Find-SourceInf -Sys $sys
if (-not $sourceInf) {
    throw "ndis_driver.inf was not found next to $sys or next to this script."
}
Assert-NativeDriver -Path $sys

$inf2Cat = Find-Inf2Cat
$signTool = Find-KitExe -FileName 'signtool.exe' -Arch 'x64'
if (-not $inf2Cat) {
    throw 'Inf2Cat.exe was not found. Install the Windows Driver Kit, or the microsoft.windows.wdk.x64 NuGet package.'
}
if (-not $signTool) {
    throw 'signtool.exe was not found. Install the Windows SDK signing tools.'
}

if ($bound -and $Reinstall) {
    Write-Step 'Reinstall: unbinding the current filter.'
    Disable-Membrane
}

$cert = Get-MembraneCert
Write-Step "Signing certificate $($cert.Thumbprint)"
$package = Publish-Package -SourceSys $sys -SourceInf $sourceInf
Write-Step "Staged $($package.Sys)"
Protect-CatalogAndSign -Package $package -Cert $cert -Inf2Cat $inf2Cat -SignTool $signTool
Install-Filter -Package $package
Set-DemandStart

if ($NoStart) {
    Write-Step 'Filter bound. Service was left stopped (demand start).'
    exit 0
}

Start-MembraneService
$opened = Test-MembraneDevice
Write-Step $opened
if ($opened -eq 'OPEN_HUNG') {
    Stop-MembraneService
    & sc.exe config $ServiceName start= disabled | Out-Null
    throw 'Opening \\.\MembraneNdis hung. The service was stopped and set to disabled. Unbind with: netcfg -u membrane_ndis_filter'
}
if ($opened -notmatch '^OPEN_OK') {
    throw "\\.\MembraneNdis did not open ($opened)."
}
Write-Step 'Membrane NDIS is running. \\.\MembraneNdis opened. Service start type is demand.'
exit 0
