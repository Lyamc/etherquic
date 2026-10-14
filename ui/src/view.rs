//! Parsec-style console: host this PC, or sit down as the player.
//!
//! The host card shows a QR code of this PC so a phone can read the address,
//! the service, and the port. The taskbar button is applied from a deferred
//! callback so the Win32 call is not inside the render borrow. It runs only
//! when the desired state changes.

use std::time::{Duration, Instant};

use etherquic::discover::Found;
use etherquic::{BackendProbe, MediaBackend};
use etherquic_play::{
    display_hz, this_computer, AudioId, AudioMachine, MachineCodecs, PicturePref,
};
use etherquic_ui::{
    active_layer, apply_override, audio_for, audio_hint, audio_match_line, backend_hint,
    backend_label, backend_line, capture_rate, dial_target, format_mac, lanes, list_nics,
    listed_top, load_connections, load_preferences, local_video_line, mac_key, qr_grid,
    remote_backend_line, remote_hardware_line, store_connections, store_preferences, swap_earlier,
    swap_later, this_pc, top_selection, video_hint, video_match_line, Applied, ConnectionOverride,
    ConnectionPrefs, DialTarget, FrameRate, HexMac, Layer, Link, NicChoice, Preferences, QrGrid,
    Role, Share, CLIENT_MAC, FPS_CHOICES, HOST_MAC, SOFTWARE_FPS,
};

use crate::scan::Scan;
use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Application, Bounds, ClickEvent,
    Context, FocusHandle, FontWeight, KeyDownEvent, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};

const BG: u32 = 0x12141a;
const CARD: u32 = 0x1b1f29;
const CARD_EDGE: u32 = 0x2c3340;
const TEXT: u32 = 0xe7ecf3;
const MUTED: u32 = 0x9aa3b2;
const HOST_GREEN: u32 = 0x3dd68c;
const PLAYER_BLUE: u32 = 0x4cc2ff;
const DANGER: u32 = 0xff6b6b;
const INK: u32 = 0x0e1116;

const USAGE: &str = "\
etherquic
etherquic host [--nic IFACE]
etherquic connect --peer-mac MAC [--nic IFACE]

host shares this machine. connect joins one.
";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Window,
    Host,
    Connect,
}

#[derive(Clone)]
struct Launch {
    command: Command,
    peer: HexMac,
    nic: Option<String>,
}

impl Launch {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let args: Vec<String> = args.collect();
        if args.is_empty() {
            return Ok(Self {
                command: Command::Window,
                peer: HexMac::from_bytes(HOST_MAC),
                nic: None,
            });
        }
        if args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
            return Err(USAGE.into());
        }
        let command = match args[0].as_str() {
            "host" => Command::Host,
            "connect" => Command::Connect,
            other => return Err(format!("unknown command {other}\n{USAGE}")),
        };
        let mut peer = None;
        let mut nic = None;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--peer-mac" => {
                    i += 1;
                    let value = args.get(i).ok_or("--peer-mac needs an address")?;
                    peer = Some(mac_from_text(value)?);
                }
                "--nic" => {
                    i += 1;
                    nic = Some(args.get(i).cloned().ok_or("--nic needs a name")?);
                }
                other => return Err(format!("unknown argument {other}\n{USAGE}")),
            }
            i += 1;
        }
        if command == Command::Host && peer.is_some() {
            return Err(format!("host does not take --peer-mac\n{USAGE}"));
        }
        if command == Command::Connect && peer.is_none() {
            return Err(format!("connect needs --peer-mac\n{USAGE}"));
        }
        Ok(Self {
            command,
            peer: peer.unwrap_or_else(|| HexMac::from_bytes(HOST_MAC)),
            nic,
        })
    }
}

fn mac_from_text(text: &str) -> Result<HexMac, String> {
    let mut mac = HexMac::default();
    for ch in text.chars() {
        mac.push(ch);
    }
    if mac.parse().is_none() {
        return Err(format!("peer address needs 12 hex digits, got {text}"));
    }
    Ok(mac)
}

pub fn run() {
    let launch = match Launch::parse(std::env::args().skip(1)) {
        Ok(launch) => launch,
        Err(err) => {
            eprintln!("etherquic: {err}");
            std::process::exit(2);
        }
    };
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = WindowBounds::centered(size(px(960.), px(880.)), cx);
        let launch = launch.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("etherquic".into()),
                    ..Default::default()
                }),
                app_id: Some("etherquic".into()),
                window_min_size: Some(size(px(760.), px(560.))),
                ..Default::default()
            },
            move |_window, cx| {
                let launch = launch.clone();
                cx.new(|cx| Desk::new(cx, launch))
            },
        )
        .expect("open etherquic window");
        cx.activate(true);
    });
}

struct Desk {
    link: Link,
    nics: Vec<NicChoice>,
    nic_error: Option<String>,
    nic: Option<String>,
    peer: HexMac,
    /// L2 unless the user picks an announced IPv4 or IPv6 address.
    layer: Layer,
    focus: FocusHandle,
    message: Option<String>,
    /// `true` matches a freshly created normal window (`WS_EX_APPWINDOW`).
    taskbar_on: bool,
    scan: Scan,
    /// Adapter the current code was built from. `None` is Auto.
    share_nic: Option<String>,
    share_at: Instant,
    share_ready: bool,
    share_text: String,
    share: Share,
    qr: Option<QrGrid>,
    /// Global order. The first codec a discovered computer can run is the match.
    video_pref: Vec<PicturePref>,
    audio_pref: Vec<AudioId>,
    /// Skip a codec unless the sender has a hardware encoder. MJPEG stays available.
    skip_without_hw: bool,
    /// Ask the remote to confirm input and show how long that took.
    trace_input: bool,
    /// Default rate, and the rate this PC captures at while hosting.
    fps: FrameRate,
    /// Display refresh, when it can be read. This is the server rate for Auto.
    display_hz: Option<u32>,
    /// Per-computer pins. Missing fields use the defaults above.
    connections: ConnectionPrefs,
    /// Packet capture. Not written to the preferences file.
    capturing: bool,
    prefs_open: bool,
    local_video: MachineCodecs,
    local_audio: AudioMachine,
    /// Tools found once at startup. Rendering does not probe again.
    backends: BackendProbe,
    /// Opus tool order. The first installed tool is the one this computer announces.
    backend_pref: Vec<MediaBackend>,
    /// The installed tool at the front of `backend_pref`.
    backend: MediaBackend,
    /// Themaister's shared library, not merely a Vulkan loader.
    pyrowave_library: bool,
}

