# PATH for a non-interactive OpenSSH session on AN515, then build-msi.ps1.
#   powershell -NoProfile -File packaging\windows\remote-msi.ps1 -Version 0.1.10
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Npcap = "C:\Build\o3-borderless\third_party\npcap\lib"
)
$ErrorActionPreference = "Stop"
$env:PATH = "C:\Program Files\Git\cmd;C:\Program Files\Git\usr\bin;C:\Users\Lyam\.rustup\toolchains\nightly-x86_64-pc-windows-msvc\bin;C:\Users\Lyam\.dotnet\tools;" + $env:PATH
$env:CARGO_BUILD_JOBS = "1"
$env:GIT_TERMINAL_PROMPT = "0"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $root
& powershell -NoProfile -File (Join-Path $PSScriptRoot "build-msi.ps1") -Version $Version -Npcap $Npcap
if ($LASTEXITCODE -ne 0) { throw "msi build failed $LASTEXITCODE" }
$msi = Join-Path $root "dist\etherquic-$Version-windows-x64.msi"
Write-Output "MSI_DONE"
Get-Item $msi | Format-List FullName, Length, LastWriteTime
