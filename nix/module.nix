# Install the etherquic listeners and clients.
#
#   imports = [ /home/lyam/Build/etherquic/nix/module.nix ];
#   services.etherquic.enable = true;
#
# This does not start a daemon. The listeners are one-shot programs:
# etherquic-video-recv (service 4433), etherquic-audio-recv (4434), and
# etherquic-input-recv (4435). The matching *-send binaries are the clients.
# Opening a NIC needs CAP_NET_RAW (run them as root, or grant the capability).
# Video, audio, and input are separate packages. `package` is the library and demo.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.etherquic;
in
{
  options.services.etherquic = {
    enable = lib.mkEnableOption "etherquic L2 listeners and clients";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      description = "Library and in-memory demo.";
    };

    video = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./video.nix { };
      description = "Video listener and client.";
    };

    audio = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./audio.nix { };
      description = "Audio listener and client.";
    };

    input = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./input.nix { };
      description = "Input listener and client.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      cfg.package
      cfg.video
      cfg.audio
      cfg.input
    ];
  };
}