impl Desk {
    fn new(cx: &mut Context<Self>, launch: Launch) -> Self {
        let (nics, nic_error) = match list_nics() {
            Ok(nics) => (nics, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        let backends = etherquic::probe_backends();
        let saved = load_preferences();
        let backend = etherquic::pick_backend(&saved.opus, &backends);
        let mut desk = Self {
            link: Link::idle(),
            nics,
            nic_error,
            nic: launch.nic.clone(),
            peer: launch.peer.clone(),
            layer: Layer::L2,
            focus: cx.focus_handle(),
            message: None,
            taskbar_on: true,
            scan: Scan::start(launch.nic.clone()),
            share_nic: None,
            share_at: Instant::now(),
            share_ready: false,
            share_text: String::new(),
            share: Share::blank(),
            qr: None,
            video_pref: saved.video,
            audio_pref: saved.audio,
            skip_without_hw: saved.skip_without_hw,
            trace_input: saved.trace_input,
            fps: saved.fps,
            display_hz: display_hz(),
            connections: load_connections(),
            capturing: etherquic::capture_active(),
            prefs_open: false,
            local_video: this_computer(),
            local_audio: audio_for(backends, backend),
            backends,
            backend_pref: saved.opus,
            backend,
            pyrowave_library: etherquic::pyrowave_library_present(),
        };
        desk.refresh_share();
        desk.message = match launch.command {
            Command::Host => {
                let fps = desk.capture_fps();
                desk.link.host(launch.nic, desk.backend, fps).err()
            }
            Command::Connect => match desk.peer.parse() {
                Some(peer) => {
                    let trace = desk.applied(peer).trace_input;
                    desk.link.connect(launch.nic, peer, trace, 0, None).err()
                }
                None => Some("Peer address needs 12 hex digits.".into()),
            },
            Command::Window => None,
        };
        if desk.message.is_none() {
            if let Some(dir) = etherquic::capture_dir() {
                desk.message = Some(format!("Capturing to {}", dir.display()));
            }
        }
        desk.start_poll(cx);
        desk
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(400)).await;
            if this
                .update(cx, |desk, cx| {
                    desk.link.reap();
                    desk.refresh_share();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    fn sync_taskbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.link.shows_taskbar();
        if want == self.taskbar_on {
            return;
        }
        let Some(hwnd) = crate::taskbar::hwnd_of(window) else {
            return;
        };
        self.taskbar_on = want;
        window.set_window_title(if want { "etherquic host" } else { "etherquic" });
        cx.defer(move |_cx| crate::taskbar::apply(hwnd, want));
    }

    fn on_host(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.link.reap();
        if self.link.role() == Role::Client {
            return;
        }
        if self.link.role() == Role::Host {
            self.link.stop();
            self.message = None;
            cx.notify();
            return;
        }
        match self.link.host(self.nic.clone(), self.backend, self.capture_fps()) {
            Ok(()) => self.message = None,
            Err(err) => self.message = Some(err),
        }
        cx.notify();
    }

    fn on_connect(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.link.reap();
        if self.link.role() == Role::Host {
            return;
        }
        if self.link.role() == Role::Client {
            self.link.stop();
            self.message = None;
            cx.notify();
            return;
        }
        let Some(peer) = self.peer.parse() else {
            self.message = Some("Peer address needs 12 hex digits.".into());
            cx.notify();
            return;
        };
        let device = self
            .scan
            .board()
            .devices
            .iter()
            .find(|device| device.l2_mac == peer)
            .cloned();
        let trace = self.applied(peer).trace_input;
        let host_l2 = device.as_ref().map(|device| device.l2_backend).unwrap_or(0);
        let (v4, v6, port) = match device.as_ref() {
            Some(device) => (device.ipv4.clone(), device.ipv6.clone(), device.l3_port),
            None => (Vec::new(), Vec::new(), 0),
        };
        let layer = active_layer(self.layer, &v4, &v6);
        let udp = match dial_target(layer, port, ipv6_scope(self.nic.as_deref(), &self.nics)) {
            DialTarget::L2 => None,
            DialTarget::Udp(addr) => Some(addr),
            DialTarget::NeedsNic => {
                self.message = Some(
                    "That IPv6 address is link-local. Pick the adapter, not Auto.".into(),
                );
                cx.notify();
                return;
            }
        };
        match self
            .link
            .connect(self.nic.clone(), peer, trace, host_l2, udp)
        {
            Ok(()) => self.message = None,
            Err(err) => self.message = Some(err),
        }
        cx.notify();
    }

    /// Rebuild the phone code when the adapter changes, and every few seconds
    /// so a late DHCP address shows up. The grid is kept while the text is unchanged.
    fn refresh_share(&mut self) {
        let due = !self.share_ready
            || self.share_nic != self.nic
            || self.share_at.elapsed() >= Duration::from_secs(5);
        if !due {
            return;
        }
        self.share_ready = true;
        self.share_nic = self.nic.clone();
        self.share_at = Instant::now();
        let share = this_pc(self.nic.as_deref());
        let text = share.text();
        if text != self.share_text {
            self.qr = qr_grid(&text).ok();
            self.share_text = text;
        }
        self.share = share;
    }

    fn on_peer_key(&mut self, ev: &KeyDownEvent) {
        let key = ev.keystroke.key.as_str();
        let mods = &ev.keystroke.modifiers;
        if key == "backspace" {
            if mods.control || mods.alt || mods.platform {
                self.peer.clear();
            } else {
                self.peer.backspace();
            }
            return;
        }
        if ev.is_held || mods.control || mods.alt || mods.platform {
            return;
        }
        if let Some(ch) = ev
            .keystroke
            .key_char
            .as_deref()
            .and_then(|text| text.chars().next())
            .or_else(|| (key.len() == 1).then(|| key.chars().next()).flatten())
        {
            self.peer.push(ch);
        }
    }

    fn status_line(&self) -> String {
        self.message.clone().unwrap_or_else(|| self.link.status())
    }

    fn status_color(&self) -> u32 {
        if self.link.shows_taskbar() {
            return HOST_GREEN;
        }
        if self.link.role() == Role::Client {
            return PLAYER_BLUE;
        }
        let text = self.status_line().to_ascii_lowercase();
        if text.contains("fail")
            || text.contains("timed")
            || text.contains("missing")
            || text.contains("needs")
            || text.contains("denied")
            || text.contains("error")
            || text.contains("dropped")
            || text.contains("closed")
        {
            DANGER
        } else {
            MUTED
        }
    }

    fn host_card(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let hosting = self.link.role() == Role::Host;
        let enabled = self.link.role() != Role::Client;
        let label = if hosting { "Stop hosting" } else { "Host" };
        let edge = if hosting { HOST_GREEN } else { CARD_EDGE };
        let capture = self.capture_fps();
        let rates = self.global_fps_row("host-fps", cx);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .gap_2()
            .p_3()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(edge))
            .rounded_md()
            .child(card_title("Host", HOST_GREEN))
            .child(card_body(
                "Share this desktop and its audio. A player sees it, hears it, and controls it.",
            ))
            .child(card_meta(format!("This PC captures at {capture} fps.")))
            .child(rates)
            .child(phone_share(&self.qr))
            .child(div().flex_1())
            .child(action_button(
                "host-btn",
                label,
                enabled,
                HOST_GREEN,
                cx.listener(|desk, _, window, cx| {
                    desk.on_host(window, cx);
                }),
            ))
    }

    fn connect_card(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playing = self.link.role() == Role::Client;
        let enabled = self.link.role() != Role::Host;
        let label = if playing { "Disconnect" } else { "Connect" };
        let edge = if playing { PLAYER_BLUE } else { CARD_EDGE };
        let focused = self.focus.is_focused(window);
        let selection = self.connection_selection();
        let typed = self.peer.display();
        let field_color = if typed.is_empty() { MUTED } else { TEXT };
        let field_text = if typed.is_empty() {
            "Host address".to_string()
        } else {
            typed
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .gap_2()
            .p_3()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(edge))
            .rounded_md()
            .child(card_title("Connect", PLAYER_BLUE))
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(PLAYER_BLUE))
                    .child(selection),
            )
            .child(card_body(
                "Open the remote desktop. Focus that window to use the keyboard and the mouse.",
            ))
            .child(card_meta(format!(
                "This PC appears as {}",
                format_mac(CLIENT_MAC)
            )))
            .child(
                div()
                    .id("peer-mac")
                    .flex()
                    .w_full()
                    .h(px(36.))
                    .px_2()
                    .items_center()
                    .overflow_hidden()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(if focused { PLAYER_BLUE } else { CARD_EDGE }))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(rgb(field_color))
                    .track_focus(&self.focus)
                    .child(div().w_full().truncate().child(field_text))
                    .on_click(cx.listener(|desk, _: &ClickEvent, window, cx| {
                        window.focus(&desk.focus);
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, _, cx| {
                        desk.on_peer_key(ev);
                        cx.notify();
                    })),
            )
            .child(self.path_row(cx))
            .child(div().flex_1())
            .child(action_button(
                "connect-btn",
                label,
                enabled,
                PLAYER_BLUE,
                cx.listener(|desk, _, window, cx| {
                    desk.on_connect(window, cx);
                }),
            ))
    }

