# ffmpeg with libsvtjpegxs so JPEG XS encode/decode is available.
{
  pkgs ? import <nixpkgs> { },
}:
let
  svtJpegxs = pkgs.callPackage ./svt-jpegxs.nix { };
in
pkgs.ffmpeg.overrideAttrs (old: {
  buildInputs = (old.buildInputs or [ ]) ++ [ svtJpegxs ];
  configureFlags = (old.configureFlags or [ ]) ++ [ "--enable-libsvtjpegxs" ];
})
