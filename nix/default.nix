#   nix-build nix
{ pkgs ? import <nixpkgs> { } }:
pkgs.callPackage ./package.nix { }