    fn path_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let peer = self.peer.parse();
        let device = peer.and_then(|mac| {
            self.scan
                .board()
                .devices
                .iter()
                .find(|device| device.l2_mac == mac)
                .cloned()
        });
        let v4 = device.as_ref().map(|device| device.ipv4.clone()).unwrap_or_default();
        let v6 = device.as_ref().map(|device| device.ipv6.clone()).unwrap_or_default();
        let active = active_layer(self.layer, &v4, &v6);
        let mut chips = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
        chips = chips.child(path_chip(
            "path-l2",
            "L2",
            matches!(active, Layer::L2),
            cx.listener(|desk, _, _, cx| {
                desk.layer = Layer::L2;
                cx.notify();
            }),
        ));
        for (index, ip) in v4.into_iter().enumerate() {
            let on = matches!(active, Layer::V4(chosen) if chosen == ip);
            chips = chips.child(path_chip(
                SharedString::from(format!("path-v4-{index}")),
                format!("IPv4 {ip}"),
                on,
                cx.listener(move |desk, _, _, cx| {
                    desk.layer = Layer::V4(ip);
                    cx.notify();
                }),
            ));
        }
        for (index, ip) in v6.into_iter().enumerate() {
            let on = matches!(active, Layer::V6(chosen) if chosen == ip);
            chips = chips.child(path_chip(
                SharedString::from(format!("path-v6-{index}")),
                format!("IPv6 {ip}"),
                on,
                cx.listener(move |desk, _, _, cx| {
                    desk.layer = Layer::V6(ip);
                    cx.notify();
                }),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .child(section_label("Layer"))
            .child(chips)
    }

    fn nic_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.nic.clone();
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
        let auto_on = selected.is_none();
        row = row.child(nic_chip(
            "nic-auto",
            "Auto",
            auto_on,
            cx.listener(|desk, _, _, cx| {
                desk.nic = None;
                desk.refresh_share();
                cx.notify();
            }),
        ));
        for (index, nic) in self.nics.iter().enumerate() {
            if !nic_shown(nic) {
                continue;
            }
            let name = nic.name.clone();
            let on = selected.as_deref() == Some(nic.name.as_str());
            let label = nic_label(nic);
            row = row.child(nic_chip(
                SharedString::from(format!("nic-{index}")),
                label,
                on,
                cx.listener(move |desk, _, _, cx| {
                    desk.nic = Some(name.clone());
                    desk.refresh_share();
                    cx.notify();
                }),
            ));
        }
        row
    }

    fn device_section(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let board = self.scan.board();
        let selected = self.peer.parse();
        let mut list = div().flex().flex_col().gap_2().w_full().min_w_0();
        if board.devices.is_empty() {
            let text = if let Some(err) = board.error.clone() {
                err
            } else if board.looking {
                "Looking for hosts…".into()
            } else {
                "No hosts answered.".into()
            };
            let color = if board.error.is_some() { DANGER } else { MUTED };
            list = list.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(color))
                    .whitespace_normal()
                    .child(text),
            );
        } else {
            for (index, device) in board.devices.iter().enumerate() {
                let mac = device.l2_mac;
                let on = selected == Some(mac);
                let applied = self.applied(mac);
                let selection = top_selection(
                    &applied.video,
                    &applied.audio,
                    &self.local_video,
                    &self.local_audio,
                    device.video.as_deref(),
                    device.audio.as_deref(),
                    applied.skip_without_hw,
                    applied.fps,
                    None,
                );
                list = list.child(device_card(
                    SharedString::from(format!("dev-{index}")),
                    device,
                    &selection,
                    on,
                    cx.listener(move |desk, _: &ClickEvent, _, cx| {
                        desk.peer = HexMac::from_bytes(mac);
                        cx.notify();
                    }),
                ));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .flex_none()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .min_w_0()
                    .child(section_label("Devices"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(MUTED))
                                    .when(board.looking, |row| row.child("Looking…")),
                            )
                            .child(refresh_chip(cx.listener(|desk, _, _, cx| {
                                desk.scan.refresh(desk.nic.clone());
                                cx.notify();
                            }))),
                    ),
            )
            .child(list)
    }

    fn apply_backend_order(&mut self) {
        self.backend = etherquic::pick_backend(&self.backend_pref, &self.backends);
        self.local_audio = audio_for(self.backends, self.backend);
    }

    fn preferences_now(&self) -> Preferences {
        Preferences {
            video: self.video_pref.clone(),
            audio: self.audio_pref.clone(),
            opus: self.backend_pref.clone(),
            skip_without_hw: self.skip_without_hw,
            trace_input: self.trace_input,
            fps: self.fps,
        }
    }

    fn applied(&self, mac: [u8; 6]) -> Applied {
        apply_override(
            &self.preferences_now(),
            self.connections.get(&mac_key(mac)),
        )
    }

    fn capture_fps(&self) -> u32 {
        capture_rate(
            &self.video_pref,
            &self.local_video,
            self.skip_without_hw,
            self.fps,
            self.display_hz,
        )
    }

    fn persist(&mut self) {
        if let Err(err) = store_preferences(&self.preferences_now()) {
            self.message = Some(format!("Preferences were not saved: {err}"));
        }
    }

    fn edit_connection(&mut self, mac: [u8; 6], edit: impl FnOnce(&mut ConnectionOverride)) {
        let key = mac_key(mac);
        let mut over = self
            .connections
            .get(&key)
            .cloned()
            .unwrap_or_default();
        edit(&mut over);
        self.connections.set(key, over);
        if let Err(err) = store_connections(&self.connections) {
            self.message = Some(format!("This connection was not saved: {err}"));
        }
    }

    fn global_fps_row(
        &mut self,
        id_prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.fps;
        let mut row = div().flex().flex_row().flex_wrap().gap_2().w_full().min_w_0();
        row = row.child(choice_chip(
            SharedString::from(format!("{id_prefix}-auto")),
            "Auto",
            selected == FrameRate::Auto,
            96.,
            cx.listener(|desk, _, _, cx| {
                desk.fps = FrameRate::Auto;
                desk.persist();
                cx.notify();
            }),
        ));
        for fps in FPS_CHOICES {
            let on = selected == FrameRate::Fixed(fps);
            row = row.child(choice_chip(
                SharedString::from(format!("{id_prefix}-{fps}")),
                fps.to_string(),
                on,
                96.,
                cx.listener(move |desk, _, _, cx| {
                    desk.fps = FrameRate::Fixed(fps);
                    desk.persist();
                    cx.notify();
                }),
            ));
        }
        if let FrameRate::Fixed(fps) = selected {
            if !FPS_CHOICES.contains(&fps) {
                row = row.child(choice_chip(
                    SharedString::from(format!("{id_prefix}-{fps}")),
                    fps.to_string(),
                    true,
                    96.,
                    cx.listener(|_, _, _, cx| cx.notify()),
                ));
            }
        }
        row
    }

    fn connection_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mac = self.peer.parse();
        let controls = mac.map(|mac| self.connection_controls(mac, cx));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .flex_none()
            .child(section_label("This connection"))
            .when(mac.is_none(), |panel| {
                panel.child(muted_line(
                    "Select a computer to choose its picture, audio, and rate.".into(),
                ))
            })
            .when_some(controls, |panel, controls| panel.child(controls))
    }

    fn connection_controls(&mut self, mac: [u8; 6], cx: &mut Context<Self>) -> impl IntoElement {
        let stored = self.connections.get(&mac_key(mac)).cloned();
        let applied = self.applied(mac);
        let device = self
            .scan
            .board()
            .devices
            .into_iter()
            .find(|device| device.l2_mac == mac);
        let title = device
            .as_ref()
            .map(|device| {
                if device.name.trim().is_empty() {
                    format_mac(device.l2_mac)
                } else {
                    device.name.clone()
                }
            })
            .unwrap_or_else(|| format_mac(mac));
        let video_line = device.as_ref().map(|device| {
            video_match_line(
                &applied.video,
                &self.local_video,
                device.video.as_deref(),
                applied.skip_without_hw,
            )
        });
        let audio_line = device
            .as_ref()
            .map(|device| audio_match_line(&applied.audio, &self.local_audio, device.audio.as_deref()));
        let picture = self.picture_pin_row(mac, stored.as_ref().and_then(|over| over.video), cx);
        let sound = self.audio_pin_row(mac, stored.as_ref().and_then(|over| over.audio), cx);
        let rate = self.connection_fps_row(mac, stored.as_ref().and_then(|over| over.fps), cx);
        let skip_on = applied.skip_without_hw;
        let trace_on = applied.trace_input;
        let custom = applied.custom;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .p_3()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(CARD_EDGE))
            .rounded_md()
            .child(div().font_weight(FontWeight::BOLD).child(title))
            .child(muted_line(
                "Choices here stay on this computer. Default keeps your saved order. A picture or audio choice is tried first, and the rest of your order still applies.".into(),
            ))
            .child(section_label("Picture"))
            .child(picture)
            .when_some(video_line, |panel, line| panel.child(muted_line(line)))
            .child(section_label("Audio"))
            .child(sound)
            .when_some(audio_line, |panel, line| panel.child(muted_line(line)))
            .child(section_label("Rate"))
            .child(rate)
            .child(muted_line(format!(
                "Auto uses this computer's rate when the picture is hardware-encoded. Without that, Auto is {SOFTWARE_FPS} fps. A host that does not read a rate still sends its own."
            )))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(choice_chip(
                        "conn-skip",
                        "Skip without HW",
                        skip_on,
                        180.,
                        cx.listener(move |desk, _, _, cx| {
                            let next = !skip_on;
                            let global = desk.skip_without_hw;
                            desk.edit_connection(mac, |over| {
                                over.skip_without_hw = (next != global).then_some(next);
                            });
                            cx.notify();
                        }),
                    ))
                    .child(choice_chip(
                        "conn-trace",
                        "Trace input",
                        trace_on,
                        180.,
                        cx.listener(move |desk, _, _, cx| {
                            let next = !trace_on;
                            let global = desk.trace_input;
                            desk.edit_connection(mac, |over| {
                                over.trace_input = (next != global).then_some(next);
                            });
                            cx.notify();
                        }),
                    ))
                    .when(custom, |row| {
                        row.child(choice_chip(
                            "conn-clear",
                            "Use my defaults",
                            false,
                            180.,
                            cx.listener(move |desk, _, _, cx| {
                                desk.edit_connection(mac, |over| {
                                    *over = ConnectionOverride::default();
                                });
                                cx.notify();
                            }),
                        ))
                    }),
            )
    }

    fn picture_pin_row(
        &mut self,
        mac: [u8; 6],
        pin: Option<PicturePref>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut row = div().flex().flex_row().flex_wrap().gap_2().w_full().min_w_0();
        row = row.child(choice_chip(
            "conn-video-default",
            "Default",
            pin.is_none(),
            168.,
            cx.listener(move |desk, _, _, cx| {
                desk.edit_connection(mac, |over| over.video = None);
                cx.notify();
            }),
        ));
        for pref in PicturePref::order() {
            row = row.child(choice_chip(
                SharedString::from(format!("conn-video-{}", pref.token())),
                pref.name(),
                pin == Some(pref),
                168.,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_connection(mac, |over| over.video = Some(pref));
                    cx.notify();
                }),
            ));
        }
        row
    }

    fn audio_pin_row(
        &mut self,
        mac: [u8; 6],
        pin: Option<AudioId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut row = div().flex().flex_row().flex_wrap().gap_2().w_full().min_w_0();
        row = row.child(choice_chip(
            "conn-audio-default",
            "Default",
            pin.is_none(),
            140.,
            cx.listener(move |desk, _, _, cx| {
                desk.edit_connection(mac, |over| over.audio = None);
                cx.notify();
            }),
        ));
        for codec in AudioId::order() {
            row = row.child(choice_chip(
                SharedString::from(format!("conn-audio-{}", codec.token())),
                codec.name(),
                pin == Some(codec),
                140.,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_connection(mac, |over| over.audio = Some(codec));
                    cx.notify();
                }),
            ));
        }
        row
    }

    fn connection_fps_row(
        &mut self,
        mac: [u8; 6],
        pin: Option<FrameRate>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut row = div().flex().flex_row().flex_wrap().gap_2().w_full().min_w_0();
        row = row.child(choice_chip(
            "conn-fps-default",
            "Default",
            pin.is_none(),
            96.,
            cx.listener(move |desk, _, _, cx| {
                desk.edit_connection(mac, |over| over.fps = None);
                cx.notify();
            }),
        ));
        row = row.child(choice_chip(
            "conn-fps-auto",
            "Auto",
            pin == Some(FrameRate::Auto),
            96.,
            cx.listener(move |desk, _, _, cx| {
                desk.edit_connection(mac, |over| over.fps = Some(FrameRate::Auto));
                cx.notify();
            }),
        ));
        for fps in FPS_CHOICES {
            row = row.child(choice_chip(
                SharedString::from(format!("conn-fps-{fps}")),
                fps.to_string(),
                pin == Some(FrameRate::Fixed(fps)),
                96.,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_connection(mac, |over| over.fps = Some(FrameRate::Fixed(fps)));
                    cx.notify();
                }),
            ));
        }
        if let Some(FrameRate::Fixed(fps)) = pin {
            if !FPS_CHOICES.contains(&fps) {
                row = row.child(choice_chip(
                    SharedString::from(format!("conn-fps-{fps}")),
                    fps.to_string(),
                    true,
                    96.,
                    cx.listener(|_, _, _, cx| cx.notify()),
                ));
            }
        }
        row
    }

    fn toggle_capture(&mut self) {
        if self.capturing {
            match etherquic::capture_stop() {
                Ok(info) => {
                    self.message = Some(match (info.dropped_ether, info.dropped_app) {
                        (0, 0) => format!("Capture saved in {}", info.dir.display()),
                        (ether, app) => format!(
                            "Capture saved in {}. {ether} ethernet frames and {app} records were dropped.",
                            info.dir.display()
                        ),
                    });
                    self.capturing = false;
                }
                Err(err) => {
                    self.message = Some(format!("Capture was not saved: {err}"));
                    self.capturing = etherquic::capture_active();
                }
            }
        } else {
            match etherquic::capture_start(None) {
                Ok(dir) => {
                    self.message = Some(format!("Capturing to {}", dir.display()));
                    self.capturing = true;
                }
                Err(err) => {
                    self.message = Some(format!("Capture did not start: {err}"));
                    self.capturing = etherquic::capture_active();
                }
            }
        }
    }

    fn connection_selection(&self) -> String {
        let Some(mac) = self.peer.parse() else {
            return listed_top(&self.video_pref, &self.audio_pref, self.fps);
        };
        let applied = self.applied(mac);
        let device = self
            .scan
            .board()
            .devices
            .into_iter()
            .find(|device| device.l2_mac == mac);
        match device {
            Some(device) => top_selection(
                &applied.video,
                &applied.audio,
                &self.local_video,
                &self.local_audio,
                device.video.as_deref(),
                device.audio.as_deref(),
                applied.skip_without_hw,
                applied.fps,
                None,
            ),
            None => listed_top(&applied.video, &applied.audio, applied.fps),
        }
    }

    fn preferences(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let local = local_video_line(&self.local_video);
        let audio_local = if self.local_audio.opus {
            "PCM, Opus"
        } else {
            "PCM"
        };
        let skip_without_hw = self.skip_without_hw;
        let trace_input = self.trace_input;
        let capturing = self.capturing;
        let peer = self.peer.parse();
        let board = self.scan.board();
        let connection = peer.and_then(|mac| {
            let applied = self.applied(mac);
            let device = board.devices.iter().find(|device| device.l2_mac == mac)?;
            Some((
                remote_hardware_line(device.video.as_deref()),
                video_match_line(
                    &applied.video,
                    &self.local_video,
                    device.video.as_deref(),
                    applied.skip_without_hw,
                ),
                audio_match_line(&applied.audio, &self.local_audio, device.audio.as_deref()),
                remote_backend_line(device.audio.as_deref()),
            ))
        });
        let capture = self.capture_fps();
        let rates = self.global_fps_row("pref-fps", cx);
        let mut tools = div().flex().flex_col().gap_2().w_full().min_w_0();
        for (index, tool) in self.backend_pref.iter().copied().enumerate() {
            let hint = backend_hint(tool, self.backends.has(tool), tool == self.backend);
            tools = tools.child(pref_row(
                SharedString::from(format!("opus-{index}")),
                backend_label(tool),
                hint,
                cx.listener(move |desk, _, _, cx| {
                    swap_earlier(&mut desk.backend_pref, index);
                    desk.apply_backend_order();
                    desk.persist();
                    cx.notify();
                }),
                cx.listener(move |desk, _, _, cx| {
                    swap_later(&mut desk.backend_pref, index);
                    desk.apply_backend_order();
                    desk.persist();
                    cx.notify();
                }),
            ));
        }
        let mut video = div().flex().flex_col().gap_2().w_full().min_w_0();
        for (index, codec) in self.video_pref.iter().copied().enumerate() {
            video = video.child(pref_row(
                SharedString::from(format!("vid-{index}")),
                codec.name(),
                video_hint(codec),
                cx.listener(move |desk, _, _, cx| {
                    swap_earlier(&mut desk.video_pref, index);
                    desk.persist();
                    cx.notify();
                }),
                cx.listener(move |desk, _, _, cx| {
                    swap_later(&mut desk.video_pref, index);
                    desk.persist();
                    cx.notify();
                }),
            ));
        }
        let mut audio = div().flex().flex_col().gap_2().w_full().min_w_0();
        for (index, codec) in self.audio_pref.iter().copied().enumerate() {
            audio = audio.child(pref_row(
                SharedString::from(format!("aud-{index}")),
                codec.name(),
                audio_hint(codec),
                cx.listener(move |desk, _, _, cx| {
                    swap_earlier(&mut desk.audio_pref, index);
                    desk.persist();
                    cx.notify();
                }),
                cx.listener(move |desk, _, _, cx| {
                    swap_later(&mut desk.audio_pref, index);
                    desk.persist();
                    cx.notify();
                }),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .flex_none()
            .child(section_label("Preferences"))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(
                        "This order is the default for every computer. The first row that computer can run is the match. AV1, H.265, and H.264 (HW) need hardware encode. H.264 (SW) is the CPU path. A connection can choose its own picture, audio, and rate. That choice is saved for that computer only.",
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(TEXT))
                    .whitespace_normal()
                    .child(format!("This computer: {local}. Audio: {audio_local}.")),
            )
            .child(section_label("Rate"))
            .child(muted_line(format!(
                "This PC captures at {capture} fps. Auto uses the display rate when the picture this PC sends has a hardware encoder. Without one, or when the display rate cannot be read, Auto is {SOFTWARE_FPS} fps."
            )))
            .child(rates)
            .child(section_label("Opus tools"))
            .child(muted_line(backend_line(self.backends, self.backend)))
            .child(tools)
            .child(muted_line(
                "The first installed tool in this order is announced. Move a row to change the order. The announcement is used the next time this computer shares.".into(),
            ))
            .child(muted_line(if self.pyrowave_library {
                "Pyrowave library: found.".into()
            } else {
                "Pyrowave library: not found.".into()
            }))
            .child(video)
            .child(
                div()
                    .id("skip-without-hw")
                    .flex()
                    .w_full()
                    .min_w_0()
                    .h(px(32.))
                    .px_2()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(if skip_without_hw {
                        PLAYER_BLUE
                    } else {
                        CARD_EDGE
                    }))
                    .bg(rgb(if skip_without_hw { 0x243044 } else { CARD }))
                    .cursor_pointer()
                    .child("Skip when there's no HW encoder")
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.skip_without_hw = !desk.skip_without_hw;
                        desk.persist();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(
                        "A codec without a hardware encoder is skipped. MJPEG stays available. The desktop picture stays MJPEG, and desktop audio stays PCM.",
                    ),
            )
            .child(
                div()
                    .id("trace-input")
                    .flex()
                    .w_full()
                    .min_w_0()
                    .h(px(32.))
                    .px_2()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(if trace_input { PLAYER_BLUE } else { CARD_EDGE }))
                    .bg(rgb(if trace_input { 0x243044 } else { CARD }))
                    .cursor_pointer()
                    .child("Trace input timing")
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.trace_input = !desk.trace_input;
                        desk.persist();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(
                        "The remote confirms each key and click. The picture window shows how long that confirmation took. Turn this on, then connect.",
                    ),
            )
            .child(
                div()
                    .id("capture-traffic")
                    .flex()
                    .w_full()
                    .min_w_0()
                    .h(px(32.))
                    .px_2()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(if capturing { PLAYER_BLUE } else { CARD_EDGE }))
                    .bg(rgb(if capturing { 0x243044 } else { CARD }))
                    .cursor_pointer()
                    .child("Capture traffic")
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.toggle_capture();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(
                        "Writes ether.pcap and app.pcap for Wireshark. ether.pcap is the Ethernet frames. app.pcap is the picture, audio, and input on the session. This stays off until you turn it on, and it is not saved.",
                    ),
            )
            .when_some(connection, |panel, (hardware, choice, audio, tools)| {
                panel
                    .child(section_label("This connection"))
                    .child(muted_line(hardware))
                    .child(muted_line(choice))
                    .child(muted_line(audio))
                    .child(muted_line(tools))
            })
            .child(section_label("Audio"))
            .child(audio)
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.link.reap();
        self.sync_taskbar(window, cx);
        let status = self.status_line();
        let status_color = self.status_color();
        let host = self.host_card(cx);
        let connect = self.connect_card(window, cx);
        let devices = self.device_section(cx);
        let connection = self.connection_panel(cx);
        let prefs_open = self.prefs_open;
        let preferences = prefs_open.then(|| self.preferences(cx));
        let nics = self.nic_row(cx);
        let nic_error = self.nic_error.clone();

        div()
            .id("desk")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .p_4()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("etherquic"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .text_color(rgb(MUTED))
                                    .whitespace_normal()
                                    .child(
                                        "Share this desktop, or open a remote one. The taskbar button is on only while hosting.",
                                    ),
                            ),
                    )
                    .child(prefs_button(
                        prefs_open,
                        cx.listener(|desk, _, _, cx| {
                            desk.prefs_open = !desk.prefs_open;
                            cx.notify();
                        }),
                    )),
            )
            .when_some(preferences, |desk, panel| desk.child(panel))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_color(rgb(status_color))
                    .whitespace_normal()
                    .child(status),
            )
            .child(this_pc_section(&self.share))
            .child(devices)
            .child(connection)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_3()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .child(host)
                    .child(connect),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(section_label("Adapter"))
                    .when_some(nic_error, |row, err| {
                        row.child(
                            div()
                                .w_full()
                                .text_color(rgb(DANGER))
                                .whitespace_normal()
                                .child(err),
                        )
                    })
                    .child(nics),
            )
            .child(lane_row())
    }
}

