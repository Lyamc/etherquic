# Tools that build and read the Windows and Android application packages.
#
#   nix-shell nix/installers.nix --run 'cargo test --manifest-path packaging/Cargo.toml -- --nocapture'
{
  pkgs ? import <nixpkgs> { },
}:

pkgs.mkShell {
  nativeBuildInputs = [
    pkgs.msitools
    pkgs.aapt
    pkgs.apksigner
    pkgs.android-tools
  ];
}
