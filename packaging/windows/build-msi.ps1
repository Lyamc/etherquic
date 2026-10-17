# Build dist\etherquic-<version>-windows-x64.msi (WiX 5: dotnet tool install --global wix --version 5.0.2).
#   powershell -File packaging\windows\build-msi.ps1 [-Version 0.1.6] [-Cjxl DIR] [-Codecs DIR] [-VulkanCpu DIR] [-Npcap DIR]
# -Version defaults to the root Cargo package version.
# Codec libraries are built from git submodules by build-codecs.ps1 and always staged.
# -Codecs, -Cjxl, and -VulkanCpu add to that set; they do not replace it.
# -VulkanCpu is staged as vulkan-cpu\ so PyroWave works on PCs without a GPU Vulkan driver.
# Builds etherquic-host and the app (both link Npcap's wpcap.lib), etherquic-setup and the etherquic app,
# stages them with the Membrane driver package and the codecs, and runs wix build.
param(
    [string]$Version = "",
    [string]$Cjxl = "",
    [string]$Codecs = "",
    [string]$VulkanCpu = "",
    [string]$Npcap = "C:\Build\networking\borderless\third_party\npcap\lib"
)
$ErrorActionPreference = "Stop"
if (-not $Version) {
    $manifest = Join-Path $PSScriptRoot "..\..\Cargo.toml"
    $inPackage = $false
    foreach ($line in Get-Content $manifest) {
        if ($line -match '^\[package\]') { $inPackage = $true; continue }
        if ($inPackage -and $line -match '^\[') { break }
        if ($inPackage -and $line -match '^version = "([^"]+)"') {
            $Version = $Matches[1]
            break
        }
    }
    if (-not $Version) { throw "package version missing from $manifest" }
}

# ui/build.rs writes PNG images inside the .ico. Windows Installer stores that
# file in the Icon table and wants a classic BMP icon, so convert before wix.
function Convert-PngIcoToBmp([string]$src, [string]$dest) {
    Add-Type -AssemblyName System.Drawing
    $bytes = [IO.File]::ReadAllBytes($src)
    $count = [BitConverter]::ToUInt16($bytes, 4)
    $images = New-Object System.Collections.Generic.List[byte[]]
    $metas = @()
    for ($i = 0; $i -lt $count; $i++) {
        $e = 6 + ($i * 16)
        $len = [BitConverter]::ToInt32($bytes, $e + 8)
        $ofs = [BitConverter]::ToInt32($bytes, $e + 12)
        $png = New-Object byte[] $len
        [Array]::Copy($bytes, $ofs, $png, 0, $len)
        $ms = New-Object System.IO.MemoryStream(,$png)
        $bmp = New-Object System.Drawing.Bitmap $ms
        $rect = New-Object System.Drawing.Rectangle 0, 0, $bmp.Width, $bmp.Height
        $fmt = [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
        $clone = $bmp.Clone($rect, $fmt)
        $bmp.Dispose()
        $ms.Dispose()
        $locked = $clone.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, $fmt)
        $raw = New-Object byte[] ($locked.Stride * $clone.Height)
        [Runtime.InteropServices.Marshal]::Copy($locked.Scan0, $raw, 0, $raw.Length)
        $stride = $locked.Stride
        $clone.UnlockBits($locked)
        $width = $clone.Width
        $height = $clone.Height
        $clone.Dispose()
        $xorStride = $width * 4
        $andStride = [int]([math]::Ceiling($width / 32.0) * 4)
        $xorSize = $xorStride * $height
        $dib = New-Object byte[] (40 + $xorSize + ($andStride * $height))
        [BitConverter]::GetBytes([int32]40).CopyTo($dib, 0)
        [BitConverter]::GetBytes([int32]$width).CopyTo($dib, 4)
        [BitConverter]::GetBytes([int32]($height * 2)).CopyTo($dib, 8)
        [BitConverter]::GetBytes([int16]1).CopyTo($dib, 12)
        [BitConverter]::GetBytes([int16]32).CopyTo($dib, 14)
        [BitConverter]::GetBytes([int32]$xorSize).CopyTo($dib, 20)
        for ($y = 0; $y -lt $height; $y++) {
            [Array]::Copy($raw, ($height - 1 - $y) * $stride, $dib, 40 + ($y * $xorStride), $xorStride)
        }
        $images.Add($dib)
        $byteW = if ($width -ge 256) { 0 } else { $width }
        $byteH = if ($height -ge 256) { 0 } else { $height }
        $metas += [pscustomobject]@{ W = $byteW; H = $byteH; Bytes = $dib.Length }
    }
    $out = New-Object System.Collections.Generic.List[byte]
    $out.AddRange([byte[]](0, 0, 1, 0))
    $out.AddRange([byte[]][BitConverter]::GetBytes([uint16]$count))
    $dataOff = 6 + (16 * $count)
    foreach ($m in $metas) {
        $out.Add([byte]$m.W)
        $out.Add([byte]$m.H)
        $out.Add(0)
        $out.Add(0)
        $out.AddRange([byte[]][BitConverter]::GetBytes([uint16]1))
        $out.AddRange([byte[]][BitConverter]::GetBytes([uint16]32))
        $out.AddRange([byte[]][BitConverter]::GetBytes([int32]$m.Bytes))
        $out.AddRange([byte[]][BitConverter]::GetBytes([int32]$dataOff))
        $dataOff += $m.Bytes
    }
    foreach ($img in $images) { $out.AddRange($img) }
    [IO.File]::WriteAllBytes($dest, $out.ToArray())
}