fn this_pc_section(share: &Share) -> impl IntoElement {
    let mut body = div().flex().flex_col().gap_1().w_full().min_w_0();
    for row in share.rows() {
        body = body.child(identity_row(row.label, row.value));
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .flex_none()
        .child(section_label("This PC"))
        .child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .p_3()
                .bg(rgb(CARD))
                .border_1()
                .border_color(rgb(CARD_EDGE))
                .rounded_md()
                .child(body),
        )
}

fn identity_row(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .w_full()
        .min_w_0()
        .child(
            div()
                .w(px(88.))
                .flex_none()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(div().flex_1().min_w_0().whitespace_normal().child(value))
}

fn phone_share(qr: &Option<QrGrid>) -> impl IntoElement {
    let mut column = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0();
    if let Some(grid) = qr {
        column = column.child(
            div()
                .flex_none()
                .p_2()
                .bg(rgb(0xffffff))
                .rounded_sm()
                .child(qr_picture(grid)),
        );
    }
    column.child(
        div()
            .w_full()
            .min_w_0()
            .text_center()
            .text_color(rgb(MUTED))
            .whitespace_normal()
            .child("Scan to join this PC."),
    )
}

/// Black modules on white. Each module is a whole number of device pixels so a
/// phone camera sees a hard edge instead of a gray blend.
fn qr_picture(grid: &QrGrid) -> impl IntoElement {
    let modules = grid.modules.clone();
    let width = grid.width;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            window.paint_quad(fill(bounds, rgb(0xffffff)));
            if width == 0 {
                return;
            }
            let edge = if bounds.size.width <= bounds.size.height {
                bounds.size.width
            } else {
                bounds.size.height
            };
            let scale = window.scale_factor();
            let scale = if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                1.0
            };
            let raw = (edge.to_f64() as f32) / (width as f32);
            let module = px((raw * scale).floor().max(1.0) / scale);
            let drawn = module * width;
            let ox = bounds.origin.x + (bounds.size.width - drawn) * 0.5;
            let oy = bounds.origin.y + (bounds.size.height - drawn) * 0.5;
            for y in 0..width {
                let row = y * width;
                let mut x = 0;
                while x < width {
                    if !modules[row + x] {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    x += 1;
                    while x < width && modules[row + x] {
                        x += 1;
                    }
                    let rect = Bounds {
                        origin: point(ox + module * start, oy + module * y),
                        size: size(module * (x - start), module),
                    };
                    window.paint_quad(fill(rect, rgb(0x000000)));
                }
            }
        },
    )
    .w(px(240.))
    .h(px(240.))
}

