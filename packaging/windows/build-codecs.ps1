# Build the Windows codec files the MSI ships, from git submodules, into
# target\codecs\stage. build-msi.ps1 copies that directory into the package.
# Pins match nix/pyrowave.nix and the release tags named in the README.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$vendor = Join-Path $root 'vendor'
$work = Join-Path $root 'target\codecs'
$build = Join-Path $work 'build'
$stage = Join-Path $work 'stage'
$gcc = 'C:\msys64\mingw64\bin\gcc.exe'
$gxx = 'C:\msys64\mingw64\bin\g++.exe'
$mingwBin = 'C:\msys64\mingw64\bin'
$vcvars = 'C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat'
$fxcDir = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64'
$python = 'C:\Users\Lyam\AppData\Local\Programs\Python\Python313'
$cmake = 'C:\Program Files\CMake\bin\cmake.exe'
$seven = 'C:\Program Files\7-Zip\7z.exe'

$opusRev = 'v1.6.1'
$pyrowaveRev = 'a0d2e71e98b85f6a23f62407df21a39e9745b875'
$graniteRev = 'fb178c8080d163419e8d20f10715c61c53c1ec9b'
$volkRev = '47cddf7ed97b94118a08aacb548a411188e016cc'
$vkHeadersRev = '6802bb4733b63ed5efd3adb308a6c885ef180ea1'
$libjxlRev = 'v0.12.0'
$mesaVer = '26.2.4'
$mesaSha = '351fc8c8b695878ffb3eaa044b3ead08672a48b1a045e3c3e3975811df0f6695'

$env:GIT_TERMINAL_PROMPT = '0'
# cc1.exe lives outside mingw64\bin and loads libwinpthread from PATH.
$env:PATH = "$mingwBin;C:\Program Files\CMake\bin;" + $env:PATH
foreach ($tool in @($gcc, $gxx, $vcvars, $cmake, $seven)) {
    if (-not (Test-Path $tool)) { throw "codec build needs $tool" }
}

function Step([string]$name) { Write-Output "codec: $name" }

function Run([scriptblock]$cmd) {
    & $cmd
    if ($LASTEXITCODE -ne 0) { throw "command failed ($LASTEXITCODE): $cmd" }
}