$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $root "target" }
$rel = Join-Path $env:CARGO_TARGET_DIR "release"
if (-not (Test-Path (Join-Path $Npcap "wpcap.lib"))) { throw "wpcap.lib was not found in $Npcap" }

# A caller can already set the Npcap library path and the MSVC linker.
# Replacing that drops the linker this tree needs.
if (-not $env:RUSTFLAGS) { $env:RUSTFLAGS = "-L native=$Npcap" }
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
& (Join-Path $PSScriptRoot "build-codecs.ps1")
$codecStage = Join-Path $root "target\codecs\stage"
Get-ChildItem $codecStage -File | Copy-Item -Destination $stage -Force
$vkBuilt = Join-Path $codecStage "vulkan-cpu"
$vk = Join-Path $stage "vulkan-cpu"
New-Item -ItemType Directory -Force -Path $vk | Out-Null
Copy-Item (Join-Path $vkBuilt "*") $vk -Force
if ($Codecs) {
    Get-ChildItem $Codecs -File | Where-Object { $_.Name -like "*.dll" -or $_.Name -like "LICENSE-*" } |
        Copy-Item -Destination $stage -Force
}
if ($VulkanCpu) {
    foreach ($f in "vulkan-1.dll", "vulkan_lvp.dll", "LICENSE-mesa.txt") { Copy-Item (Join-Path $VulkanCpu $f) $vk -Force }
}
if ($Cjxl) {
    Copy-Item (Join-Path $Cjxl "cjxl.exe") $stage -Force
    Get-ChildItem $Cjxl -Filter "LICENSE.*" | Copy-Item -Destination $stage -Force
}
$ico = Get-ChildItem (Join-Path $env:CARGO_TARGET_DIR "release\build") -Recurse -Filter "etherquic.ico" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $ico) { throw "the ui build did not write etherquic.ico" }

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$arpIco = Join-Path $dist "etherquic.ico"
Convert-PngIcoToBmp $ico.FullName $arpIco
$msi = Join-Path $dist "etherquic-$Version-windows-x64.msi"
wix extension add -g WixToolset.Util.wixext/5.0.2
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
wix extension add -g WixToolset.UI.wixext/5.0.2
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
wix build (Join-Path $PSScriptRoot "etherquic.wxs") -arch x64 -ext WixToolset.Util.wixext -ext WixToolset.UI.wixext `
    -d "Stage=$stage" -d "ProductVersion=$Version" -d "Icon=$arpIco" -o $msi
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Get-Item $msi | Select-Object FullName, Length