fn card_title(text: &'static str, color: u32) -> impl IntoElement {
    div()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(color))
        .child(text)
}

fn card_body(text: &'static str) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_color(rgb(MUTED))
        .whitespace_normal()
        .line_clamp(3)
        .child(text)
}

fn card_meta(text: String) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_color(rgb(TEXT))
        .whitespace_normal()
        .child(text)
}

fn section_label(text: &'static str) -> impl IntoElement {
    div().text_xs().text_color(rgb(MUTED)).child(text)
}

fn action_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    enabled: bool,
    accent: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .w_full()
        .h(px(34.))
        .px_4()
        .items_center()
        .justify_center()
        .bg(rgb(accent))
        .text_color(rgb(INK))
        .font_weight(FontWeight::BOLD)
        .rounded_sm()
        .when(enabled, |this| this.cursor_pointer())
        .when(!enabled, |this| this.opacity(0.4).cursor_default())
        .active(|this| this.opacity(0.85))
        .child(label)
        .on_click(on_click)
}

fn path_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    choice_chip(id, label, on, 280., on_click)
}

fn nic_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    choice_chip(id, label, on, 168., on_click)
}

fn choice_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    on: bool,
    max_w: f32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .max_w(px(max_w))
        .h(px(28.))
        .px_2()
        .items_center()
        .overflow_hidden()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { CARD }))
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .cursor_pointer()
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label.into()),
        )
        .on_click(on_click)
}

