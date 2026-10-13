# Smoke-check: nix-instantiate --eval --strict nix/eval-smoke.nix
{ pkgs ? import <nixpkgs> { } }:
let
  inherit (pkgs) lib;
  eval = lib.evalModules {
    modules = [
      ./module.nix
      {
        _module.check = false;
        services.etherquic.enable = true;
      }
    ];
    specialArgs = { inherit pkgs; };
  };
in
{
  enable = eval.config.services.etherquic.enable;
  core = eval.config.services.etherquic.package.pname;
  video = eval.config.services.etherquic.video.pname;
  audio = eval.config.services.etherquic.audio.pname;
  input = eval.config.services.etherquic.input.pname;
}
