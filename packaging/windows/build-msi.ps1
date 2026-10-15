# Build dist\etherquic-<version>-windows-x64.msi (WiX 5: dotnet tool install --global wix --version 5.0.2).
#   powershell -File packaging\windows\build-msi.ps1 [-Version 0.1.0] [-Cjxl DIR] [-Codecs DIR] [-Npcap DIR]
# -Codecs is a directory with libopus-0.dll, libpyrowave-shared-1.dll, the MinGW runtime
# DLLs and their LICENSE-*.txt files (see packaging/windows/codecs.sh).
# Builds etherquic-host and the app (both link Npcap's wpcap.lib), etherquic-setup and the etherquic app,
# stages them with the Membrane driver package and cjxl, and runs wix build.
param(
    [string]$Version = "0.1.0",
    [string]$Cjxl = "",
    [string]$Codecs = "",
    [string]$Npcap = "C:\Build\networking\borderless\third_party\npcap\lib"
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $root "target" }
$rel = Join-Path $env:CARGO_TARGET_DIR "release"
if (-not (Test-Path (Join-Path $Npcap "wpcap.lib"))) { throw "wpcap.lib was not found in $Npcap" }

$env:RUSTFLAGS = "-L native=$Npcap"
cargo build --release --manifest-path (Join-Path $root "host\Cargo.toml") --bin etherquic-host
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release --manifest-path (Join-Path $root "setup\Cargo.toml") --bin etherquic-setup
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release --manifest-path (Join-Path $root "ui\Cargo.toml")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$stage = Join-Path $root "dist\msi-stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path (Join-Path $stage "membrane") | Out-Null
foreach ($f in "etherquic-host.exe", "etherquic-setup.exe", "etherquic.exe") { Copy-Item (Join-Path $rel $f) $stage }
Copy-Item (Join-Path $root "certs\cert.pem"), (Join-Path $root "certs\key.pem") $stage
Copy-Item (Join-Path $PSScriptRoot "msi-setup.ps1") $stage
Get-ChildItem (Join-Path $root "windows") -File | Copy-Item -Destination (Join-Path $stage "membrane")
Copy-Item (Join-Path $PSScriptRoot "run-logged.ps1") (Join-Path $stage "membrane")
if ($Codecs) {
    Get-ChildItem $Codecs -File | Where-Object { $_.Name -like "*.dll" -or $_.Name -like "LICENSE-*" } |
        Copy-Item -Destination $stage
}
if ($Cjxl) {
    Copy-Item (Join-Path $Cjxl "cjxl.exe") $stage
    Get-ChildItem $Cjxl -Filter "LICENSE.*" | Copy-Item -Destination $stage
}

$dist = Join-Path $root "dist"
$msi = Join-Path $dist "etherquic-$Version-windows-x64.msi"
wix extension add -g WixToolset.Util.wixext/5.0.2
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
wix build (Join-Path $PSScriptRoot "etherquic.wxs") -arch x64 -ext WixToolset.Util.wixext `
    -d "Stage=$stage" -d "ProductVersion=$Version" -o $msi
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Get-Item $msi | Select-Object FullName, Length