fn refresh_chip(
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("discover-refresh")
        .flex()
        .flex_none()
        .h(px(28.))
        .px_3()
        .items_center()
        .rounded_sm()
        .bg(rgb(PLAYER_BLUE))
        .text_color(rgb(INK))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .child("Refresh")
        .on_click(on_click)
}

fn pref_row(
    id: SharedString,
    name: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    earlier: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    later: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let name = name.into();
    let hint = hint.into();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(order_chip(
            SharedString::from(format!("{id}-up")),
            "Up",
            earlier,
        ))
        .child(order_chip(
            SharedString::from(format!("{id}-down")),
            "Down",
            later,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::BOLD).child(name))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_color(rgb(MUTED))
                        .whitespace_normal()
                        .child(hint),
                ),
        )
}

fn order_chip(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(28.))
        .px_2()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(CARD_EDGE))
        .bg(rgb(CARD))
        .text_color(rgb(TEXT))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

fn prefs_button(
    open: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("prefs-open")
        .flex()
        .flex_none()
        .w(px(34.))
        .h(px(34.))
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if open { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if open { 0x243044 } else { CARD }))
        .cursor_pointer()
        .child(sliders_icon())
        .on_click(on_click)
}

/// Three slider tracks. This opens preferences.
fn sliders_icon() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .justify_between()
        .w(px(16.))
        .h(px(18.))
        .child(slider_track(px(2.)))
        .child(slider_track(px(8.)))
        .child(slider_track(px(5.)))
}

