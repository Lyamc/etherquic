# Workspace and the running program

`plan.md` at the repo root stays the note on Quinn and Ethernet. This file is the crate layout and the docs that have to match the program that actually runs.

## 1. One Cargo workspace

The repo root is a Cargo workspace and still the `etherquic` package. Members:

- `.` (`etherquic`)
- `ui`, `play`, `host`, `setup`, `media`
- `audio`, `video`, `input`, `container`, `packaging`, `android`

`default-members` is `.`, so `cargo test` and `nix/package.nix` still mean the root package. `cargo test --workspace` is the whole tree. `cargo build -p etherquic-ui` (and the other member names) builds that crate into the shared `target/`.

The root package is this manifest. It is not repeated in `members`. Listing `.` there marks every path under the repo as an explicit member, and `exclude` can no longer keep `vendor/quinn` out.

`vendor/quinn` stays its own workspace. It is excluded from this one, along with the other vendored crates that are path dependencies (`rustls-rustcrypto`, `softbuffer`, `xattr`). A build from `vendor/quinn` still uses Quinn's lock and members.

`[patch.crates-io]` lives only on the workspace root:

- `gpui_http_client` → `../../interfaces/gpui_http_client`
- `xattr` → `vendor/xattr`

`ui/Cargo.toml` no longer carries a `[patch]` table. Paths inside `ui/Cargo.toml` stay relative to that file.

One `Cargo.lock` at the repo root is the resolution. The per-crate lockfiles are unused once the members join, and they are removed so a later build does not pick up a stale lock. `vendor/quinn/Cargo.lock` stays. Existing per-crate `target/` trees stay on disk. New builds write `target/` at the repo root, including a build started from `ui/`.

`[profile.release] debug = true` was already on the root package. Cargo reads profiles from the workspace root, so that setting now covers every member.

`nix/video.nix`, `nix/audio.nix`, and `nix/input.nix` read the root lock and build with `-p`. A nix sandbox that contains only this repo still cannot see `interfaces/gpui` once `ui` is a member, because Cargo loads every member before it builds one.

## 2. Docs match the running program

Host and Connect in the window call the play library inside `etherquic.exe`. They do not launch the `etherquic-play` binary.

The live picture negotiates a codec. The preference order is AV1 hardware, H.265 hardware, H.264 hardware, PyroWave, MJPEG, H.264 software, then CPU PyroWave. The paths a desktop actually settles on are PyroWave, MJPEG, or H.264. When both ends agree on ALPN `etherquic/2`, the picture stream is `EVI2` and the audio stream is `EAU2`. The original `EVID` stream is still a JPEG or an `EINT` intra frame, and the original `EAUD` stream is still PCM. Quality applies to MJPEG. There is no fixed "JPEG at quality 40" desktop encoder.

The service process does not capture. `etherquic-host --desk` does: it is the console-session helper, and it still has its own capture and encode path (item 4).

Service `4433` is the Ethernet play connection and the video lab tool. On UDP, play asks for `4433` and binds the next free port, up to 16 later, when another program holds `4433`. On these machines that port is `4434`. Discovery announces the bound port. `4434` is also the audio lab tool. `4435` is the input lab tool. `4440` is discovery, not QUIC.

The video lab tool copies one file. Those bytes are the file, not LOC objects. The audio lab tool is the one that puts Opus in LOC objects. The input lab tool sends EQIN documents.

## 3. Lab tools in the crate table

`video/`, `audio/`, and `input/` stay in the top-level crate table as lab tools. They are useful, and they are not the remote-desktop session. The video tool listens on service `4433`. On a machine that is also hosting, that listener is the one that collides with the host.

## 4. Later: one capture path

Have `etherquic-host --desk` call the play session instead of `host/src/desk.rs`. That removes the second capture and encode path. Until then a codec fix has to land in both places. This is a real change. It is not part of tidying the tree, and it is not started here.
