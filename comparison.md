# What a remote-desktop user expects

People who use RustDesk, TeamViewer, AnyDesk, or Parsec expect a computer they can find, a picture they can watch, input that arrives, and a host that stays up when nobody is sitting at it. This note lists those expectations and the state of this tree.

Status is what a person can do from the desk (`etherquic`), the live play session, or the installed host. A library function with no control in the desk is not complete.

| Mark | Meaning |
| --- | --- |
| ✓ | Completed. It works from the desk or the live session on at least one supported system. Limits are in the note. |
| /!\ | Incomplete. Part of it is in the tree, or the usual case still fails. |
| 🚧 | Planned. A plan in this repo describes the work, and it is not shipped. |
| X | Not supported. Nothing here commits to building it. |

Platforms in the installers are Windows, Linux, macOS, and an Android player. The picture is its own winit window. Desk tabs group those windows. They do not draw the remote desktop inside the desk.

## Finding a computer and joining

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Connect on the local network without an account | ✓ | Discovery on service 4440. The desk shows latency, jitter, min, average, max, standard deviation, and a small graph. |
| Saved, recent, and discovered computers | ✓ | One roster with a filter. A closed connection leaves the active list. |
| Type an address | ✓ | A MAC for Ethernet, or an IPv4/IPv6 address for UDP. |
| QR code or share text for a phone | ✓ | The host card shows the name, MAC, service, port, and addresses. The Android player parses that text. |
| Public ID that works from another network | X | There is no rendezvous server and no ID. |
| Relay when both sides are behind NAT | X | A route has to exist already. UDP to a typed address is the wide-area path. |
| Address book that syncs through an account | X | The roster is on this computer. |
| Several outbound sessions at once | ✓ | Each player is a tab. One desk window can hold some of them, and **New window** opens another group. |
| See who is connected to this host | ✓ | The host card lists service guests from `presence.txt` as connected, and an in-window host shows its player count. |
| Active sessions with a live status | ✓ | Each open tab is `[connected]`, `[paused/idle]`, or `[error: reason]`, plus its window group. |
| Choose which adapters to listen and discover on | ✓ | Each row shows a friendly name, MAC, IPv4, one IPv6, and whether it is physical, virtual, loopback, or capture. Physical and virtual start on. Loopback and capture start off. **All** is Auto. One selected adapter dials that name. The last listening adapter stays on. |
| Change the adapter or L2/L3 on an open player | ✓ | **Apply interface / layer** closes that player and dials the same peer again. |
| Double-click a session to raise its picture | ✓ | The picture unminimizes, shows, and takes focus. On macOS that focus call is what brings a fullscreen window on another Space forward. There is no Spaces API. |

## Picture

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Live desktop picture | ✓ | The play session captures, encodes, and draws in a winit window. |
| Picture inside the desk tab | X | Plan-2 keeps the framebuffer out of GPUI. Tabs only group the picture windows. |
| Hardware and software codecs | ✓ | Auto prefers AV1, H.265, and H.264 hardware when that encoder opens, then GPU PyroWave, JPEG XS, OpenAPV, MJ2K, MJPEG, H.264 software, and CPU PyroWave. On the Radeon HD 6670, GPU PyroWave at 1280×720 holds about 44–48 fps. |
| Change codec, scale, look, and quality on the open view | ✓ | Each player keeps its own view and tune. Scale, look, dither, quality, dirty rectangles, cursor, keyboard, mouse, upscale, frame generation, and YUV apply to that view. Codec, audio, fps, and carrier are resent on that connection. |
| Quality and frame rate | ✓ | Quality is 1–100. Capture fps can change while a host is up. Auto encode is 15 fps software and 30 fps hardware. A 60 fps request is only as fast as the codec. PyroWave on that GPU stays under 50 fps at 720p. |
| Skip unchanged picture, dirty tiles | ✓ | Identical frames send nothing. MJPEG and MJ2K can send type-`R` tiles. H.264, H.265, and AV1 use a long GOP when dirty rectangles are on. |
| Dirty tiles from the display engine | X | Detection is in this program. DXGI dirty rectangles are outside `plan.md`. |
| PyroWave dirty tiles | /!\ | PyroWave still sends a full picture when something changed. Skip-unchanged still applies. |
| Force a new keyframe after a lost frame | 🚧 | `plan.md` leaves a player refresh / forced IDR for later. |
| Client upscale (FSR, DLSS, XeSS) | ✓ | FSR 1 runs here. DLSS and XeSS are sharpness presets on that same spatial path, picked from the GPU vendor. On asks the host for 720p. |
| Frame generation | ✓ | The player can insert a synthesised picture between arrivals. |
| Cursor: local, remote, or hidden | ✓ | A cursor mode is part of the live view. |
| Pick which monitor to share | /!\ | The desk lists the monitors the host reported. The host shares the primary. The row says picking another is not available yet. |
| Multi-monitor as one canvas | X | One primary picture. |
| Automatic fullscreen | /!\ | The picture window can already be fullscreen. Nothing switches it to fullscreen when the session starts. |
| Raise a fullscreen picture on another desktop | /!\ | Double-click focuses that window. A Mac Space comes forward only if the window manager honours that focus. |
| Stats for latency and bitrate | ✓ | The picture can show them. Network trace keeps them on screen when input-timing trace would hide them. |
| Record the session to a file | X | Capture under Settings writes `ether.pcap` and `app.pcap` for debugging. That is not a video recording. |