fn slider_track(knob_at: gpui::Pixels) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(6.))
        .child(div().w(knob_at).h(px(2.)).bg(rgb(MUTED)))
        .child(div().flex_none().w(px(4.)).h(px(4.)).bg(rgb(TEXT)))
        .child(div().flex_1().h(px(2.)).bg(rgb(MUTED)))
}

fn device_card(
    id: impl Into<gpui::ElementId>,
    device: &Found,
    selection: &str,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let title = if device.name.trim().is_empty() {
        format_mac(device.l2_mac)
    } else {
        device.name.clone()
    };
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .p_3()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(CARD))
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_weight(FontWeight::BOLD)
                        .child(title),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(HOST_GREEN))
                        .font_weight(FontWeight::BOLD)
                        .child(ms(device.stats.avg_ms)),
                ),
        )
        .child(muted_line(l2_line(device)))
        .child(muted_line(format!("L3 {}", l3_line(device))))
        .child(
            div()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(PLAYER_BLUE))
                .child(selection.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_end()
                .justify_between()
                .gap_3()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(MUTED))
                        .whitespace_normal()
                        .child(format!(
                            "min {} · avg {} · max {} · std {} · jitter {}",
                            ms(device.stats.min_ms),
                            ms(device.stats.avg_ms),
                            ms(device.stats.max_ms),
                            ms(device.stats.std_ms),
                            ms(device.stats.jitter_ms),
                        )),
                )
                .child(sparkline(&device.l2_mac, &device.samples_ms)),
        )
        .on_click(on_click)
}

