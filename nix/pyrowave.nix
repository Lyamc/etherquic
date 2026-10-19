# Shared PyroWave library for the Linux shell.
# Pins match packaging/pyrowave/fetch.sh. The shipping build does not compile
# shaders, so Granite's compiler subdirectory (shaderc) is left out.
{
  lib,
  stdenv,
  fetchFromGitHub,
  cmake,
  ninja,
}:

let
  pyrowaveSrc = fetchFromGitHub {
    owner = "Themaister";
    repo = "pyrowave";
    rev = "a0d2e71e98b85f6a23f62407df21a39e9745b875";
    sha256 = "1nhjmkmn81awlbz1kfq9i0yzqhcp9dkdnn16djrqka5hyviwc8k4";
  };
  graniteSrc = fetchFromGitHub {
    owner = "Themaister";
    repo = "Granite";
    rev = "fb178c8080d163419e8d20f10715c61c53c1ec9b";
    sha256 = "05mdwf79vjkg956j1wrykrb3gz01p0ry2ddf01vzlxrgr0kg7hzj";
  };
  volkSrc = fetchFromGitHub {
    owner = "zeux";
    repo = "volk";
    rev = "47cddf7ed97b94118a08aacb548a411188e016cc";
    sha256 = "1v0jxbvy9lb6dz2r8gr45pymc1hkwsa8rx4r368ks817l2iz9b73";
  };
  vulkanHeadersSrc = fetchFromGitHub {
    owner = "KhronosGroup";
    repo = "Vulkan-Headers";
    rev = "6802bb4733b63ed5efd3adb308a6c885ef180ea1";
    sha256 = "1rpgygg4ss7dhcximxnwj94qwkyj54s5nba39ya4g8dmqna83x0k";
  };
in
stdenv.mkDerivation {
  pname = "pyrowave";
  version = "1.0.0";

  src = pyrowaveSrc;

  nativeBuildInputs = [
    cmake
    ninja
  ];

  # One heavy translation unit at a time. device.cpp is large.
  enableParallelBuilding = false;

  preConfigure = ''
    rm -rf Granite
    cp -a ${graniteSrc} Granite
    chmod -R u+w Granite
    rm -rf Granite/third_party/volk Granite/third_party/khronos/vulkan-headers
    mkdir -p Granite/third_party/khronos
    cp -a ${volkSrc} Granite/third_party/volk
    cp -a ${vulkanHeadersSrc} Granite/third_party/khronos/vulkan-headers
    chmod -R u+w Granite/third_party/volk Granite/third_party/khronos/vulkan-headers
    substituteInPlace Granite/CMakeLists.txt \
      --replace-fail 'add_subdirectory(compiler)' 'if (GRANITE_VULKAN_SHADER_MANAGER_RUNTIME_COMPILER)
    add_subdirectory(compiler)
endif()'
  '';

  cmakeFlags = [
    "-DCMAKE_BUILD_TYPE=Release"
    "-DPYROWAVE_DEVEL=OFF"
    "-DPYROWAVE_UTILS=OFF"
  ];

  postInstall = ''
    libdir="$out/lib"
    if [ ! -e "$libdir/libpyrowave-shared.so" ] && [ ! -e "$libdir/libpyrowave-shared.so.1" ]; then
      libdir="$out/lib64"
    fi
    test -e "$libdir/libpyrowave-shared.so" || test -e "$libdir/libpyrowave-shared.so.1"
    install -Dm644 "$src/LICENSE" "$out/share/pyrowave/LICENSE-pyrowave.txt"
    cp "$out/share/pyrowave/LICENSE-pyrowave.txt" "$out/lib/LICENSE-pyrowave.txt"
  '';

  meta = {
    description = "PyroWave shared library (Vulkan compute intra codec)";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
