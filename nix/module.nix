# NixOS module for the etherquic desktop host.
#
# In configuration.nix:
#
#   etherquic = pkgs.callPackage ./etherquic.nix { };
#   imports = [ /path/to/etherquic/nix/module.nix ];
#
#   services.etherquic.enable = true;
#   services.etherquic.user = "lyam";
#   services.etherquic.nic = "enp4s0";
#   services.etherquic.package = etherquic;
#
# The unit is a system service. It runs `package`'s release `etherquic`
# (`etherquic host`), not a cargo build. AmbientCapabilities carry
# CAP_NET_RAW and CAP_NET_ADMIN. The release window answers discovery on
# UDP 4440 while that process is up.

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.etherquic;
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    optionals
    types
    ;

  gstPlugins = with pkgs.gst_all_1; [
    gst-plugins-base
    gst-plugins-good
    gst-plugins-bad
    gst-plugins-ugly
    gst-libav
  ];
  runtimeLibs = with pkgs; [
    libpcap
    alsa-lib
    libxcb
    libxkbcommon
    libgbm
    udev
    vulkan-loader
    wayland
    libglvnd
    libGL
    fontconfig
    freetype
    libX11
    libXcursor
    libXrandr
    libXi
    libopus
    stdenv.cc.cc.lib
  ];

  exe = if cfg.bin != null then toString cfg.bin else "${cfg.package}/bin/etherquic";

  hostArgs = lib.concatStringsSep " " (
    [ "host" ]
    ++ optionals cfg.tray [ "--tray" ]
    ++ optionals (cfg.nic != null) [
      "--nic"
      (lib.escapeShellArg cfg.nic)
    ]
    ++ cfg.extraArgs
  );

  # nixpkgs ffmpeg has no libsvtjpegxs, so a JPEG XS offer falls through to
  # MJPEG. The release wrapper also puts that ffmpeg first on PATH. The app
  # reads ETHERQUIC_FFMPEG before PATH.
  ffmpeg = import ./ffmpeg-jpegxs.nix { inherit pkgs; };
  mediaPath = lib.makeBinPath (
    [
      ffmpeg
      pkgs.gst_all_1.gstreamer
      pkgs.libjxl
    ]
    ++ gstPlugins
  );
  libraryPath = lib.concatStringsSep ":" [
    (lib.makeLibraryPath runtimeLibs)
    "/run/opengl-driver/lib"
    "${cfg.package}/bin"
  ];
  gstPath = lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins;
  start = pkgs.writeShellScript "etherquic-host" ''
    runtime="/run/user/$(id -u)"
    export XDG_RUNTIME_DIR="$runtime"
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/bus"
    # Xwayland's cookie name changes every login. It lives in the runtime dir.
    auth=$(ls -1 "$runtime"/xauth_* 2>/dev/null | head -n 1 || true)
    if [ -n "$auth" ]; then
      export XAUTHORITY="$auth"
    fi
    export PATH=${mediaPath}:''${PATH}
    export ETHERQUIC_FFMPEG=${ffmpeg}/bin/ffmpeg
    export LD_LIBRARY_PATH=${libraryPath}
    export GST_PLUGIN_SYSTEM_PATH_1_0=${gstPath}
    export ETHERQUIC_CERTS=${cfg.package}/share/etherquic/certs
    export ETHERQUIC_CJXL=${pkgs.libjxl}/bin/cjxl
    export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/nvidia_icd.json
    exec ${exe} ${hostArgs}
  '';
in
{
  options.services.etherquic = {
    enable = mkEnableOption "etherquic desktop host";

    package = mkOption {
      type = types.package;
      description = ''
        Release package. The service runs `bin/etherquic` from it and reads
        `share/etherquic/certs` and `bin/libpyrowave-shared.so.1`.
      '';
    };

    bin = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = ''
        If set, systemd runs this program instead of `package`'s etherquic.
        Leave unset so the service uses the downloaded release.
      '';
    };

    tray = mkOption {
      type = types.bool;
      default = true;
      description = "Pass --tray so the host stays in the notification area.";
    };

    user = mkOption {
      type = types.str;
      example = "lyam";
      description = "Account that owns the desktop session the host captures.";
    };

    nic = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "enp4s0";
      description = "Interface passed to `etherquic host --nic`.";
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra arguments after `host`.";
    };
  };

  config = mkIf cfg.enable {
    users.users.${cfg.user}.extraGroups = [
      "input"
      "audio"
      "video"
    ];

    systemd.services.etherquic = {
      description = "etherquic desktop host";
      after = [
        "network-online.target"
        "systemd-user-sessions.service"
      ];
      wants = [ "network-online.target" ];
      wantedBy = [ "graphical.target" ];
      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = "users";
        SupplementaryGroups = "input audio video";
        ExecStartPre = "${pkgs.bash}/bin/bash -c 'runtime=/run/user/$(id -u); for i in $(seq 1 90); do test -S \"$runtime/wayland-0\" && exit 0; sleep 1; done; echo etherquic: no Wayland socket at \"$runtime/wayland-0\"; exit 1'";
        ExecStart = start;
        # A closed window exits 0. on-failure leaves the machine undiscoverable.
        Restart = "always";
        RestartSec = 5;
        AmbientCapabilities = "CAP_NET_RAW CAP_NET_ADMIN";
        CapabilityBoundingSet = "CAP_NET_RAW CAP_NET_ADMIN";
        NoNewPrivileges = false;
        KillMode = "mixed";
        TimeoutStopSec = 10;
      };
      environment = {
        WAYLAND_DISPLAY = "wayland-0";
        DISPLAY = ":0";
        XDG_SESSION_TYPE = "wayland";
      };
    };
  };
}
