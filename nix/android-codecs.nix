# arm64-v8a codec libraries for the EtherQuic Play APK, built with the
# Android NDK (pkgsCross.aarch64-android's compiler-rt cannot find pthread.h).
#
# Hosts can send PyroWave, Opus, H.264, H.265, AV1, OpenAPV, JPEG XS, MJPEG,
# and MJ2K. MJPEG and MJ2K decode in Rust inside libetherquic_android.so.
# The rest need these jniLibs, packed as unversioned lib*.so:
#
#   libopus.so              libopus
#   libpyrowave-shared.so   Vulkan PyroWave
#   libffmpeg.so            ffmpeg with dav1d, liboapv, libsvtjpegxs
#
#   nix-build nix/android-codecs.nix
#   sh packaging/android/codecs.sh packaging/android/jni/arm64-v8a
{
  pkgs ? import <nixpkgs> { },
}:
let
  inherit (pkgs) fetchFromGitHub;
  ndkBundle = pkgs.androidenv.androidPkgs.ndk-bundle;
  api = "21";
  abi = "arm64-v8a";
  llvmPrebuilt = "linux-x86_64";
  osRelease = pkgs.writeText "os-release" "ID=Android";

  withNdk =
    {
      pname,
      version,
      src,
      nativeBuildInputs ? [ ],
      buildInputs ? [ ],
      configurePhase,
      extra ? { },
    }:
    pkgs.stdenvNoCC.mkDerivation (
      {
        inherit
          pname
          version
          src
          buildInputs
          ;
        nativeBuildInputs = [
          pkgs.cmake
          pkgs.ninja
          pkgs.pkg-config
          pkgs.python3
          pkgs.which
        ]
        ++ nativeBuildInputs;
        inherit ndkBundle;
        ANDROID_NDK_ROOT = "${ndkBundle}/libexec/android-sdk/ndk";
        NIX_ENFORCE_PURITY = 0;
        hardeningDisable = [ "all" ];
        dontAddDisableDepTrack = true;
        preConfigure = ''
          export NDK=$(echo "$ndkBundle"/libexec/android-sdk/ndk/*)
          export LLVM="$NDK/toolchains/llvm/prebuilt/${llvmPrebuilt}/bin"
          export PATH="$LLVM:$PATH"
          export CC="$LLVM/aarch64-linux-android${api}-clang"
          export CXX="$LLVM/aarch64-linux-android${api}-clang++"
          export AR="$LLVM/llvm-ar"
          export RANLIB="$LLVM/llvm-ranlib"
          export STRIP="$LLVM/llvm-strip"
          export NM="$LLVM/llvm-nm"
          export LD="$CC"
        '';
        inherit configurePhase;
        enableParallelBuilding = true;
      }
      // extra
    );

  mesonCross = ''
    cat > android-cross.ini <<EOF
    [binaries]
    c = '$CC'
    cpp = '$CXX'
    ar = '$AR'
    strip = '$STRIP'
    pkg-config = 'pkg-config'
    [host_machine]
    system = 'android'
    cpu_family = 'aarch64'
    cpu = 'aarch64'
    endian = 'little'
    EOF
  '';

  opus = withNdk {
    pname = "libopus-android";
    inherit (pkgs.libopus) version src;
    nativeBuildInputs = [ pkgs.meson ];
    configurePhase = ''
      runHook preConfigure
      patchShebangs meson/
      ${mesonCross}
      meson setup build --prefix="$out" --buildtype=release \
        --cross-file=android-cross.ini \
        -Ddefault_library=shared \
        -Ddocs=disabled \
        -Dtests=disabled
      runHook postConfigure
    '';
    extra.buildPhase = ''
      meson compile -C build -j ''${NIX_BUILD_CORES:-1}
    '';
    extra.installPhase = ''
      meson install -C build
    '';
  };

  dav1d = withNdk {
    pname = "dav1d-android";
    inherit (pkgs.dav1d) version src;
    nativeBuildInputs = [
      pkgs.meson
      pkgs.nasm
    ];
    configurePhase = ''
      runHook preConfigure
      ${mesonCross}
      meson setup build --prefix="$out" --buildtype=release \
        --cross-file=android-cross.ini \
        -Ddefault_library=static \
        -Denable_tools=false \
        -Denable_tests=false \
        -Denable_examples=false
      runHook postConfigure
    '';
    extra.buildPhase = ''
      meson compile -C build -j ''${NIX_BUILD_CORES:-1}
    '';
    extra.installPhase = ''
      meson install -C build
    '';
  };

  openapv = withNdk {
    pname = "openapv-android";
    inherit (pkgs.openapv) version src;
    extra.postPatch = ''
      substituteInPlace CMakeLists.txt \
        --replace-fail "/etc/os-release" "${osRelease}"
    '';
    configurePhase = ''
      runHook preConfigure
      cmake -S . -B build -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI=${abi} \
        -DANDROID_PLATFORM=android-${api} \
        -DANDROID_STL=c++_static \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$out" \
        -DBUILD_SHARED_LIBS=OFF
      runHook postConfigure
    '';
    extra.buildPhase = ''
      cmake --build build -j ''${NIX_BUILD_CORES:-1}
    '';
    extra.installPhase = ''
      cmake --install build
    '';
  };

  svtJpegxs = withNdk {
    pname = "svt-jpegxs-android";
    inherit (pkgs.callPackage ./svt-jpegxs.nix { }) version src;
    nativeBuildInputs = [ pkgs.nasm ];
    configurePhase = ''
      runHook preConfigure
      cmake -S . -B build -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI=${abi} \
        -DANDROID_PLATFORM=android-${api} \
        -DANDROID_STL=c++_static \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$out" \
        -DBUILD_SHARED_LIBS=OFF \
        -DBUILD_APPS=OFF
      runHook postConfigure
    '';
    extra.buildPhase = ''
      cmake --build build -j ''${NIX_BUILD_CORES:-1}
    '';
    extra.installPhase = ''
      cmake --install build
    '';
  };

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

  pyrowave = withNdk {
    pname = "pyrowave-android";
    version = "1.0.0";
    src = pyrowaveSrc;
    configurePhase = ''
      runHook preConfigure
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
      cmake -S . -B build -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI=${abi} \
        -DANDROID_PLATFORM=android-${api} \
        -DANDROID_STL=c++_static \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$out" \
        -DPYROWAVE_DEVEL=OFF \
        -DPYROWAVE_UTILS=OFF
      runHook postConfigure
    '';
    extra.buildPhase = ''
      cmake --build build -j 1
    '';
    extra.installPhase = ''
      cmake --install build
    '';
    extra.enableParallelBuilding = false;
  };

  ffmpeg = withNdk {
    pname = "ffmpeg-android";
    inherit (pkgs.ffmpeg) version src;
    buildInputs = [
      dav1d
      openapv
      svtJpegxs
    ];
    nativeBuildInputs = [
      pkgs.perl
      pkgs.stdenv.cc
    ];
    configurePhase = ''
      runHook preConfigure
      mkdir -p pc
      cp ${dav1d}/lib/pkgconfig/*.pc pc/
      cp ${openapv}/lib/pkgconfig/*.pc pc/
      cp ${svtJpegxs}/lib/pkgconfig/*.pc pc/
      # Android libc already has pthreads; a separate -lpthread fails the
      # ffmpeg pkg-config compile test.
      sed -i 's/-lpthread//g; s/-pthread//g' pc/*.pc
      export PKG_CONFIG_PATH="$PWD/pc"
      export PKG_CONFIG_LIBDIR="$PWD/pc"
      unset PKG_CONFIG_SYSROOT_DIR
      ./configure \
        --pkg-config=pkg-config \
        --prefix="$out" \
        --enable-cross-compile \
        --arch=aarch64 \
        --cpu=armv8-a \
        --target-os=android \
        --cc="$CC" \
        --cxx="$CXX" \
        --host-cc=cc \
        --ar="$AR" \
        --ranlib="$RANLIB" \
        --strip="$STRIP" \
        --nm="$NM" \
        --enable-static \
        --disable-shared \
        --enable-pic \
        --disable-everything \
        --disable-doc \
        --disable-htmlpages \
        --disable-manpages \
        --disable-podpages \
        --disable-txtpages \
        --disable-ffplay \
        --disable-ffprobe \
        --disable-autodetect \
        --disable-network \
        --disable-debug \
        --enable-avcodec \
        --enable-avformat \
        --enable-avutil \
        --enable-swscale \
        --enable-swresample \
        --enable-ffmpeg \
        --enable-libdav1d \
        --enable-liboapv \
        --enable-libsvtjpegxs \
        --enable-decoder=h264,hevc,av1,libdav1d,apv,liboapv,libsvtjpegxs \
        --enable-parser=h264,hevc,av1,apv \
        --enable-demuxer=nut,apv,h264,hevc \
        --enable-muxer=rawvideo,nut,apv \
        --enable-encoder=rawvideo \
        --enable-protocol=file,pipe,fd \
        --enable-filter=scale,format \
        --extra-cflags="-I${dav1d}/include -I${openapv}/include -I${openapv}/include/oapv -I${svtJpegxs}/include -I${svtJpegxs}/include/svt-jpegxs" \
        --extra-ldflags="-L${dav1d}/lib -L${openapv}/lib -L${svtJpegxs}/lib" \
        --extra-libs="-lm"
      runHook postConfigure
    '';
    extra.buildPhase = ''
      make -j''${NIX_BUILD_CORES:-1}
    '';
    extra.installPhase = ''
      make install
    '';
  };
in
pkgs.runCommand "etherquic-android-jni" {
  nativeBuildInputs = [ pkgs.file ];
  inherit opus pyrowave ffmpeg dav1d openapv svtJpegxs;
  passthru = {
    inherit
      opus
      pyrowave
      ffmpeg
      dav1d
      openapv
      svtJpegxs
      ;
  };
} ''
  jni="$out/lib/arm64-v8a"
  mkdir -p "$jni"

  stage_so() {
    local src="$1"
    local dest="$2"
    if [ ! -e "$src" ]; then
      echo "missing $dest ($src)" >&2
      return 1
    fi
    cp -L "$src" "$jni/$dest"
    chmod u+w "$jni/$dest"
  }

  opus_src=
  for cand in "$opus/lib/libopus.so" "$opus/lib/libopus.so.0" "$opus/lib/libopus.so.0.0.0"; do
    if [ -e "$cand" ]; then
      opus_src=$cand
      break
    fi
  done
  stage_so "$opus_src" libopus.so

  pyro_src=
  for cand in \
    "$pyrowave/lib/libpyrowave-shared.so" \
    "$pyrowave/lib/libpyrowave-shared.so.1" \
    "$pyrowave/lib64/libpyrowave-shared.so" \
    "$pyrowave/lib64/libpyrowave-shared.so.1"
  do
    if [ -e "$cand" ]; then
      pyro_src=$cand
      break
    fi
  done
  stage_so "$pyro_src" libpyrowave-shared.so

  if [ -x "$ffmpeg/bin/ffmpeg" ]; then
    stage_so "$ffmpeg/bin/ffmpeg" libffmpeg.so
  else
    echo "ffmpeg binary missing" >&2
    ls -laR "$ffmpeg" >&2 || true
    exit 1
  fi

  file "$jni"/* || true
  ls -l "$jni"
''