## Sound

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Hear the remote desktop | ✓ | Windows reads the default output as loopback. Linux prefers a source whose name contains `monitor` or `loopback`. Opus and PCM are the live codecs. |
| Pick the playback device and the shared device | /!\ | Both are settings. A running host uses the new share device the next time capture opens. It does not restart audio on every tweak. |
| AAC | X | The preference list has an AAC row. Nothing encodes it. |
| Talk with a microphone, or a voice call beside the desktop | X | A microphone is only a last-resort capture source when no desktop monitor exists. There is no separate talkback stream. |
| Silence while the remote desktop is quiet | ✓ | The Windows helper sends silence when WASAPI loopback has nothing to read, and reopens the default output after ten seconds without a packet. |

## Keyboard, mouse, and gamepads

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Keyboard and mouse | ✓ | Focus the picture window. Windows injects into the desktop that has input, including the secure desktop and the screen saver. |
| Turn keyboard, mouse, or gamepads off | ✓ | Each one is a live switch, including on the open connection. |
| View only | ✓ | Connect without sending keyboard, mouse, or controllers. Other players can still inject. |
| Gamepad | ✓ | A local pad sends while the picture is open. Linux uses `/dev/uinput` (`etherquic-play pad N`). Windows uses an Xbox 360 controller when ViGEmBus is installed. |
| On-screen controls for a phone | ✓ | The Android player has sticks, d-pads, and buttons. |
| Wayland injection | /!\ | The keyboard is a virtual keyboard. The pointer tries KWin fake-input, then a virtual pointer, then libei from the remote-desktop portal. The portal can ask. |
| X11-only injection | X | An X11 session with none of those Wayland seats gets no keyboard or pointer events. |
| Linux signed-out input and capture | /!\ | The in-window host captures. The Linux `etherquic-host` service does not capture or inject. |

## Clipboard, files, and chat

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Paste this computer's clipboard text into the remote desktop | ✓ | **Paste clipboard**, and Ctrl+V in the picture. The text is UTF-8. |
| Type the clipboard as keystrokes | ✓ | **Send clipboard as keystrokes** uses the keystroke pipe instead of the clipboard pipe. One paste does not do both. |
| Clipboard the other way, or images and files on the clipboard | X | The host receives the player's string. There is no continuous sync and no image or file clipboard. |
| Send a named file during the session | /!\ | The live session can queue one named file in either direction (`queue_file`). The desk has no file picker, progress, or save dialog. The video lab tool's one-file send is a different program. |
| A file manager on the remote computer | X | One named payload is the whole transfer. |
| Drag and drop | X | |
| Chat with everyone, a team, or one person | /!\ | The session routes All, Team, and One, and tests deliver those messages. The desk has no chat box, roster editor, or history. |
| Whiteboard, arrows, or drawing on the picture | X | |

## A host that stays up

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Host from the desk window | ✓ | **Host** shares this desktop and its audio. |
| Install a background service | ✓ | Windows service `etherquic-service`. macOS LaunchDaemon `dev.etherquic.host`, installed from the app with an administrator password. |
| Unattended accept | ✓ | `host.conf` `unattended=yes` accepts immediately. The default waits. |
| Accept or refuse one player | ✓ | `etherquic-host --approve` and `--deny`, or the `control/approve` and `control/deny` files. The wait is 60 seconds. |
| Exclusive session | ✓ | An exclusive connect closes the other viewers and later joins are refused until that seat leaves. |
| Several people watching one host | ✓ | Without exclusive, the room admits more than one connection. |
| Wake the display and leave the screen saver | ✓ | The Windows desk helper, when a player attaches, resets the idle timer, nudges the pointer to end a screen saver, and holds the display awake until the player leaves. Power settings stay as they are. |
| Capture the lock screen | ✓ | The Windows helper follows `Default`, `Screen-saver`, and `Winlogon`. |
| Linux or macOS lock-screen capture | /!\ | That desktop follow is the Windows helper. |
| Share the screen from the service with nobody logged in | /!\ | Windows `--desk` runs in the console session. The service process itself does not capture. On Linux the service listens and does not capture. |
| One capture path for the service and the in-window host | 🚧 | `docs/workspace-plan.md` leaves merging `--desk` and the play session for later. |
| Blank the host's monitors while someone is connected | X | |
| Lock the host when the session ends | X | |
| Reboot, shut down, or wake the host on the LAN | X | An Npcap installer exit can report that a reboot is required. Setup does not reboot the machine, and a player cannot. |
| Remote terminal | X | |