function Sync-Pin([string]$path, [string]$url, [string]$rev) {
    $rel = $path.Substring($root.Length).TrimStart('\')
    if (-not (Test-Path (Join-Path $path '.git'))) {
        Push-Location $root
        & git submodule add --force $url $rel
        $added = $LASTEXITCODE
        Pop-Location
        if ($added -ne 0 -or -not (Test-Path (Join-Path $path '.git'))) {
            throw "could not add submodule $url at $rel"
        }
    }
    & git -C $path fetch --depth 1 origin $rev
    if ($LASTEXITCODE -ne 0) { throw "fetch $rev in $path failed" }
    & git -C $path checkout --detach --force FETCH_HEAD
    if ($LASTEXITCODE -ne 0) { throw "checkout $rev in $path failed" }
}

$runtimeDlls = @('libgcc_s_seh-1.dll', 'libstdc++-6.dll', 'libmcfgthread-2.dll', 'libwinpthread-1.dll')

function Copy-Runtime([string]$dll, [string]$dest) {
    $dump = & "$mingwBin\objdump.exe" -p $dll
    $names = @()
    foreach ($line in $dump) {
        if ($line -match 'DLL Name:\s+(\S+)') { $names += $Matches[1] }
    }
    foreach ($name in $runtimeDlls) {
        if ($names -contains $name) {
            Copy-Item (Join-Path $mingwBin $name) (Join-Path $dest $name) -Force
            Write-Output "codec: runtime $name"
        }
    }
}

function Assert-ShippedDlls([string]$path) {
    $allow = @(
        'KERNEL32.dll', 'KERNELBASE.dll', 'ntdll.dll', 'msvcrt.dll',
        'USER32.dll', 'GDI32.dll', 'ADVAPI32.dll', 'SHELL32.dll',
        'ole32.dll', 'OLEAUT32.dll', 'WS2_32.dll', 'bcrypt.dll',
        'RPCRT4.dll', 'SHLWAPI.dll', 'COMCTL32.dll', 'IMM32.dll',
        'SETUPAPI.dll', 'WINMM.dll', 'VERSION.dll', 'USP10.dll',
        'd3d11.dll', 'dxgi.dll', 'D3DCOMPILER_47.dll'
    ) + $runtimeDlls
    $dump = & "$mingwBin\objdump.exe" -p $path
    foreach ($line in $dump) {
        if ($line -match 'DLL Name:\s+(\S+)') {
            $name = $Matches[1]
            if ($allow -notcontains $name) {
                throw "$(Split-Path $path -Leaf) imports $name, which the installer does not ship"
            }
            if ($runtimeDlls -contains $name -and -not (Test-Path (Join-Path $stage $name))) {
                throw "$(Split-Path $path -Leaf) imports $name, and that runtime was not staged"
            }
        }
    }
}

New-Item -ItemType Directory -Force -Path $build, $stage, (Join-Path $stage 'vulkan-cpu') | Out-Null

Step 'sources'
Sync-Pin (Join-Path $vendor 'opus') 'https://github.com/xiph/opus.git' $opusRev
Sync-Pin (Join-Path $vendor 'pyrowave') 'https://github.com/Themaister/pyrowave.git' $pyrowaveRev
Sync-Pin (Join-Path $vendor 'granite') 'https://github.com/Themaister/Granite.git' $graniteRev
Sync-Pin (Join-Path $vendor 'libjxl') 'https://github.com/libjxl/libjxl.git' $libjxlRev
Push-Location $root
& git add -- .gitmodules vendor/opus vendor/pyrowave vendor/granite vendor/libjxl
Pop-Location

$granite = Join-Path $vendor 'granite'
& git -C $granite submodule update --init --depth 1 -- third_party/volk third_party/khronos/vulkan-headers
if ($LASTEXITCODE -ne 0) { throw 'granite submodules were not checked out' }
& git -C (Join-Path $granite 'third_party\volk') fetch --depth 1 origin $volkRev
if ($LASTEXITCODE -ne 0) { throw 'volk fetch failed' }
& git -C (Join-Path $granite 'third_party\volk') checkout --detach --force FETCH_HEAD
if ($LASTEXITCODE -ne 0) { throw 'volk checkout failed' }
& git -C (Join-Path $granite 'third_party\khronos\vulkan-headers') fetch --depth 1 origin $vkHeadersRev
if ($LASTEXITCODE -ne 0) { throw 'Vulkan-Headers fetch failed' }
& git -C (Join-Path $granite 'third_party\khronos\vulkan-headers') checkout --detach --force FETCH_HEAD
if ($LASTEXITCODE -ne 0) { throw 'Vulkan-Headers checkout failed' }

$link = Join-Path $vendor 'pyrowave\Granite'
if (Test-Path $link) {
    $item = Get-Item $link -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { cmd /c "rmdir `"$link`"" | Out-Null }
    elseif (-not (Test-Path (Join-Path $link 'CMakeLists.txt'))) { Remove-Item -Recurse -Force $link }
}
if (-not (Test-Path (Join-Path $link 'CMakeLists.txt'))) {
    cmd /c "mklink /J `"$link`" `"$granite`"" | Out-Null
    if (-not (Test-Path (Join-Path $link 'CMakeLists.txt'))) { throw 'could not junction Granite into pyrowave' }
}

& git -C (Join-Path $vendor 'libjxl') submodule update --init --recursive --depth 1
if ($LASTEXITCODE -ne 0) { throw 'libjxl submodules were not checked out' }

Step 'opus'
$opusBuild = Join-Path $build 'opus'
Run { & $cmake -S (Join-Path $vendor 'opus') -B $opusBuild -G Ninja -DCMAKE_BUILD_TYPE=Release `
    -DCMAKE_C_COMPILER="$gcc" -DBUILD_SHARED_LIBS=ON -DOPUS_BUILD_TESTING=OFF -DOPUS_BUILD_PROGRAMS=OFF }
Run { & $cmake --build $opusBuild --parallel }
$opusDll = Get-ChildItem $opusBuild -Recurse -Filter '*.dll' | Where-Object { $_.Name -like 'libopus*.dll' -or $_.Name -eq 'opus.dll' } | Select-Object -First 1
if (-not $opusDll) { throw 'libopus shared library was not built' }
Copy-Item $opusDll.FullName (Join-Path $stage 'libopus-0.dll') -Force
Copy-Item (Join-Path $vendor 'opus\COPYING') (Join-Path $stage 'LICENSE-opus.txt') -Force

Step 'pyrowave-d3d11'
$dxSrc = Join-Path $vendor 'pyrowave-dx11\src'
$dxBuild = Join-Path $build 'pyrowave-d3d11'
$dxCmd = Join-Path $work 'dx11.cmd'
@"
@echo off
call "$vcvars" >nul
set "PATH=$fxcDir;$python;$python\Scripts;C:\Program Files\CMake\bin;%PATH%"
"$cmake" -S "$dxSrc" -B "$dxBuild" -G "Visual Studio 17 2022" -A x64
if errorlevel 1 exit /b 1
"$cmake" --build "$dxBuild" --config Release --target pyrowave_d3d11 --parallel
if errorlevel 1 exit /b 1
"@ | Set-Content -Path $dxCmd -Encoding ASCII
cmd /c $dxCmd
if ($LASTEXITCODE -ne 0) { throw "pyrowave_d3d11 build failed ($LASTEXITCODE)" }
$dxDll = Get-ChildItem $dxBuild -Recurse -Filter 'pyrowave_d3d11.dll' | Select-Object -First 1
if (-not $dxDll) { throw 'pyrowave_d3d11.dll was not built' }
Copy-Item $dxDll.FullName (Join-Path $stage 'pyrowave_d3d11.dll') -Force
Copy-Item (Join-Path $vendor 'pyrowave-dx11\LICENSE') (Join-Path $stage 'LICENSE-pyrowave-d3d11.txt') -Force

Step 'pyrowave'
$pwBuild = Join-Path $build 'pyrowave'
Run { & $cmake -S (Join-Path $vendor 'pyrowave') -B $pwBuild -G Ninja -DCMAKE_BUILD_TYPE=Release `
    -DCMAKE_C_COMPILER="$gcc" -DCMAKE_CXX_COMPILER="$gxx" `
    -DPYROWAVE_DEVEL=OFF -DPYROWAVE_UTILS=OFF }
Run { & $cmake --build $pwBuild --target pyrowave-shared --parallel }
$pwDll = Get-ChildItem $pwBuild -Recurse -Filter 'libpyrowave-shared*.dll' | Select-Object -First 1
if (-not $pwDll) { throw 'libpyrowave-shared was not built' }
Copy-Item $pwDll.FullName (Join-Path $stage 'libpyrowave-shared-1.dll') -Force
Copy-Item (Join-Path $vendor 'pyrowave\LICENSE') (Join-Path $stage 'LICENSE-pyrowave.txt') -Force
Copy-Runtime $pwDll.FullName $stage
$gccLic = 'C:\msys64\mingw64\share\licenses\gcc-libs\COPYING.RUNTIME'
$mcfLic = Get-ChildItem 'C:\msys64\mingw64\share\licenses' -Recurse -Filter 'COPYING*' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match 'mcfgthread' } | Select-Object -First 1
if ((Test-Path (Join-Path $stage 'libgcc_s_seh-1.dll')) -and (Test-Path $gccLic)) {
    Copy-Item $gccLic (Join-Path $stage 'LICENSE-gcc-runtime.txt') -Force
}
if ((Test-Path (Join-Path $stage 'libmcfgthread-2.dll')) -and $mcfLic) {
    Copy-Item $mcfLic.FullName (Join-Path $stage 'LICENSE-mcfgthread.txt') -Force
}

Step 'cjxl'
# libjxl 0.12 calls the MSVC intrinsic _sub_overflow_i32, which this compiler
# does not provide. MinGW has __builtin_sub_overflow, so cjxl is built with gcc.
# OpenEXR stays off. A found MSYS2 OpenEXR makes cjxl import DLLs the MSI does not ship.
$jxl = Join-Path $vendor 'libjxl'
$jxlBuild = Join-Path $build 'libjxl'
$jxlCache = Join-Path $jxlBuild 'CMakeCache.txt'
$reuse = $false
if (Test-Path $jxlCache) {
    $cache = Get-Content $jxlCache -Raw
    if ($cache -match 'CMAKE_CXX_COMPILER:.*=.*g\+\+\.exe' -and
        $cache -match 'JPEGXL_ENABLE_OPENEXR:BOOL=OFF' -and
        $cache -match 'JPEGXL_STATIC:BOOL=ON') {
        $reuse = $true
    }
}
if (-not $reuse -and (Test-Path $jxlBuild)) { Remove-Item -Recurse -Force $jxlBuild }
Run { & $cmake -S $jxl -B $jxlBuild -G Ninja -DCMAKE_BUILD_TYPE=Release `
    -DCMAKE_C_COMPILER="$gcc" -DCMAKE_CXX_COMPILER="$gxx" `
    -DBUILD_TESTING=OFF -DBUILD_SHARED_LIBS=OFF -DJPEGXL_STATIC=ON `
    -DJPEGXL_ENABLE_OPENEXR=OFF `
    -DJPEGXL_ENABLE_TOOLS=ON -DJPEGXL_ENABLE_BENCHMARK=OFF -DJPEGXL_ENABLE_EXAMPLES=OFF `
    -DJPEGXL_ENABLE_MANPAGES=OFF -DJPEGXL_ENABLE_JNI=OFF -DJPEGXL_ENABLE_SJPEG=OFF `
    -DJPEGXL_ENABLE_VIEWERS=OFF -DJPEGXL_ENABLE_PLUGINS=OFF -DJPEGXL_ENABLE_DEVTOOLS=OFF `
    -DJPEGXL_ENABLE_JPEGLI=OFF -DJPEGXL_FORCE_SYSTEM_BROTLI=OFF -DJPEGXL_FORCE_SYSTEM_HWY=OFF `
    -DJPEGXL_FORCE_SYSTEM_LCMS2=OFF }
Run { & $cmake --build $jxlBuild --target cjxl --parallel }
$cjxl = Get-ChildItem $jxlBuild -Recurse -Filter 'cjxl.exe' | Select-Object -First 1
if (-not $cjxl) { throw 'cjxl.exe was not built' }
Copy-Item $cjxl.FullName (Join-Path $stage 'cjxl.exe') -Force
Copy-Runtime $cjxl.FullName $stage
$jxlLic = Join-Path $jxl 'LICENSE'
if (Test-Path $jxlLic) { Copy-Item $jxlLic (Join-Path $stage 'LICENSE.libjxl') -Force }
Get-ChildItem (Join-Path $jxl 'third_party') -Directory -ErrorAction SilentlyContinue | ForEach-Object {
    $notice = Get-ChildItem $_.FullName -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'LICENSE*' -or $_.Name -eq 'COPYING' } | Select-Object -First 1
    if ($notice) { Copy-Item $notice.FullName (Join-Path $stage ("LICENSE." + $_.Name)) -Force }
}

Step 'lavapipe'
$vkDir = Join-Path $stage 'vulkan-cpu'
$archive = Join-Path $work "mesa3d-$mesaVer-release-msvc.7z"
$url = "https://github.com/pal1000/mesa-dist-win/releases/download/$mesaVer/mesa3d-$mesaVer-release-msvc.7z"
if (-not (Test-Path $archive) -or (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower() -ne $mesaSha) {
    Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
}
$hash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
if ($hash -ne $mesaSha) { throw "mesa archive sha256 is $hash" }
$extracted = Join-Path $work 'mesa-extract'
if (Test-Path $extracted) { Remove-Item -Recurse -Force $extracted }
New-Item -ItemType Directory -Force -Path $extracted | Out-Null
& $seven e -y "-o$extracted" $archive 'x64/vulkan_lvp.dll'
if ($LASTEXITCODE -ne 0) { throw '7z did not extract vulkan_lvp.dll' }
Copy-Item (Join-Path $extracted 'vulkan_lvp.dll') (Join-Path $vkDir 'vulkan_lvp.dll') -Force
$shim = Join-Path $PSScriptRoot 'lvp-shim\vulkan_shim.c'
& $gcc -O2 -shared -static-libgcc -s -o (Join-Path $vkDir 'vulkan-1.dll') $shim
if ($LASTEXITCODE -ne 0) { throw 'vulkan-1.dll shim failed to link' }
$licenseRst = Join-Path $work 'license.rst'
Invoke-WebRequest -Uri "https://gitlab.freedesktop.org/mesa/mesa/-/raw/mesa-$mesaVer/docs/license.rst" -OutFile $licenseRst -UseBasicParsing
@(
    "vulkan_lvp.dll is Mesa $mesaVer lavapipe, built by https://github.com/pal1000/mesa-dist-win"
    "(release $mesaVer, mesa3d-$mesaVer-release-msvc.7z, sha256 $mesaSha)."
    'It statically links LLVM (Apache License 2.0 with LLVM Exceptions).'
    "vulkan-1.dll is etherquic's shim (packaging/windows/lvp-shim), same licence as etherquic."
    ''
    (Get-Content $licenseRst -Raw)
) | Set-Content -Path (Join-Path $vkDir 'LICENSE-mesa.txt') -Encoding ASCII

foreach ($name in @(
    'libopus-0.dll', 'libpyrowave-shared-1.dll', 'pyrowave_d3d11.dll', 'cjxl.exe',
    'vulkan-cpu\vulkan-1.dll', 'vulkan-cpu\vulkan_lvp.dll'
)) {
    Assert-ShippedDlls (Join-Path $stage $name)
}

$required = @(
    'libopus-0.dll', 'LICENSE-opus.txt',
    'pyrowave_d3d11.dll', 'LICENSE-pyrowave-d3d11.txt',
    'libpyrowave-shared-1.dll', 'LICENSE-pyrowave.txt',
    'cjxl.exe', 'LICENSE.libjxl',
    'vulkan-cpu\vulkan-1.dll', 'vulkan-cpu\vulkan_lvp.dll', 'vulkan-cpu\LICENSE-mesa.txt'
)
foreach ($name in $required) {
    $path = Join-Path $stage $name
    if (-not (Test-Path $path)) { throw "codec stage is missing $name" }
    $len = (Get-Item $path).Length
    Write-Output "codec: $len $name"
}
Write-Output "codec: staged $stage"
