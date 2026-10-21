# Desk layout: live settings, hosting, and sessions

The desk draws one GPUI window. The picture is a separate winit window. Settings, the host card, and the connection chips did not share one session object, so a change on the desk often updated a file or a process-global default and left the open connection alone. This note is the critique of that layout and the behaviour that replaces it. `plan.md` stays the dirty-rectangle note.

## What was wrong

1. **Settings and the open view were different objects.** `apply_settings` wrote the process-global `LiveView`. The macOS picture child received that view once, on its argv. Codec, carrier, and frame rate were not on the offer the player resends every 200 ms. Scale and filters could move in the global view while a second connection still showed the first snapshot. The host card and the connect card are siblings, so nothing in one card was the session the other card had opened.

2. **Hosting did not say what the service was doing.** The in-window Host button only knew `Link::role()`. A LaunchDaemon or `etherquic-host` process could be running, hosting, or idle with players attached, and the card still said "Share this desktop". Capture fps was fixed when the capture loop started. Changing the listen adapter rewrote nothing the service was bound to.

3. **A connection was a chip, not a session you could inspect.** Connecting parked the previous `Link` and added a chip in the top left. There was no list of machines attached to this host, and no list under discovery of the connections this desk had opened. Status text was whatever the link last reported. Closing a connection removed the chip only after the tab model said so; a failed open could leave the chip if the tab was not aborted.

4. **One NIC picker, and it hid the interesting adapters.** Details offered Auto or a single name. `nic_shown` dropped `any`, `lo`, `nflog`, and `nfqueue`, and the label was a name plus a speed. Virtual links (`docker0`, `bridge0`, `utun`) and real links looked the same, and you could not listen on two of them.

5. **Names were not enough to choose an adapter.** The row did not show the MAC or the addresses that discovery and dial actually use.

6. **A click only switched the chip.** It did not show the view, codec, carrier, or layer that connection was using, and it did not raise the picture window. A minimized picture, or one on another desktop, stayed where it was.

7. **Input tracing hid the picture stats.** `trace_input` is a timing flag. When it is on, the picture loop suppresses stats. There was no separate switch for latency, loss, and packet errors.

8. **One picture window per connection, and no way to group them.** That window cannot become a GPUI tab: the framebuffer is softbuffer/winit, and on macOS GPUI already owns the main thread so the picture runs in a child process. RustDesk-style "everything in this window" would mean decoding into GPUI. That is not this change. What was missing was a way to group those picture windows, and a way to reach scale, codec, and carrier without digging through the connect card.

## What the desk does now

### One live view per connection

Each player tab stores `SessionLive`: the `LiveView` and `LiveTune` arcs handed to that connection, plus the peer and the UDP address it dialed. `stage_session` runs before `Link::connect`, including a launch-command connect. Immediate fields (scale, look, dither, quality, dirty rects, cursor, keyboard, mouse, upscale, frame generation, YUV) are written into that view. Codec, audio, carrier, and fps are written into the tune. The in-process player reads those arcs. The macOS child receives the same values as `set key=value` lines on stdin (`parent_ctl`). Settings still update the active player tab, and they write `control/host-settings` for the service.

Left-click an active row or a connection chip to select it and open quick settings. The panel can change scale, picture codec, audio, fps, carrier, YUV, look, dither, cursor, upscale, frame generation, quality, keyboard, mouse, and dirty rects on that connection. Interface and L2/L3 are not a silent retune: **Apply interface / layer** closes that player and dials the same peer again with the current listen adapter and the layer chips (L2, or an IPv4/IPv6 address discovery has for that peer). A host tab does not redial.

Double-click selects the connection, switches the picture hub to that seat, and calls `request_raise`. The picture thread unminimizes, shows, and focuses the winit window. On macOS that focus call is what moves a fullscreen window on another Space forward. The desk does not talk to the Spaces API itself. The GPUI desk also activates.

### Hosting status, and settings that reach the service

The host card reads `presence.txt` next to `host.conf` about twice a second. The service writes `running`, `hosting`, and one `guest=` line per player. The card says whether a service is installed, whether it is running, and whether it is hosting, and it lists those guests as `[connected]`. An in-window host adds "This window is hosting · N players".

`control/host-settings` carries dirty rects, capture fps, the listen adapter, and the share audio device. The service applies dirty rects and capture fps on its tick. Audio is logged and used the next time capture opens, not by tearing the device down on every tweak. A listen adapter that differs from `host.conf` is written back (mac, service, and unattended are preserved) and the L2 listener reopens without a stop note. The in-window host restarts when the dial adapter changes, including when that host tab is parked behind a player.

### Active connections, under the roster

Below discovered, recent, and saved hosts:

- **Active connections** are the tabs still open. Each row is `name  [connected] | [paused/idle] | [error: reason]  · window N`. The badge comes from the link status (the parked link, if that tab is not current). When the tab closes, the row goes away.
- **Guests** on the host card are who is attached to the host service, from `presence.txt`, not from the outbound list.

### Which adapters we listen on

`list_faces` classifies each adapter as physical, virtual, loopback, or capture (`any`, `nflog`, `nfqueue`, `dbus`, `pktap`). The host card lists every face with its friendly name, MAC, IPv4, and one IPv6 address. Physical and virtual start on. Loopback and capture start off. **All** restores that default. The last listening adapter cannot be turned off. Capture rows are visible and not clickable. Discovery probes every adapter that is on. One adapter selected means dial that name. The default set means Auto.

The details NIC row is the same listen set: Auto is **All**, and a name listens only on that adapter. It no longer writes `desk.nic` by itself.

### Network trace

**Network trace** is separate from **Trace input timing**. It keeps the last lines from the player (`debug_log`, and the macOS child's stderr trace lines) and from the session status. The active-connection block shows the latest lines while it is on. Stats stay visible when this trace is on, even if input timing would have hidden them. Ctrl+Alt+S toggles quick settings unless a text field has focus.

### Several windows, each with its own tabs

Sessions start in window group 1, which is this desk. **New window** moves the current session into a new group and opens a shelf window that lists only that group. A shelf click, double-click, close, or quick-settings toggle is a command the desk applies on its next frame. The picture stays in the winit window for that session. One desk can hold three sessions and another shelf two. Dragging a picture framebuffer into a GPUI tab is not part of this layout.

Quick settings sit under the active list, and on the shelf as a button. Ctrl+Alt+S opens the same panel.

## Limits

- The picture is still one winit/softbuffer window per connection (one hub with seats on platforms that share a process). Tabs group those windows. They do not draw the remote desktop inside GPUI.
- Raising a window on another macOS Space is `focus_window` after unminimize. There is no separate Spaces call.
- Redial uses the peer stored for that session and the layer and adapter selected now. It opens a new connection. Codec and carrier stay on the open connection and are resent on the offer.
- The service applies audio on the next capture open.
- An empty listen set is refused by the last-adapter rule. If the machine has only loopback and capture, **All** leaves discovery on the default adapter.
- `src/carrier/session.rs` was referenced and not in the tree. The desk does not use it. The re-export was removed so the library builds. AVB and ST 2110 packet wrappers keep the RTP bytes the seam already sends.