## Security and accounts

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Encrypted QUIC session | ✓ | Quinn with rustls (AES-128-GCM). No `ring` on that path. |
| The player checks the host certificate | X | `ETHERQUIC_CERTS` supplies `cert.pem` and `key.pem`. The player does not verify them. The demo pair in the repo is public. |
| One-time or permanent remote-desktop password | X | The gate is approve/deny or unattended. There is no RustDesk-style password. |
| User accounts, groups, SSO, or a second factor | X | |
| Per-person audit log of who did what | X | `host.log` records accept, capture, and wake lines. It is not an audit trail. |
| Confirm before a stranger connects | ✓ | The default unattended setting is no. |
| AVB or ST 2110 encryption | X | AVB is trusted because the LAN was reserved. ST 2110 defaults to `plain`. `gcm` exists for ST 2110 and UDP/RTP and is not what cleartext broadcast gear decodes. |

## Install, update, and the desk itself

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Windows MSI, macOS pkg and dmg, Linux tarball, Android APK | ✓ | Tagged releases. **Download and install** in Settings fetches the asset for this computer. Beta tags are optional. |
| Tray and taskbar | ✓ | The taskbar button is on while hosting. Closing the window hides it. Quit is on the tray icon. |
| English only | /!\ | The language setting accepts `en`. The desk says more languages are not translated yet. |
| Quick settings on the open session | ✓ | A control under the active list, the same control on a shelf window, and Ctrl+Alt+S. |
| Network trace separate from input timing | ✓ | While it is on, latency and packet-error lines stay, and picture stats stay visible. |
| Packet capture for a person debugging the link | ✓ | Settings → Capture writes `ether.pcap` and `app.pcap`. Wireshark Lua dissectors live in `wireshark/`. The switch is not saved. |
| Run without an account or a vendor cloud | ✓ | Install, discovery, and the session stay on the machines you run. |

## How the bits move

RustDesk and TeamViewer hide this. It is the product here, so it is part of the comparison.

| Feature | Status | Where this tree is |
| --- | --- | --- |
| QUIC over Ethernet, EtherType `0x88B6` | ✓ | First choice on a LAN. NDIS `membrane_ndis` on Windows, then Npcap. libpcap on Linux. BPF and AF_NDRV on macOS. |
| QUIC over UDP | ✓ | The phone uses this. The desk uses it when the peer is an IP address. The port is 4433, or the next free port up to 16 later. Discovery announces the bound port. |
| QUIC datagrams for the short LAN path | ✓ | Carrier 2, only when both sides agree. A stream-only peer is never sent a datagram. |
| UDP/RTP without Quinn | /!\ | Carrier 3 is in the tree (picture, audio, input ports, `gcm`). A desktop connect on carriers 3, 4, and 5 returns before the dial. |
| AVB / IEEE 1722 | /!\ | Carrier 4 when the operator turns it on. Shaped send, then a reservation. EtherType `0x22F0`. A live switch is manual. Not a normal remote-desktop connect. |
| ST 2110 | /!\ | Carrier 5 when the operator turns it on. Uncompressed or JPEG XS picture, PCM audio. A software clock runs when no PTP node is present. |
| Open the Ethernet adapter without root in the desk | 🚧 | `docs/link-helper-plan.md`. Today the macOS daemon and a Linux process need the raw socket themselves. UDP still works without that privilege. |
| Android on Ethernet | X | The APK is UDP. The link-helper plan leaves a rooted phone out of this work. |

## Platforms

| Feature | Status | Where this tree is |
| --- | --- | --- |
| Windows host and player | ✓ | Desk, service, and console helper. |
| macOS host and player | ✓ | Desk and LaunchDaemon. BPF needs the root daemon. |
| Linux host and player | /!\ | The desk hosts and connects. Injection needs a Wayland seat. The service does not share the screen. |
| Android player | ✓ | UDP, QR or pasted share text, on-screen controls. |
| Android host | X | |
| iOS or iPadOS | X | |
| A session in a browser | X | |
| Headless server with a virtual display | X | Capture follows a real desktop session. |
