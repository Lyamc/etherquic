# etherquic-video-recv (service 4433) and etherquic-video-send (client).
{
  lib,
  rustPlatform,
  pkg-config,
  libpcap,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "etherquic-video";
  version = "0.1.0";

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
  cargoBuildFlags = [ "-p" "etherquic-video" ];
  cargoTestFlags = [ "-p" "etherquic-video" ];

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [ libpcap ];

  doCheck = true;

  postInstall = ''
    mkdir -p "$out/share/etherquic"
    cp -r "$src/certs" "$out/share/etherquic/certs"
    wrapProgram "$out/bin/etherquic-video-recv" \
      --set ETHERQUIC_CERTS "$out/share/etherquic/certs"
  '';

  meta = {
    description = "Copy a file over etherquic on one QUIC stream";
    mainProgram = "etherquic-video-recv";
    platforms = lib.platforms.linux;
  };
}
