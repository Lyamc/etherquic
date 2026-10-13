# etherquic-input-recv (service 4435) and etherquic-input-send (client).
{
  lib,
  rustPlatform,
  pkg-config,
  libpcap,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "etherquic-input";
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

  cargoLock.lockFile = ../input/Cargo.lock;
  cargoRoot = "input";
  buildAndTestSubdir = "input";

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [ libpcap ];

  doCheck = true;

  postInstall = ''
    mkdir -p "$out/share/etherquic"
    cp -r "$src/certs" "$out/share/etherquic/certs"
    wrapProgram "$out/bin/etherquic-input-recv" \
      --set ETHERQUIC_CERTS "$out/share/etherquic/certs"
  '';

  meta = {
    description = "Keyboard, mouse, and gamepad events over etherquic";
    mainProgram = "etherquic-input-recv";
    platforms = lib.platforms.linux;
  };
}
