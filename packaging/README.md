# Packaging

Installers for a GitHub tagged release. The GUI **Check for updates** action
reads those tags (`Lyamc/etherquic`) and downloads the asset for this computer.

| File | Command |
| --- | --- |
| Linux `etherquic-<ver>-x86_64-linux.tar.gz` | `nix-shell nix/shell.nix --run packaging/linux/make-tarball.sh` |
| Windows `etherquic-<ver>-windows-x64.msi` | `powershell -File packaging\windows\build-msi.ps1` |
| macOS `.app` / `.dmg` | `packaging/macos/make-app.sh` |
| macOS `.pkg` | `packaging/macos/make-pkg.sh` |
| All of the above plus `gh release create` | `packaging/release.sh` |

`packaging/release.sh` builds Linux here, then copies the tree through
`lyam@192.168.0.24` to AN515 (`lyam@192.168.0.58`, checkout
`C:/Users/Lyam/src/etherquic-0.1.6`, Npcap `C:/Build/o3-borderless/third_party/npcap/lib`)
and the Mac (`claire@192.168.0.53`). It does not overwrite `*.bak*` on AN515, does not
retag, and does not pass `--nic`. `VERSION=` overrides the root Cargo version.

```bash
# After bumping version = in the root Cargo.toml and committing:
packaging/release.sh --notes /tmp/notes.md
packaging/release.sh --linux-only --prerelease
```