fn muted_line(text: String) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_color(rgb(MUTED))
        .whitespace_normal()
        .child(text)
}

fn l2_line(device: &Found) -> String {
    let driver = etherquic::discover::l2_backend_label(device.l2_backend);
    if driver.is_empty() {
        format!(
            "L2 {} · service {}",
            format_mac(device.l2_mac),
            device.l2_service
        )
    } else {
        format!(
            "L2 {} · service {} · {driver}",
            format_mac(device.l2_mac),
            device.l2_service
        )
    }
}

fn l3_line(device: &Found) -> String {
    let mut addrs: Vec<String> = device.ipv4.iter().map(|ip| ip.to_string()).collect();
    addrs.extend(device.ipv6.iter().take(2).map(|ip| ip.to_string()));
    let hosts = if addrs.is_empty() {
        "no address".to_string()
    } else {
        addrs.join(", ")
    };
    if device.l3_port == 0 {
        hosts
    } else {
        format!("{hosts} · port {}", device.l3_port)
    }
}

fn ms(value: f32) -> String {
    format!("{value:.1} ms")
}

fn sparkline(mac: &[u8; 6], samples: &[f32]) -> impl IntoElement {
    let peak = samples.iter().copied().fold(0.0, f32::max).max(1.0);
    let key = format_mac(*mac).replace(':', "");
    let mut row = div().flex().flex_row().items_end().flex_none().h(px(22.));
    for (index, sample) in samples.iter().enumerate() {
        let height = 3.0 + 19.0 * (*sample / peak);
        row = row.child(
            div()
                .id(SharedString::from(format!("bar-{key}-{index}")))
                .w(px(5.))
                .h(px(height))
                .bg(rgb(PLAYER_BLUE)),
        );
    }
    row
}

fn nic_label(nic: &NicChoice) -> String {
    let name = nic.name.trim();
    if name.is_empty() {
        nic.description.trim().to_string()
    } else {
        name.to_string()
    }
}

/// Interface index for a link-local IPv6 dial. Auto uses the first shown adapter.
fn ipv6_scope(nic: Option<&str>, nics: &[NicChoice]) -> u32 {
    let name = nic.or_else(|| {
        nics.iter()
            .find(|nic| nic_shown(nic))
            .map(|nic| nic.name.as_str())
    });
    let Some(name) = name else {
        return 0;
    };
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        0
    }
}

/// `any`, loopback, and the netfilter pseudo-devices are not play adapters.
fn nic_shown(nic: &NicChoice) -> bool {
    !matches!(nic.name.trim(), "any" | "lo" | "nflog" | "nfqueue")
}

fn lane_row() -> impl IntoElement {
    let mut row = div().flex().flex_row().gap_2().w_full().min_w_0();
    for lane in lanes() {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .gap_1()
                .p_3()
                .bg(rgb(CARD))
                .rounded_sm()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .items_center()
                        .w_full()
                        .min_w_0()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::BOLD)
                                .child(lane.name),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(rgb(HOST_GREEN))
                                .font_weight(FontWeight::BOLD)
                                .child(format!("{}", lane.priority)),
                        ),
                )
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .truncate()
                        .child(format!("service {}", lane.service)),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .flex_none()
        .child(section_label("Lanes"))
        .child(row)
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_xs()
                .text_color(rgb(MUTED))
                .whitespace_normal()
                .child("A higher priority is sent first."),
        )
}
