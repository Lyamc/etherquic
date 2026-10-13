# Dev shell so `cargo build` can link libpcap on NixOS.
#
#   nix-shell nix/shell.nix
{
  pkgs ? import <nixpkgs> { },
}:

let
  libs = [ pkgs.libpcap ];
in
pkgs.mkShell {
  name = "etherquic";

  nativeBuildInputs = [
    pkgs.pkg-config
  ];

  buildInputs = libs;

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;

  shellHook = ''
    echo "etherquic nix-shell (libpcap ${pkgs.libpcap.version})"
    echo "  cargo test"
    echo "  cargo build --release --bins"
    echo "  cargo run --manifest-path video/Cargo.toml --bin etherquic-video-recv -- --help"
    echo "  cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-recv -- --help"
  '';
}
