# etherquic-audio-recv (service 4434) and etherquic-audio-send (client).
{
  lib,
  rustPlatform,
  pkg-config,
  libpcap,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "etherquic-audio";
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

  cargoLock.lockFile = ../audio/Cargo.lock;
  cargoRoot = "audio";
  buildAndTestSubdir = "audio";

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [ libpcap ];

  doCheck = true;

  postInstall = ''
    mkdir -p "$out/share/etherquic"
    cp -r "$src/certs" "$out/share/etherquic/certs"
    wrapProgram "$out/bin/etherquic-audio-recv" \
      --set ETHERQUIC_CERTS "$out/share/etherquic/certs"
  '';

  meta = {
    description = "Opus audio over etherquic";
    mainProgram = "etherquic-audio-recv";
    platforms = lib.platforms.linux;
  };
}
