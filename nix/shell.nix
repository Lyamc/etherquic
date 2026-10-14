# Dev shell so `cargo build` can link libpcap, and etherquic-play can
# link the desktop capture, audio, and window libraries on NixOS.
# ffmpeg and gst-inspect-1.0 are on PATH. libopus and libpyrowave-shared
# are on the library path. That is the order the Opus probe uses.
#
#   nix-shell nix/shell.nix
{
  pkgs ? import <nixpkgs> { },
}:

let
  pyrowave = pkgs.callPackage ./pyrowave.nix { };
  libs = with pkgs; [
    libpcap
    alsa-lib
    pipewire
    wayland
    libxkbcommon
    libxcb
    libdrm
    libgbm
    libGL
    libglvnd
    vulkan-loader
    libopus
    pyrowave
    libX11
    libXcursor
    libXrandr
    libXi
    udev
  ];
in
pkgs.mkShell {
  name = "etherquic";

  nativeBuildInputs = [
    pkgs.pkg-config
    pkgs.rustPlatform.bindgenHook
    pkgs.ffmpeg
    pkgs.gst_all_1.gstreamer
  ];

  buildInputs = libs;

  # rustc (rust-lld) searches LIBRARY_PATH. LD_LIBRARY_PATH is only for running
  # the binary. NIX_LDFLAGS is for the Nix gcc wrapper, which this rustc does not use.
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;
  LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;

  shellHook = ''
    # The host GPU driver lives outside the nix store search path. GPUI loads
    # libvulkan.so.1 from this shell, then that loader opens the NVIDIA ICD.
    if [ -d /run/opengl-driver/lib ]; then
      export LD_LIBRARY_PATH="/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:}$LD_LIBRARY_PATH"
    fi
    if [ -z "''${VK_DRIVER_FILES:-}" ] && [ -f /run/opengl-driver/share/vulkan/icd.d/nvidia_icd.json ]; then
      export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/nvidia_icd.json
    fi
    echo "etherquic nix-shell (libpcap ${pkgs.libpcap.version}, ffmpeg ${pkgs.ffmpeg.version}, gstreamer ${pkgs.gst_all_1.gstreamer.version}, libopus ${pkgs.libopus.version}, pyrowave ${pyrowave.version})"
    echo "  cargo test"
    echo "  cargo build --release --bins"
    echo "  cargo run --manifest-path video/Cargo.toml --bin etherquic-video-recv -- --help"
    echo "  cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-recv -- --help"
    echo "  cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- --help"
  '';
}
