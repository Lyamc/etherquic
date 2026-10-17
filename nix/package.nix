# Build the etherquic library and demo from this repo.
#
#   nix-build nix
#   nix-build -E 'with import <nixpkgs> {}; callPackage ./nix/video.nix {}'
#   nix-build -E 'with import <nixpkgs> {}; callPackage ./nix/audio.nix {}'
#   nix-build -E 'with import <nixpkgs> {}; callPackage ./nix/input.nix {}'
#
# Video is service 4433, audio is 4434, input is 4435. Each listener needs the
# demo certificate; its wrapper sets ETHERQUIC_CERTS to the copy in the store.
{
  lib,
  rustPlatform,
  pkg-config,
  libpcap,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "etherquic";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;

  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        base = baseNameOf path;
        p = toString path;
      in
      base != "target"
      && !(lib.hasInfix "/target/" p)
      && base != ".git"
      && !(lib.hasInfix "/.git/" p);
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [ libpcap ];

  # In-memory fabric tests. They do not open a NIC.
  doCheck = true;

  postInstall = ''
    mkdir -p "$out/share/etherquic"
    cp -r "$src/certs" "$out/share/etherquic/certs"
    wrapProgram "$out/bin/etherquic-demo" \
      --set ETHERQUIC_CERTS "$out/share/etherquic/certs"
  '';

  meta = {
    description = "QUIC over Ethernet (libpcap on Linux)";
    mainProgram = "etherquic-demo";
    platforms = lib.platforms.linux;
  };
}
