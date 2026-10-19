# SVT-JPEG-XS (ISO/IEC 21122). ffmpeg 9 needs SvtJpegxs >= 0.10.0.
# The GitHub tag is still v0.9.0; master reports version 0.10.0.
{
  lib,
  stdenv,
  fetchFromGitHub,
  cmake,
  nasm,
  pkg-config,
}:

stdenv.mkDerivation rec {
  pname = "svt-jpegxs";
  version = "0.10.0";

  src = fetchFromGitHub {
    owner = "OpenVisualCloud";
    repo = "SVT-JPEG-XS";
    rev = "f4720203534d6f5ea90f828a58814af5597461d7";
    hash = "sha256-mxiR7cf2e+8xGI4Whib3bc8Mwg4DzLCLwgKo2Dz70CM=";
  };

  nativeBuildInputs = [
    cmake
    nasm
    pkg-config
  ];

  cmakeFlags = [
    "-DCMAKE_BUILD_TYPE=Release"
  ];

  meta = {
    description = "SVT JPEG XS encoder and decoder";
    homepage = "https://github.com/OpenVisualCloud/SVT-JPEG-XS";
    license = lib.licenses.bsd2Patent;
    platforms = lib.platforms.unix;
  };
}
