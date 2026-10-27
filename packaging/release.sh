#!/bin/sh
# Cut a GitHub release from this tree.
#
#   packaging/release.sh [--notes FILE] [--prerelease] [--linux-only]
#
# Linux is built here (nix-shell). Windows MSI and macOS dmg/pkg are built on
# AN515 and the Mac through the 192.168.0.24 hop, then copied back. Tag and
# GitHub release use the root Cargo package version unless VERSION= is set.
#
# Does not bump Cargo.toml. Does not retag an existing tag.
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
notes=
prerelease=0
linux_only=0
while [ $# -gt 0 ]; do
    case $1 in
        --notes)
            notes=$2
            shift 2
            ;;
        --prerelease)
            prerelease=1
            shift
            ;;
        --linux-only)
            linux_only=1
            shift
            ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "usage: packaging/release.sh [--notes FILE] [--prerelease] [--linux-only]" >&2
            exit 2
            ;;
    esac
done

version=${VERSION:-$(awk '
  /^\[package\]/ { inpkg = 1; next }
  inpkg && /^\[/ { exit }
  inpkg && /^version = "/ {
    sub(/^version = "/, "")
    sub(/".*/, "")
    print
    exit
  }
' "$root/Cargo.toml")}
tag=v$version
dist=$root/dist
mkdir -p "$dist"
gh=${GH:-gh}
if ! command -v "$gh" >/dev/null 2>&1; then
    for p in /nix/store/*/bin/gh; do
        if [ -x "$p" ]; then
            gh=$p
            break
        fi
    done
fi

hop=${HOP:-lyam@192.168.0.24}
win=${WIN:-lyam@192.168.0.58}
mac=${MAC:-claire@192.168.0.53}
# Remote checkouts. Do not write over AN515 *.bak* files.
win_dir=${WIN_DIR:-C:/Users/Lyam/src/etherquic-0.1.6}
win_npcap=${WIN_NPCAP:-C:/Build/o3-borderless/third_party/npcap/lib}
mac_dir=${MAC_DIR:-/Users/claire/Build/etherquic}

echo "version $version tag $tag"

"$here/linux/make-tarball.sh" "$dist"
linux=$dist/etherquic-$version-x86_64-linux.tar.gz

win_msi=
mac_dmg=
mac_pkg=
src_tar=$dist/etherquic-$version-src.tar.gz
if [ "$linux_only" -eq 0 ]; then
    echo "pack source $src_tar"
    tar -C "$root" \
        --exclude=./target --exclude=./dist --exclude=./.git \
        --exclude='*.bak' --exclude='*.bak*' \
        --exclude=./packaging/android/jni \
        -czf "$src_tar" .

    echo "sync tree to Windows via $hop -> $win"
    scp -o BatchMode=yes -o ProxyJump="$hop" \
        "$src_tar" "$win:C:/Users/Public/etherquic-$version-src.tar.gz"
    ssh -o BatchMode=yes -J "$hop" "$win" \
        "cd /d ${win_dir} && tar -xzf C:/Users/Public/etherquic-$version-src.tar.gz"
    ssh -o BatchMode=yes -J "$hop" "$win" \
        "powershell -NoProfile -File ${win_dir}/packaging/windows/remote-msi.ps1 -Version $version -Npcap ${win_npcap}"
    scp -o BatchMode=yes -o ProxyJump="$hop" \
        "$win:${win_dir}/dist/etherquic-$version-windows-x64.msi" \
        "$dist/"
    win_msi=$dist/etherquic-$version-windows-x64.msi

    echo "sync tree to macOS via $hop -> $mac"
    if scp -o BatchMode=yes -o ConnectTimeout=25 -o ProxyJump="$hop" \
        "$src_tar" "$mac:/tmp/etherquic-$version-src.tar.gz"
    then
        if ssh -o BatchMode=yes -o ConnectTimeout=25 -o ServerAliveInterval=30 \
            -J "$hop" "$mac" \
            "mkdir -p $mac_dir && tar -C $mac_dir -xzf /tmp/etherquic-$version-src.tar.gz && VERSION=$version $mac_dir/packaging/macos/make-pkg.sh"
        then
            scp -o BatchMode=yes -o ConnectTimeout=25 -o ProxyJump="$hop" \
                "$mac:$mac_dir/dist/macos/etherquic-$version-macos-arm64.dmg" \
                "$mac:$mac_dir/dist/macos/etherquic-$version-macos-arm64.pkg" \
                "$dist/" \
                && mac_dmg=$dist/etherquic-$version-macos-arm64.dmg \
                && mac_pkg=$dist/etherquic-$version-macos-arm64.pkg \
                || echo "macOS package copy failed; continuing without it" >&2
        else
            echo "macOS build failed; continuing without it" >&2
        fi
    else
        echo "macOS build skipped (host did not answer)" >&2
    fi
fi

assets=$linux
[ -n "$win_msi" ] && assets="$assets $win_msi"
[ -n "$mac_dmg" ] && assets="$assets $mac_dmg"
[ -n "$mac_pkg" ] && assets="$assets $mac_pkg"

if git -C "$root" rev-parse "$tag" >/dev/null 2>&1; then
    echo "tag $tag already exists; not retagging" >&2
    exit 1
fi

if [ -z "$notes" ]; then
    notes=$(mktemp)
    trap 'rm -f "$notes"' EXIT
    {
        echo "etherquic $version"
        echo
        echo "Installers: Linux tar.gz, Windows MSI, macOS pkg and dmg."
        echo "Settings can check GitHub tags and install the matching package. Beta releases include pre-release tags."
    } >"$notes"
fi

git -C "$root" tag -a "$tag" -m "Release $version"
git -C "$root" push origin HEAD
git -C "$root" push origin "$tag"

pre=
[ "$prerelease" -eq 1 ] && pre=--prerelease
# shellcheck disable=SC2086
"$gh" release create "$tag" $pre --title "$version" --notes-file "$notes" $assets
echo "published $tag"
