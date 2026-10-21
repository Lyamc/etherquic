//! Parsec-style console: host this PC, or sit down as the player.
//!
//! The host card shows a QR code of this PC so a phone can read the address,
//! the service, and the port. The taskbar button is applied from a deferred
//! callback so the Win32 call is not inside the render borrow. It runs only
//! when the desired state changes. Alt-Tab still lists the window. On Windows
//! the notification area keeps the Q from `assets/etherquic.svg` on the
//! taskbar, and swaps to the computer-in-the-Q mark while a player is
//! connected. Closing the window hides it there.

use std::cell::Cell;
use std::collections::HashMap;
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{
    active_layer, apply_override, audio_for, audio_hint, audio_layout_name, audio_match_line,
    backend_hint, backend_label, backend_line, capture_rate, codec_table_text, dial_target,
    format_mac, lanes, list_nics, listed_top, load_connections, load_preferences, local_codec_rows,
    mac_key, qr_grid, remote_backend_line, remote_hardware_line, scale_audio_kbps,
    store_connections, store_preferences, swap_earlier, swap_later, this_pc, top_selection,
    video_hint, video_match_line, audio_override_reason, remote_audio, video_override_reason,
    Applied,
    ConnectionOverride, ConnectionPrefs, ConnectionTabs, OverrideFacts,
    DialTarget, FrameRate, HexMac, Layer, Link, NicChoice, Preferences, QrGrid, Role, Share,
    AUDIO_BITRATE_CHOICES, AUDIO_LAYOUTS, CLIENT_MAC, FPS_CHOICES, HARDWARE_FPS, HOST_MAC,
    SOFTWARE_FPS,
};
use crate::{
    ago, caps_details, caps_summary, decode_thumbnail, load_hosts, matches_filter, scrollbar,
    store_hosts, HostAddr, HostBook, Presence, Roster, Sighting, ThisMachine,
};
use crate::{
    backends as backend_list, chosen_audio, chosen_video, default_audio, default_video,
    load_defaults, load_settings, move_to_front, store_defaults, store_settings, BackendState,
    Field, NetworkDriver, Part, RosterRow, Section, Settings,
};
use crate::{codec_choice_for, last_session, predicted_pair, Predicted};
use etherquic::discover::Found;
use etherquic::negotiate::{HostCaps, YuvChroma, YuvDepth};
use etherquic::{BackendProbe, MediaBackend};
use etherquic_play::{
    choose_codec, display_hz, this_computer, AudioId, AudioMachine, MachineCodecs, PicturePref,
};
use etherquic_play::{CursorMode, Dither, FrameGen, Look, ScaleMode, SizeMatch, Upscale, PAD_SLOTS};

use crate::badge::os_badge;
use crate::extra_keys::{self, ExtraKey, ExtraMods, Tap};
use crate::overlay::{self, Dir, OverlayItem, OverlayRuntime, Side};
use crate::scan::Scan;
use crate::update::{self, Status as UpdateStatus};
use gpui::{
    anchored, canvas, deferred, div, fill, img, point, prelude::*, px, relative, rgb, size,
    AnyElement, AnyView, App, AppContext, Application, Bounds, ClickEvent, ClipboardItem, Context,
    Div, FocusHandle, FontWeight, ImageSource, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, RenderImage, ScrollHandle, ScrollWheelEvent,
    SharedString, Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions,
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
/// Room kept free on the right of a scrolling area for its scrollbar, shown or not.
const BAR_W: f32 = 12.0;
/// Tallest the host list grows before it scrolls on its own: two and a half
/// rows, so a cut-off row shows there is more.
const HOST_LIST_MAX_H: f32 = 2.5 * HOST_ROW_H + 2. * 4.;
/// One compact host row.
/// Room for the widest set of row buttons, so a delete confirmation does not
/// move the status column.
const ACTIONS_W: f32 = 236.;
/// Discovery thumbnails show at 16:9 in this box.
const THUMB_W: f32 = 64.;
const THUMB_H: f32 = 36.;
/// Operating-system mark beside the thumbnail.
const BADGE_W: f32 = 34.;
const LEAD_W: f32 = BADGE_W + 6. + THUMB_W;
const HOST_ROW_H: f32 = 64.0;
/// Two lines of status. Longer text is clipped so the page under it never moves.
const STATUS_H: f32 = 40.0;
/// Sound devices listed before "Show all devices".
const DEVICES_SHOWN: usize = 4;

const USAGE: &str = "\
etherquic [--console] [--tray]
etherquic host [--nic IFACE] [--console]
etherquic connect --peer-mac MAC [--nic IFACE] [--console]
etherquic connect --udp IP:PORT [--console]

host shares this machine. connect joins one.
--console opens a console. A launch from a terminal uses that terminal.
--tray keeps the window in the notification area at launch.
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
    /// `connect --udp IP:PORT` dials L3 instead of the L2 peer.
    udp: Option<std::net::SocketAddr>,
    /// Start with the window hidden; the notification-area icon stays.
    tray: bool,
}

impl Launch {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut tray = false;
        let args: Vec<String> = args
            .filter(|arg| {
                if arg == "--console" {
                    false
                } else if arg == "--tray" {
                    tray = true;
                    false
                } else {
                    true
                }
            })
            .collect();
        if args.is_empty() {
            return Ok(Self {
                command: Command::Window,
                peer: HexMac::default(),
                nic: None,
                udp: None,
                tray,
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
        let mut udp = None;
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
                "--udp" => {
                    i += 1;
                    let value = args.get(i).ok_or("--udp needs IP:PORT")?;
                    udp = Some(
                        value
                            .parse::<std::net::SocketAddr>()
                            .map_err(|_| format!("--udp needs IP:PORT, got {value}"))?,
                    );
                }
                other => return Err(format!("unknown argument {other}\n{USAGE}")),
            }
            i += 1;
        }
        if command == Command::Host && (peer.is_some() || udp.is_some()) {
            return Err(format!("host does not take --peer-mac or --udp\n{USAGE}"));
        }
        if command == Command::Connect && peer.is_none() && udp.is_none() {
            return Err(format!("connect needs --peer-mac or --udp\n{USAGE}"));
        }
        Ok(Self {
            command,
            // The Connect field stays empty until --peer-mac or a picked host.
            peer: peer.unwrap_or_default(),
            nic,
            udp,
            tray,
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

/// Put the window in the notification area once the platform can host an icon.
fn install_tray(window: &mut Window, cx: &mut Context<Desk>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    let hwnd = crate::taskbar::hwnd_of(window).unwrap_or(0);
    if cfg!(windows) && hwnd == 0 {
        return;
    }
    if !crate::tray::install(hwnd) {
        DONE.store(true, Ordering::Relaxed);
        return;
    }
    DONE.store(true, Ordering::Relaxed);
    window.on_window_should_close(cx, |_window, _cx| {
        if crate::tray::quit_requested() || !crate::tray::installed() {
            true
        } else {
            crate::tray::hide_main();
            false
        }
    });
    // A minimized window may stop drawing, so Open/Quit cannot wait on render.
    let handle = window.window_handle();
    cx.spawn(async move |_desk, cx| loop {
        gpui::Timer::after(Duration::from_millis(80)).await;
        if crate::tray::take_quit() {
            crate::tray::remove();
            if cx.update(|app| app.quit()).is_err() {
                break;
            }
            break;
        }
        if crate::tray::take_show() {
            let _ = cx.update(|app| {
                let _ = handle.update(app, |_view, window, _app| {
                    window.activate_window();
                });
            });
        }
        if crate::tray::take_hide() {
            let _ = cx.update(|app| {
                let _ = handle.update(app, |_view, window, _app| {
                    window.minimize_window();
                });
            });
        }
    })
    .detach();
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

        let screen = screen_of(cx);
        let fit = crate::place::fit(screen);
        let open = fit.opening;
        let outer = fit.outer;
        let launch = launch.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(open.x), px(open.y)),
                    size: size(px(open.w), px(open.h)),
                })),
                titlebar: Some(TitlebarOptions {
                    title: Some("etherquic".into()),
                    ..Default::default()
                }),
                app_id: Some("etherquic".into()),
                window_min_size: Some(size(px(open.min_w), px(open.min_h))),
                ..Default::default()
            },
            move |window, cx| {
                if let Some(outer) = outer {
                    if let Some(hwnd) = crate::taskbar::hwnd_of(window) {
                        // After this builder returns, the window is stored and
                        // this snap can update the viewport.
                        cx.defer(move |_cx| crate::place::snap_outer(hwnd, outer));
                    }
                }
                let launch = launch.clone();
                cx.new(|cx| Desk::new(cx, launch))
            },
        )
        .expect("open etherquic window");
        cx.activate(true);
    });
}

/// The display the window should fill. Wayland's primary output is the one
/// at the origin; if that is still unknown, the largest output is used.
fn screen_of(cx: &App) -> Option<crate::place::RectF> {
    let mut best = cx.primary_display();
    if best.is_none() {
        let mut area = 0.0_f32;
        for display in cx.displays() {
            let bounds = display.bounds();
            let next = f32::from(bounds.size.width) * f32::from(bounds.size.height);
            if next > area {
                area = next;
                best = Some(display);
            }
        }
    }
    best.map(|display| crate::place::from_bounds(display.bounds()))
}

struct Desk {
    link: Link,
    /// Every connection the window is keeping. `link` is the current one.
    tabs: ConnectionTabs,
    /// Connections that are open and not current. Their sessions keep running.
    parked: HashMap<u64, Link>,
    /// Sessions told to leave. Reaped after the worker exits, not on the click.
    ending: Vec<Link>,
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
    /// The opening size has been checked against a real display. Wayland
    /// reports outputs only after a roundtrip, so the first poll grows a
    /// window that still has the fallback size.
    display_fit: bool,
    share_text: String,
    share: Share,
    qr: Option<QrGrid>,
    /// This process: etherquic version and the operating system. Read once.
    ours: etherquic::discover::Release,
    /// Global order. The first codec a discovered computer can run is the match.
    video_pref: Vec<PicturePref>,
    /// Rows switched off in Settings. A pin still uses them.
    video_off: Vec<PicturePref>,
    audio_pref: Vec<AudioId>,
    /// Skip a codec unless the sender has a hardware encoder. MJPEG stays available.
    skip_without_hw: bool,
    /// Ask the remote to confirm input and show how long that took.
    trace_input: bool,
    /// Draw input, picture, audio, and transfer on the picture.
    latency_graph: bool,
    /// Keep those samples in the temp directory for a day.
    latency_log: bool,
    /// Default rate, and the rate this PC captures at while hosting.
    fps: FrameRate,
    /// Stereo-equivalent audio bitrate. Zero is Auto (96 kbps stereo).
    audio_bitrate_kbps: u32,
    /// Max audio channels. Zero is Auto (2-channel stereo).
    audio_channels: u16,
    /// Display refresh, when it can be read. This is the server rate for Auto.
    display_hz: Option<u32>,
    /// Per-computer pins. Missing fields use the defaults above.
    connections: ConnectionPrefs,
    /// Packet capture. Not written to the preferences file.
    capturing: bool,
    /// Hide the window once the notification-area icon is up (`--tray`).
    start_in_tray: bool,
    /// macOS: hide the host-service prompt for this session only.
    host_service_hidden: bool,
    /// Collapsible sections. All start closed.
    open: Panels,
    /// The page and the host list scroll separately.
    page_scroll: ScrollHandle,
    list_scroll: ScrollHandle,
    drag: Option<Drag>,
    /// Scrollbar inputs seen last frame. A change asks for one more frame.
    bar_sig: [f32; 4],
    book: HostBook,
    roster: Roster,
    /// This computer. Its own service is left out of the host list.
    here: ThisMachine,
    roster_burst: u64,
    tab: HostTab,
    filter: String,
    filter_focus: FocusHandle,
    add_open: bool,
    add_name: String,
    add_addr: String,
    add_error: Option<String>,
    add_name_focus: FocusHandle,
    add_addr_focus: FocusHandle,
    /// Index into `book.saved` and the name being typed.
    renaming: Option<(usize, String)>,
    rename_focus: FocusHandle,
    /// Index into `book.saved` waiting for Delete to be confirmed.
    confirm_delete: Option<usize>,
    /// An IP host picked from Saved or Recent. `None` dials `peer` and `layer`.
    direct: Option<HostAddr>,
    /// Name of the picked host, for Recent.
    target_name: Option<String>,
    /// Goes to Recent once the view reports it is up.
    pending_recent: Option<(String, HostAddr)>,
    /// Decoded discovery thumbnails by host key, with the codestream they came from.
    thumbs: HashMap<[u8; 6], (u64, Option<Arc<RenderImage>>)>,
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
    /// Settings that are not codec order. Saved in `settings.txt` and pushed
    /// to the open view as they change.
    settings: Settings,
    /// Picked in the Connect card and not saved yet.
    session_video: Option<PicturePref>,
    session_audio: Option<AudioId>,
    /// The dropdown that is open. It draws over the page and moves nothing.
    menu: Option<Menu>,
    /// A Reset waiting for its confirmation. `Some(None)` is Reset all.
    confirm_reset: Option<Option<Section>>,
    /// The user's defaults: what Reset returns to and what "default" marks.
    defaults: Settings,
    local_outputs: Vec<String>,
    local_inputs: Vec<String>,
    local_pads: Vec<String>,
    devices_loading: bool,
    /// Window y of the Settings heading from the last frame.
    settings_y: Rc<Cell<f32>>,
    /// Frames left before scrolling to Settings.
    scroll_to_settings: u8,
    /// Left edge and width of the quality slider while the pointer is down.
    quality_drag: Option<(f32, f32)>,
    /// Last quality-preview encode. Rebuilt when quality, colour, codec, or rate changes.
    preview_key: Option<crate::quality_preview::Key>,
    preview_image: Option<Arc<RenderImage>>,
    preview_line: String,
    preview_rates: Vec<(String, u32)>,
    preview_anchors: Vec<(u32, u32, u32)>,
    /// Phone IME is requested open. Extra-keys sit above it while connected.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    ime_open: bool,
    extra_mods: ExtraMods,
    overlay_run: OverlayRuntime,
    update: UpdateStatus,
    /// Last Copy chip pressed, so the label can read "Copied" for a moment.
    copied: Option<(String, Instant)>,
    /// Adapters this desk discovers and hosts on. Default is every real and
    /// virtual link.
    listen: crate::desk_model::ListenSet,
    layout: crate::desk_model::TabLayout,
    outbound: Vec<crate::desk_model::Outbound>,
    /// Left-clicked session whose live settings are on screen.
    inspect: Option<u64>,
    quick: bool,
    trace_log: crate::desk_model::TraceLog,
    session_live: HashMap<u64, SessionLive>,
    host_face_at: Instant,
    host_summary: String,
    service_guests: Vec<String>,
    service_running: bool,
    service_hosting: bool,
    shelves: Vec<u64>,
}

struct SessionLive {
    view: etherquic_play::LiveView,
    tune: etherquic_play::LiveTune,
    tx: std::sync::mpsc::Sender<String>,
    peer: [u8; 6],
    udp: Option<std::net::SocketAddr>,
}

impl Desk {
    fn new(cx: &mut Context<Self>, launch: Launch) -> Self {
        // Codec probe in the background, so Auto can name the real pick.
        etherquic_play::etherquic_media::caps::warm();
        let (nics, nic_error) = match list_nics() {
            Ok(nics) => (nics, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        let backends = etherquic::probe_backends();
        let saved = load_preferences();
        let backend = etherquic::pick_backend(&saved.opus, &backends);
        let mut desk = Self {
            link: Link::idle(),
            tabs: ConnectionTabs::new(),
            parked: HashMap::new(),
            ending: Vec::new(),
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
            display_fit: false,
            share_text: String::new(),
            share: Share::blank(),
            qr: None,
            ours: etherquic::discover::local_release(),
            video_pref: saved.video,
            video_off: saved.video_off,
            audio_pref: saved.audio,
            skip_without_hw: saved.skip_without_hw,
            trace_input: saved.trace_input,
            latency_graph: saved.latency_graph,
            latency_log: saved.latency_log,
            fps: saved.fps,
            audio_bitrate_kbps: saved.audio_bitrate_kbps,
            audio_channels: saved.audio_channels,
            display_hz: display_hz(),
            connections: load_connections(),
            capturing: etherquic::capture_active(),
            start_in_tray: launch.tray,
            host_service_hidden: false,
            open: Panels::default(),
            page_scroll: ScrollHandle::new(),
            list_scroll: ScrollHandle::new(),
            drag: None,
            bar_sig: [0.0; 4],
            tab: HostTab::Discovered,
            book: load_hosts(),
            roster: Roster::default(),
            here: ThisMachine::detect(),
            roster_burst: 0,
            filter: String::new(),
            filter_focus: cx.focus_handle(),
            add_open: false,
            add_name: String::new(),
            add_addr: String::new(),
            add_error: None,
            add_name_focus: cx.focus_handle(),
            add_addr_focus: cx.focus_handle(),
            renaming: None,
            rename_focus: cx.focus_handle(),
            confirm_delete: None,
            direct: None,
            target_name: None,
            pending_recent: None,
            thumbs: HashMap::new(),
            local_video: this_computer(),
            local_audio: audio_for(backends, backend),
            backends,
            backend_pref: saved.opus,
            backend,
            pyrowave_library: etherquic::pyrowave_library_present(),
            settings: load_settings(),
            defaults: load_defaults(),
            session_video: None,
            session_audio: None,
            menu: None,
            confirm_reset: None,
            local_outputs: Vec::new(),
            local_inputs: Vec::new(),
            local_pads: Vec::new(),
            devices_loading: false,
            settings_y: Rc::new(Cell::new(0.)),
            scroll_to_settings: 0,
            quality_drag: None,
            preview_key: None,
            preview_image: None,
            preview_line: String::new(),
            preview_rates: Vec::new(),
            preview_anchors: Vec::new(),
            ime_open: false,
            extra_mods: ExtraMods::default(),
            overlay_run: OverlayRuntime::default(),
            update: UpdateStatus::Idle,
            copied: None,
            listen: listen_set(),
            layout: crate::desk_model::TabLayout::new(),
            outbound: Vec::new(),
            inspect: None,
            quick: false,
            trace_log: crate::desk_model::TraceLog::default(),
            session_live: HashMap::new(),
            host_face_at: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            host_summary: "Not hosting".into(),
            service_guests: Vec::new(),
            service_running: false,
            service_hosting: false,
            shelves: Vec::new(),
        };
        etherquic_play::set_graph(desk.latency_graph);
        etherquic_play::set_log(desk.latency_log);
        if let Some(name) = desk.nic.clone() {
            if !desk.listen.select_only(&name) {
                desk.nic = None;
            }
        }
        desk.refresh_devices(cx);
        desk.apply_settings(false);
        desk.refresh_share();
        if desk
            .book
            .saved
            .iter()
            .any(|entry| !desk.here.owns_addr(&entry.addr))
        {
            desk.tab = HostTab::Saved;
        }
        desk.message = match launch.command {
            Command::Host => {
                let fps = desk.fps.wish();
                let nic = desk.nic.clone();
                let backend = desk.backend;
                let id = desk.begin_tab("This computer".into());
                match desk.link.host(nic, backend, fps) {
                    Ok(()) => None,
                    Err(err) => {
                        desk.abort_tab(id);
                        Some(err)
                    }
                }
            }
            Command::Connect => match (launch.udp, desk.peer.parse()) {
                (Some(addr), peer) => {
                    let peer = peer.unwrap_or(HOST_MAC);
                    desk.direct = Some(HostAddr::Ip {
                        ip: addr.ip(),
                        port: addr.port(),
                    });
                    desk.pending_recent = Some((
                        addr.to_string(),
                        HostAddr::Ip {
                            ip: addr.ip(),
                            port: addr.port(),
                        },
                    ));
                    let trace = desk.applied(peer).trace_input;
                    let codecs = desk.codec_offer(Some(peer));
                    let nic = desk.nic.clone();
                    let id = desk.begin_tab(addr.to_string());
                    desk.stage_session(id, peer, Some(addr));
                    match desk
                        .link
                        .connect(nic, peer, trace, 0, Some(addr), codecs, false, None)
                    {
                        Ok(()) => None,
                        Err(err) => {
                            desk.abort_tab(id);
                            Some(err)
                        }
                    }
                }
                (None, Some(peer)) => {
                    desk.pending_recent = Some((format_mac(peer), HostAddr::Mac(peer)));
                    let trace = desk.applied(peer).trace_input;
                    let codecs = desk.codec_offer(Some(peer));
                    let nic = desk.nic.clone();
                    let id = desk.begin_tab(format_mac(peer));
                    desk.stage_session(id, peer, None);
                    let device = desk
                        .scan
                        .board()
                        .devices
                        .into_iter()
                        .find(|device| device.l2_mac == peer);
                    let (l2_offer, rtp) = desk.carrier_dial(None, device.as_ref());
                    match desk
                        .link
                        .connect(nic, peer, trace, 0, None, codecs, l2_offer, rtp)
                    {
                        Ok(()) => None,
                        Err(err) => {
                            desk.abort_tab(id);
                            Some(err)
                        }
                    }
                }
                (None, None) => Some("Peer address needs 12 hex digits.".into()),
            },
            Command::Window => None,
        };
        if desk.message.is_none() {
            if let Some(dir) = etherquic::capture_dir() {
                desk.message = Some(format!("Capturing to {}", dir.display()));
            }
        }
        desk.start_poll(cx);
        desk.watch_pastes(cx);
        desk
    }

    fn watch_pastes(&self, cx: &mut Context<Self>) {
        etherquic_play::watch_text_paste();
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(40)).await;
            if etherquic_play::take_text_paste_requests() == 0 {
                continue;
            }
            if this
                .update(cx, |desk, cx| {
                    desk.paste_local_clipboard(cx);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    /// Send this computer's clipboard text into the open view.
    /// The keystroke switch types it. Otherwise it goes out as clipboard text.
    fn paste_local_clipboard(&mut self, cx: &App) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        if text.is_empty() {
            self.message = Some("Clipboard has no text.".into());
            return;
        }
        if let Err(err) =
            etherquic_play::send_text_paste(&text, self.settings.clipboard_as_keystrokes)
        {
            self.message = Some(format!("Paste did not send: {err}"));
        }
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(400)).await;
            if this
                .update(cx, |desk, cx| {
                    desk.reap_links();
                    desk.refresh_share();
                    desk.fit_display(cx);
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

    fn host_peer_connected(&self) -> bool {
        let live = |link: &Link| link.role() == Role::Host && link.peers() > 0;
        live(&self.link) || self.parked.values().any(live)
    }

    fn sync_icons(&self) {
        crate::tray::set_face(if self.host_peer_connected() {
            crate::tray::Face::Hosting
        } else {
            crate::tray::Face::Idle
        });
    }

    fn reap_links(&mut self) {
        self.link.reap();
        for link in self.parked.values_mut() {
            link.reap();
        }
        self.ending.retain_mut(|link| {
            link.reap();
            link.running()
        });
    }

    /// Park the current session and open a new current tab. The parked
    /// session keeps running.
    fn begin_tab(&mut self, label: String) -> u64 {
        if let Some(id) = self.tabs.active_id() {
            let old = std::mem::replace(&mut self.link, Link::idle());
            self.parked.insert(id, old);
        }
        let id = self.tabs.open(label);
        self.show_picture();
        id
    }

    fn show_picture(&self) {
        etherquic_play::switch_picture(self.link.picture_id());
    }

    /// The new tab did not start. Put the previous session back.
    fn abort_tab(&mut self, id: u64) {
        self.drop_session(id);
        let _ = self.tabs.close(id);
        self.link = Link::idle();
        if let Some(prev) = self.tabs.active_id() {
            if let Some(link) = self.parked.remove(&prev) {
                self.link = link;
            }
        }
        self.show_picture();
    }

    fn switch_connection(&mut self, id: u64) {
        let Some(current) = self.tabs.active_id() else {
            return;
        };
        if current == id || !self.tabs.switch(id) {
            return;
        }
        let incoming = self.parked.remove(&id).unwrap_or_else(Link::idle);
        let outgoing = std::mem::replace(&mut self.link, incoming);
        self.parked.insert(current, outgoing);
        self.show_picture();
    }

    fn close_connection(&mut self, id: u64) {
        let active = self.tabs.active_id();
        if !self.tabs.close(id) {
            return;
        }
        self.drop_session(id);
        if active == Some(id) {
            self.link.release();
            let closing = std::mem::replace(&mut self.link, Link::idle());
            self.ending.push(closing);
            if let Some(next) = self.tabs.active_id() {
                if let Some(link) = self.parked.remove(&next) {
                    self.link = link;
                }
            }
        } else if let Some(mut link) = self.parked.remove(&id) {
            link.release();
            self.ending.push(link);
        }
        self.show_picture();
        self.message = None;
        if self.link.role() != Role::Client {
            self.hide_ime();
            self.release_overlay();
        }
    }

    fn show_host_service_prompt(&self) -> bool {
        cfg!(target_os = "macos")
            && !self.host_service_hidden
            && !self.settings.skip_host_service_prompt
            && !crate::macos_service::service_installed()
    }

    fn install_host_service(&mut self, cx: &mut Context<Self>) {
        match crate::macos_service::install_with_prompt() {
            Ok(()) => {
                self.host_service_hidden = true;
                self.message = Some("Host service installed.".into());
            }
            Err(err) => self.message = Some(err),
        }
        cx.notify();
    }

    fn remove_host_service(&mut self, cx: &mut Context<Self>) {
        match crate::macos_service::uninstall_with_prompt() {
            Ok(()) => {
                self.message = Some("Host service removed. Saved settings were kept.".into());
            }
            Err(err) => self.message = Some(err),
        }
        cx.notify();
    }

    fn on_host(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.reap_links();
        if self.link.role() == Role::Host {
            if let Some(id) = self.tabs.active_id() {
                self.close_connection(id);
            } else {
                self.link.stop();
            }
            self.message = None;
            cx.notify();
            return;
        }
        let id = self.begin_tab("This computer".into());
        match self
            .link
            .host(self.nic.clone(), self.backend, self.fps.wish())
        {
            Ok(()) => self.message = None,
            Err(err) => {
                self.abort_tab(id);
                self.message = Some(err);
            }
        }
        cx.notify();
    }

    fn on_connect(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.reap_links();
        if self.link.role() == Role::Client {
            if let Some(id) = self.tabs.active_id() {
                self.close_connection(id);
            } else {
                self.link.stop();
            }
            self.hide_ime();
            self.release_overlay();
            self.message = None;
            cx.notify();
            return;
        }
        self.open_typed_connection(cx);
    }

    fn hide_ime(&mut self) {
        self.ime_open = false;
        crate::link::set_soft_keyboard(false);
    }

    fn toggle_ime(&mut self) {
        self.ime_open = !self.ime_open;
        crate::link::set_soft_keyboard(self.ime_open);
    }

    fn on_extra_key(&mut self, key: ExtraKey, cx: &mut Context<Self>) {
        match extra_keys::tap(key, &mut self.extra_mods) {
            Tap::ToggleIme => self.toggle_ime(),
            Tap::Keys(keys) => {
                crate::link::send_player_keys(&keys);
            }
        }
        cx.notify();
    }

    fn forward_session_key(&mut self, ev: &KeyDownEvent) -> bool {
        if self.link.role() != Role::Client {
            return false;
        }
        let keys = extra_keys::type_named(
            &ev.keystroke.key,
            ev.keystroke.modifiers.control,
            ev.keystroke.modifiers.alt,
            ev.keystroke.modifiers.shift,
            &mut self.extra_mods,
        );
        if keys.is_empty() {
            return false;
        }
        crate::link::send_player_keys(&keys)
    }

    fn extra_keys_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mods = self.extra_mods;
        let ime_open = self.ime_open;
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .bg(rgb(0x1a1e27))
            .border_t_1()
            .border_color(rgb(CARD_EDGE))
            .px_1()
            .py_1()
            .gap_1()
            .children(extra_keys::default_rows().iter().map(|row| {
                let keys = *row;
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .gap_1()
                    .children(keys.iter().copied().map(|key| {
                        let on = match key {
                            ExtraKey::Keyboard => ime_open,
                            other => mods.lit(other),
                        };
                        extra_key_chip(
                            key,
                            on,
                            cx.listener(move |desk, _, _, cx| {
                                desk.on_extra_key(key, cx);
                            }),
                        )
                    }))
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .gap_1()
                    .child(extra_session_chip(
                        "ek-controls",
                        if self.settings.overlay.visible {
                            "HIDE PAD"
                        } else {
                            "CONTROLS"
                        },
                        self.settings.overlay.visible,
                        cx.listener(|desk, _, _, cx| {
                            desk.toggle_overlay();
                            cx.notify();
                        }),
                    ))
                    .child(extra_session_chip(
                        "ek-edit-pad",
                        if self.overlay_run.edit {
                            "DONE"
                        } else {
                            "EDIT"
                        },
                        self.overlay_run.edit,
                        cx.listener(|desk, _, _, cx| {
                            desk.toggle_overlay_edit();
                            cx.notify();
                        }),
                    )),
            )
    }

    fn overlay_active(&self) -> bool {
        self.link.role() == Role::Client && (self.settings.overlay.visible || self.overlay_run.edit)
    }

    fn sync_touch_pointer(&self) {
        crate::link::set_touch_pointer(self.overlay_active());
    }

    fn release_overlay(&mut self) {
        let events = overlay::release_all(&mut self.overlay_run);
        crate::link::send_player_events(events);
        self.overlay_run.edit = false;
        crate::link::set_touch_pointer(false);
    }

    fn toggle_overlay(&mut self) {
        let show = !self.settings.overlay.visible;
        self.edit_settings(|settings| settings.overlay.visible = show);
        if show && self.settings.overlay.is_empty() {
            self.overlay_run.edit = true;
        }
        if !show {
            self.overlay_run.edit = false;
            let events = overlay::release_all(&mut self.overlay_run);
            crate::link::send_player_events(events);
        }
        self.sync_touch_pointer();
    }

    fn toggle_overlay_edit(&mut self) {
        self.overlay_run.edit = !self.overlay_run.edit;
        if self.overlay_run.edit {
            self.edit_settings(|settings| settings.overlay.visible = true);
        }
        self.sync_touch_pointer();
    }

    fn overlay_add(&mut self, item: OverlayItem) {
        let side = self.overlay_run.add_side;
        self.edit_settings(|settings| settings.overlay.add(side, item));
        self.sync_touch_pointer();
    }

    fn overlay_remove(&mut self, side: Side, index: usize) {
        self.edit_settings(|settings| settings.overlay.remove(side, index));
    }

    fn overlay_preset(&mut self, layout: overlay::OverlayLayout) {
        self.edit_settings(|settings| {
            settings.overlay = layout;
            settings.overlay.visible = true;
        });
        self.overlay_run.edit = true;
        self.sync_touch_pointer();
    }

    fn emit_overlay(&mut self, events: Vec<etherquic_input::Event>, cx: &mut Context<Self>) {
        crate::link::send_player_events(events);
        cx.notify();
    }

    fn overlay_hold(&mut self, item: OverlayItem, down: bool, cx: &mut Context<Self>) {
        let events = overlay::hold(item, down, &mut self.overlay_run);
        self.emit_overlay(events, cx);
    }

    fn overlay_dir(&mut self, item: OverlayItem, dir: Dir, down: bool, cx: &mut Context<Self>) {
        let events = overlay::hold_dir(item, dir, down, &mut self.overlay_run);
        self.emit_overlay(events, cx);
    }

    fn overlay_stick_down(&mut self, side: Side, index: usize, ev: &MouseDownEvent) {
        overlay::stick_down(
            side,
            index,
            f32::from(ev.position.x),
            f32::from(ev.position.y),
            &mut self.overlay_run,
        );
    }

    fn overlay_stick_drag(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(touch) = self.overlay_run.stick else {
            return;
        };
        let Some(item) = self
            .settings
            .overlay
            .column(touch.side)
            .get(touch.index)
            .copied()
        else {
            return;
        };
        let events = overlay::stick_move(
            f32::from(ev.position.x),
            f32::from(ev.position.y),
            item,
            &mut self.overlay_run,
        );
        self.emit_overlay(events, cx);
    }

    fn overlay_stick_up(&mut self, cx: &mut Context<Self>) {
        let Some(touch) = self.overlay_run.stick else {
            return;
        };
        let Some(item) = self
            .settings
            .overlay
            .column(touch.side)
            .get(touch.index)
            .copied()
        else {
            self.overlay_run.stick = None;
            return;
        };
        let events = overlay::stick_up(item, &mut self.overlay_run);
        self.emit_overlay(events, cx);
    }

    fn overlay_layer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let edit = self.overlay_run.edit;
        let add_side = self.overlay_run.add_side;
        let layout = self.settings.overlay.clone();
        let stick = self.overlay_run.stick;
        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .justify_end()
            .when(edit, |layer| layer.child(self.overlay_editor(add_side, cx)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .justify_between()
                    .w_full()
                    .p_2()
                    .gap_2()
                    .child(overlay_column(Side::Left, &layout.left, edit, stick, cx))
                    .child(overlay_column(
                        Side::Middle,
                        &layout.middle,
                        edit,
                        stick,
                        cx,
                    ))
                    .child(overlay_column(Side::Right, &layout.right, edit, stick, cx)),
            )
    }

    fn overlay_editor(&mut self, add_side: Side, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .p_2()
            .bg(rgb(0x12141a))
            .border_b_1()
            .border_color(rgb(CARD_EDGE))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .child(small_chip(
                        "ov-game",
                        "Game pad",
                        None,
                        true,
                        cx.listener(|desk, _, _, cx| {
                            desk.overlay_preset(overlay::preset_game());
                            cx.notify();
                        }),
                    ))
                    .child(small_chip(
                        "ov-arrows",
                        "WASD + arrows",
                        None,
                        true,
                        cx.listener(|desk, _, _, cx| {
                            desk.overlay_preset(overlay::preset_arrows());
                            cx.notify();
                        }),
                    ))
                    .child(small_chip(
                        "ov-clear",
                        "Clear",
                        None,
                        true,
                        cx.listener(|desk, _, _, cx| {
                            desk.overlay_preset(overlay::OverlayLayout {
                                visible: true,
                                ..overlay::OverlayLayout::default()
                            });
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .children(Side::ALL.iter().copied().map(|side| {
                        small_chip(
                            SharedString::from(format!("ov-side-{}", side.token())),
                            side.label(),
                            (add_side == side).then_some(PLAYER_BLUE),
                            true,
                            cx.listener(move |desk, _, _, cx| {
                                desk.overlay_run.add_side = side;
                                cx.notify();
                            }),
                        )
                    })),
            )
            .child(muted_line(
                "Pick a column, then tap a control to add it. Sticks, d-pads, and arrow clusters go on the left or right.".into(),
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .children(overlay::catalog().iter().copied().map(|item| {
                        let label = item.label();
                        div()
                            .id(SharedString::from(format!("ov-add-{}", item.token())))
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
                            .on_click(cx.listener(move |desk, _, _, cx| {
                                desk.overlay_add(item);
                                cx.notify();
                            }))
                    })),
            )
    }

    /// Carriers 3–5 on an L2 dial use the scanned host IP and RTP port.
    fn carrier_dial(
        &self,
        udp: Option<std::net::SocketAddr>,
        device: Option<&Found>,
    ) -> (bool, Option<std::net::SocketAddr>) {
        let (ip, rtp_port, quic_port) = match device {
            Some(device) => (
                device
                    .ipv4
                    .first()
                    .copied()
                    .map(std::net::IpAddr::V4)
                    .or_else(|| device.ipv6.first().copied().map(std::net::IpAddr::V6)),
                device.rtp_port,
                device.l3_port,
            ),
            None => (None, 0, 0),
        };
        etherquic::carrier::l2_media_target(self.selected_carrier(), udp, ip, rtp_port, quic_port)
    }

    /// Start another player connection. A host session that is already open
    /// stays open on its own tab.
    fn open_typed_connection(&mut self, cx: &mut Context<Self>) {
        if let Some(HostAddr::Ip { ip, port }) = self.direct {
            self.connect_direct(ip, port, cx);
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
                self.message =
                    Some("That IPv6 address is link-local. Pick the adapter, not Auto.".into());
                cx.notify();
                return;
            }
        };
        let (l2_offer, rtp) = self.carrier_dial(udp, device.as_ref());
        // Carriers 1 and 2 still need the raw dial. Carriers 3–5 offer on the
        // scanned RTP socket and do not open a QUIC or Ethernet socket.
        if udp.is_none() && !l2_offer {
            if let Some(why) = etherquic::l2_unavailable_reason() {
                self.message = Some(why.into());
                cx.notify();
                return;
            }
        }
        let used = match layer {
            Layer::L2 => HostAddr::Mac(peer),
            Layer::V4(ip) => HostAddr::Ip {
                ip: ip.into(),
                port,
            },
            Layer::V6(ip) => HostAddr::Ip {
                ip: ip.into(),
                port,
            },
        };
        let name = self.target_name.clone().unwrap_or_else(|| {
            device
                .as_ref()
                .map(|device| device.name.trim().to_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format_mac(peer))
        });
        let codecs = self.codec_offer(Some(peer));
        let id = self.begin_tab(name.clone());
        self.stage_session(id, peer, udp);
        match self
            .link
            .connect(
                self.nic.clone(),
                peer,
                trace,
                host_l2,
                udp,
                codecs,
                l2_offer,
                rtp,
            )
        {
            Ok(()) => {
                self.message = None;
                self.pending_recent = Some((name, used));
            }
            Err(err) => {
                self.abort_tab(id);
                self.message = Some(err);
            }
        }
        cx.notify();
    }

    /// An IP host from Saved or Recent. It need not be discovered first.
    fn connect_direct(&mut self, ip: std::net::IpAddr, port: u16, cx: &mut Context<Self>) {
        let addr = HostAddr::Ip { ip, port };
        let known = self.roster.find(&addr).cloned();
        let port = if port == 0 {
            known.as_ref().map(|row| row.port).unwrap_or(0)
        } else {
            port
        };
        let layer = match ip {
            std::net::IpAddr::V4(ip) => Layer::V4(ip),
            std::net::IpAddr::V6(ip) => Layer::V6(ip),
        };
        let udp = match dial_target(layer, port, ipv6_scope(self.nic.as_deref(), &self.nics)) {
            DialTarget::Udp(addr) => Some(addr),
            DialTarget::L2 => None,
            DialTarget::NeedsNic => {
                self.message =
                    Some("That IPv6 address is link-local. Pick the adapter, not Auto.".into());
                cx.notify();
                return;
            }
        };
        let peer = known
            .as_ref()
            .map(|row| row.mac)
            .or_else(|| self.peer.parse())
            .unwrap_or([0; 6]);
        let trace = self.applied(peer).trace_input;
        let name = self
            .target_name
            .clone()
            .or_else(|| known.as_ref().map(|row| row.title()))
            .unwrap_or_else(|| addr.display());
        let codecs = self.codec_offer((peer != [0; 6]).then_some(peer));
        let id = self.begin_tab(name.clone());
        self.stage_session(id, peer, udp);
        match self
            .link
            .connect(self.nic.clone(), peer, trace, 0, udp, codecs, false, None)
        {
            Ok(()) => {
                self.message = None;
                self.pending_recent = Some((name, addr));
            }
            Err(err) => {
                self.abort_tab(id);
                self.message = Some(err);
            }
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
        if key == "enter" || key == "escape" || key == "tab" {
            return;
        }
        if self.direct.take().is_some() {
            self.peer.clear();
        }
        self.target_name = None;
        if key == "backspace" {
            if mods.control || mods.alt || mods.platform {
                self.peer.clear();
            } else {
                self.peer.backspace();
            }
            self.drop_connect_menus_without_target();
            return;
        }
        if ev.is_held || mods.control || mods.alt || mods.platform {
            self.drop_connect_menus_without_target();
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
        self.drop_connect_menus_without_target();
    }

    /// A picked computer, or a finished MAC, is who the Connect menus apply to.
    fn connect_choices_ready(&self) -> bool {
        self.direct.is_some() || self.peer.parse().is_some()
    }

    fn drop_connect_menus_without_target(&mut self) {
        if self.connect_choices_ready() {
            return;
        }
        if matches!(
            self.menu,
            Some(
                Menu::Video | Menu::Audio | Menu::Fps | Menu::AudioBitrate | Menu::AudioChannels
            )
        ) {
            self.menu = None;
        }
    }

    fn status_line(&self) -> String {
        self.message.clone().unwrap_or_else(|| self.link.status())
    }

    fn copy_text(&mut self, id: &str, text: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        let id = id.to_string();
        self.copied = Some((id.clone(), Instant::now()));
        cx.spawn(async move |this, cx| {
            gpui::Timer::after(Duration::from_secs(2)).await;
            let _ = this.update(cx, |desk, cx| {
                if desk.copied.as_ref().map(|(got, _)| got.as_str()) == Some(id.as_str()) {
                    desk.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn copy_chip_label(&self, id: &str) -> &'static str {
        match &self.copied {
            Some((got, at)) if got == id && at.elapsed() < Duration::from_secs(2) => "Copied",
            _ => "Copy",
        }
    }

    /// Wrapping text with a Copy chip. GPUI labels cannot be selected.
    fn copyable_line(
        &mut self,
        id: impl Into<String>,
        text: String,
        color: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = id.into();
        let label = self.copy_chip_label(&id);
        let body = text.clone();
        let text_id = SharedString::from(format!("{id}-text"));
        let copy_id = SharedString::from(format!("{id}-copy"));
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .id(text_id)
                    .flex_1()
                    .min_w_0()
                    .text_color(rgb(color))
                    .whitespace_normal()
                    .cursor_pointer()
                    .on_click({
                        let id = id.clone();
                        let body = body.clone();
                        cx.listener(move |desk, _, _, cx| {
                            desk.copy_text(&id, &body, cx);
                        })
                    })
                    .child(text),
            )
            .child(small_chip(copy_id, label, None, true, {
                let id = id.clone();
                cx.listener(move |desk, _, _, cx| {
                    cx.stop_propagation();
                    desk.copy_text(&id, &body, cx);
                })
            }))
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

    fn connection_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.active_id();
        let tabs: Vec<(u64, String, bool)> = self
            .tabs
            .iter()
            .map(|tab| (tab.id, tab.label.clone(), Some(tab.id) == active))
            .collect();
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .children(tabs.into_iter().map(|(id, label, on)| {
                let title = label.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id(SharedString::from(format!("conn-tab-{id}")))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(if on { 0x243044 } else { 0x1a1e27 }))
                            .text_color(rgb(TEXT))
                            .child(title)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |desk, ev: &MouseDownEvent, window, cx| {
                                    desk.on_session_click(id, ev.click_count, window);
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("conn-tab-x-{id}")))
                            .px_1()
                            .text_color(rgb(MUTED))
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |desk, _, _, cx| {
                                    cx.stop_propagation();
                                    desk.close_connection(id);
                                    cx.notify();
                                }),
                            ),
                    )
            }))
    }

    /// Grow a window that opened before the compositor named a display.
    fn fit_display(&mut self, cx: &mut Context<Self>) {
        if self.display_fit {
            return;
        }
        let Some(screen) = screen_of(cx) else {
            return;
        };
        if screen.w < 200. || screen.h < 200. {
            return;
        }
        let Some(handle) = cx.active_window() else {
            return;
        };
        let open = crate::place::fit(Some(screen)).opening;
        if cx
            .update_window(handle, |_desk, window, _cx| {
                let current = window.bounds().size;
                let dw = (f32::from(current.width) - open.w).abs();
                let dh = (f32::from(current.height) - open.h).abs();
                if dw > 24. || dh > 24. {
                    window.resize(size(px(open.w), px(open.h)));
                }
            })
            .is_ok()
        {
            self.display_fit = true;
        }
    }

    /// Addresses `interface_info` found for this computer.
    fn this_pc_addresses(&self) -> String {
        let v4 = if self.share.ipv4.is_empty() {
            "not found".to_string()
        } else {
            self.share
                .ipv4
                .iter()
                .map(|ip| ip.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let v6 = if self.share.ipv6.is_empty() {
            "not found".to_string()
        } else {
            self.share
                .ipv6
                .iter()
                .map(|ip| ip.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!("IPv4 {v4} · IPv6 {v6}")
    }

    fn this_pc_audio_line(&self) -> String {
        let audio = if self.local_audio.opus {
            "PCM, Opus"
        } else {
            "PCM"
        };
        format!("Audio: {audio}")
    }

    fn codec_rows(&self) -> Vec<crate::CodecSupportRow> {
        local_codec_rows(
            &self.local_video,
            etherquic_play::etherquic_media::caps::ready(),
            self.pyrowave_library,
        )
    }

    fn codec_support_table(&mut self, id: &str, cx: &mut Context<Self>) -> impl IntoElement {
        const COLS: [(&str, f32, &'static str); 10] = [
            ("Codec", 80., "Codec"),
            ("SW", 32., "Software encode on this computer"),
            ("HW", 32., "Hardware encode on this computer"),
            ("Backend", 110., "Encoder the probe opened"),
            (
                "Error",
                44.,
                "Library error. Click to copy, hover for the full text",
            ),
            (
                "Latency",
                78.,
                "Typical display-grab to decoded frame in this product",
            ),
            (
                "PSNR/B",
                54.,
                "Relative PSNR per coded byte versus H.264. Higher compresses better",
            ),
            ("I", 22., "I-frames"),
            ("P", 22., "P-frames"),
            ("B", 22., "B-frames. Live encode keeps these off"),
        ];
        let table_w: f32 = COLS.iter().map(|col| col.1).sum();
        let rows = self.codec_rows();
        let copy_id = format!("{id}-copy");
        let copy_label = self.copy_chip_label(&copy_id);
        let table_text = codec_table_text(&rows);
        let mut header = div().flex().flex_row().items_center().w(px(table_w)).pb_1();
        for (label, width, why) in COLS {
            header = header.child(
                div()
                    .id(SharedString::from(format!("{id}-h-{label}")))
                    .flex_none()
                    .w(px(width))
                    .px_1()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(MUTED))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .tooltip(why_tip(why))
                    .child(label),
            );
        }
        let mut body = div().flex().flex_col().w(px(table_w));
        for row in &rows {
            let err_id = format!("{id}-err-{}", row.name);
            let err_copied = self.copy_chip_label(&err_id) == "Copied";
            let software_color = if row.software { TEXT } else { MUTED };
            let hardware_color = if row.hardware { TEXT } else { MUTED };
            let error_text = row.error.clone();
            let line = div()
                .flex()
                .flex_row()
                .items_center()
                .w(px(table_w))
                .py(px(2.))
                .child(codec_cell(COLS[0].1, TEXT, row.name))
                .child(codec_cell(COLS[1].1, software_color, row.software_label()))
                .child(codec_cell(COLS[2].1, hardware_color, row.hardware_label()))
                .child(codec_cell(COLS[3].1, TEXT, row.backend.clone()))
                .child(
                    div()
                        .id(SharedString::from(err_id.clone()))
                        .flex_none()
                        .w(px(COLS[4].1))
                        .px_1()
                        .text_xs()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .when_some(error_text.clone(), |cell, err| {
                            cell.text_color(rgb(if err_copied { HOST_GREEN } else { DANGER }))
                                .cursor_pointer()
                                .tooltip(shared_tip(err.clone()))
                                .on_click({
                                    let err_id = err_id.clone();
                                    cx.listener(move |desk, _, _, cx| {
                                        desk.copy_text(&err_id, &err, cx);
                                    })
                                })
                                .child(if err_copied { "copied" } else { "error" })
                        })
                        .when(error_text.is_none(), |cell| {
                            cell.text_color(rgb(MUTED)).child("—")
                        }),
                )
                .child(codec_cell(COLS[5].1, MUTED, row.latency))
                .child(codec_cell(COLS[6].1, MUTED, row.psnr_per_byte))
                .child(codec_cell(
                    COLS[7].1,
                    MUTED,
                    crate::CodecSupportRow::frame_label(row.i_frame),
                ))
                .child(codec_cell(
                    COLS[8].1,
                    MUTED,
                    crate::CodecSupportRow::frame_label(row.p_frame),
                ))
                .child(codec_cell(
                    COLS[9].1,
                    MUTED,
                    crate::CodecSupportRow::frame_label(row.b_frame),
                ));
            body = body.child(line);
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(MUTED))
                            .whitespace_normal()
                            .child(
                                "Encode support on this computer. Latency and PSNR/B are catalog typical values.",
                            ),
                    )
                    .child(small_chip(
                        SharedString::from(format!("{id}-copy")),
                        copy_label,
                        None,
                        true,
                        {
                            let copy_id = copy_id.clone();
                            cx.listener(move |desk, _, _, cx| {
                                cx.stop_propagation();
                                desk.copy_text(&copy_id, &table_text, cx);
                            })
                        },
                    )),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{id}-scroll")))
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w(px(table_w))
                            .child(header)
                            .child(body),
                    ),
            )
    }

    fn host_card(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let hosting = self.link.role() == Role::Host;
        let enabled = true;
        let label = if hosting { "Stop hosting" } else { "Host" };
        let edge = if hosting { HOST_GREEN } else { CARD_EDGE };
        let capture = self.capture_fps();
        let open = self.open.host;
        let rates = open.then(|| self.global_fps_row("host-fps", cx));
        let ours = self.ours.clone();
        let grid = self.qr.clone();
        let qr = open.then(move || phone_share(&grid, &ours));
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
            .child(muted_line(self.host_summary.clone()))
            .child(self.guest_lines())
            .child(muted_line("Share this desktop and its audio.".into()))
            .child(self.listen_block(cx))
            .child(self.copyable_line("host-addr", self.this_pc_addresses(), MUTED, cx))
            .child(muted_line(self.this_pc_audio_line()))
            .child(self.codec_support_table("host-codecs", cx))
            .child(action_button(
                "host-btn",
                label,
                enabled,
                HOST_GREEN,
                cx.listener(|desk, _, window, cx| {
                    desk.on_host(window, cx);
                }),
            ))
            .when(self.show_host_service_prompt(), |card| {
                card.child(muted_line(
                    "Install the host as a background service so players can connect when you are signed out. Layer-2 Ethernet uses BPF, which that service runs as root. An administrator password is required.".into(),
                ))
                .child(action_button(
                    "host-svc-install",
                    "Install host service",
                    true,
                    HOST_GREEN,
                    cx.listener(|desk, _, _, cx| {
                        desk.install_host_service(cx);
                    }),
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(small_chip(
                            SharedString::from("host-svc-later"),
                            "Not now",
                            None,
                            true,
                            cx.listener(|desk, _, _, cx| {
                                desk.host_service_hidden = true;
                                cx.notify();
                            }),
                        ))
                        .child(small_chip(
                            SharedString::from("host-svc-skip"),
                            "Don't ask again",
                            None,
                            true,
                            cx.listener(|desk, _, _, cx| {
                                desk.host_service_hidden = true;
                                desk.edit_settings(|settings| {
                                    settings.skip_host_service_prompt = true;
                                });
                                cx.notify();
                            }),
                        )),
                )
            })
            .child(disclosure(
                "host-more",
                "Hosting options",
                format!("{capture} fps · version {}", self.ours.version),
                open,
                cx.listener(|desk, _, _, cx| {
                    desk.open.host = !desk.open.host;
                    cx.notify();
                }),
            ))
            .when(open, |card| {
                card.child(card_body(
                    "A player sees this desktop, hears it, and controls it. The taskbar button is on only while hosting.",
                ))
                .child(card_meta(format!("This PC captures at {capture} fps.")))
                .when_some(rates, |card, rates| card.child(rates))
                .when_some(qr, |card, qr| card.child(qr))
            })
    }

    fn connect_card(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playing = self.link.role() == Role::Client;
        let enabled = true;
        let label = if playing { "Disconnect" } else { "Connect" };
        let edge = if playing { PLAYER_BLUE } else { CARD_EDGE };
        let focused = self.focus.is_focused(window);
        let open = self.open.connect;
        let selection = open.then(|| self.connection_selection());
        let path = open.then(|| self.path_row(cx));
        let carriers = open.then(|| self.carrier_row(cx));
        let typed = match self.direct {
            Some(addr) => addr.display(),
            None => self.peer.display(),
        };
        let field_color = if typed.is_empty() { MUTED } else { TEXT };
        let field_text = if typed.is_empty() {
            "Host MAC, aa:bb:cc:dd:ee:ff".to_string()
        } else {
            typed
        };
        let target = self.target_label();
        let via = self.via_label();
        let tabs = self.transport_tabs(cx);
        let video = self.codec_row(Menu::Video, cx);
        let fps = self.side_pick(Menu::Fps, cx);
        let audio = self.codec_row(Menu::Audio, cx);
        let kbps = self.side_pick(Menu::AudioBitrate, cx);
        let channels = self.side_pick(Menu::AudioChannels, cx);
        let dirty_on = self.settings.dirty_rects;
        let quality = self.picture_quality_slider("connect-quality", cx);
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(card_title("Connect", PLAYER_BLUE))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(TEXT))
                            .child(target),
                    ),
            )
            .child(tabs)
            .child(
                div()
                    .id("peer-mac")
                    .flex()
                    .w_full()
                    .h(px(34.))
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
                    .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, window, cx| {
                        if desk.forward_session_key(ev) {
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        if ev.keystroke.key == "enter" {
                            desk.on_connect(window, cx);
                            return;
                        }
                        desk.on_peer_key(ev);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(video)
                    .child(row_label("FPS"))
                    .child(fps),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(audio)
                    .child(row_label("Bitrate"))
                    .child(kbps)
                    .child(row_label("Channels"))
                    .child(channels),
            )
            .child(quality)
            .child(self.yuv_pick_row("connect-yuv", cx))
            .child(muted_line(
                "PyroWave colour. 4:4:4 keeps every chroma sample. 4:2:2 and 4:2:0 subsample. 10-bit feeds the encoder finer YUV. MJPEG and the other codecs ignore this.".into(),
            ))
            .child(self.quality_preview_row())
            .child(toggle_row(
                "connect-dirty-rects",
                "Send only changed tiles",
                dirty_on,
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| settings.dirty_rects = !settings.dirty_rects);
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Asked of the host, like the codec. If it supports dirty rectangles it skips an unchanged picture, and with MJPEG or MJ2K it encodes only the tiles that moved.".into(),
            ))
            .child(toggle_row(
                "connect-view-only",
                "View only",
                self.settings.view_only,
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| settings.view_only = !settings.view_only);
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Watch the desktop without sending keyboard, mouse, or controllers. Other players can still inject.".into(),
            ))
            .child(toggle_row(
                "connect-exclusive",
                "Exclusive session",
                self.settings.exclusive,
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| settings.exclusive = !settings.exclusive);
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Kick everyone already watching and refuse later joins, including view-only.".into(),
            ))
            .child(action_button(
                "connect-btn",
                label,
                enabled,
                PLAYER_BLUE,
                cx.listener(|desk, _, window, cx| {
                    desk.on_connect(window, cx);
                }),
            ))
            .when(cfg!(target_os = "android"), |card| {
                let open = self.ime_open;
                card.child(small_chip(
                    "connect-keyboard",
                    if open { "Hide keyboard" } else { "Show keyboard" },
                    open.then_some(PLAYER_BLUE),
                    true,
                    cx.listener(|desk, _, _, cx| {
                        desk.toggle_ime();
                        cx.notify();
                    }),
                ))
            })
            .child(disclosure(
                "connect-more",
                "Connection path",
                via,
                open,
                cx.listener(|desk, _, _, cx| {
                    desk.open.connect = !desk.open.connect;
                    cx.notify();
                }),
            ))
            .when(open, |card| {
                card.child(card_body(
                    "Open the remote desktop. Focus that window to use the keyboard and the mouse.",
                ))
                .child(card_meta(format!(
                    "This PC appears as {}",
                    format_mac(CLIENT_MAC)
                )))
                .when_some(selection, |card, selection| {
                    card.child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(PLAYER_BLUE))
                            .child(selection),
                    )
                })
                .when(self.direct.is_some(), |card| {
                    card.child(muted_line(
                        "This saved host dials its IP address. Type a MAC address to use Ethernet instead.".into(),
                    ))
                })
                .when_some(path, |card, path| card.child(path))
                .when_some(carriers, |card, carriers| card.child(carriers))
            })
    }

    /// The decoded picture for one host, decoding again only when the host sent
    /// a different codestream.
    fn thumbnail_image(&mut self, key: [u8; 6], bytes: Option<&[u8]>) -> Option<Arc<RenderImage>> {
        use std::hash::{Hash, Hasher};
        let bytes = bytes?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let stamp = hasher.finish();
        if let Some((seen, picture)) = self.thumbs.get(&key) {
            if *seen == stamp {
                return picture.clone();
            }
        }
        let picture = decode_thumbnail(bytes).and_then(render_image);
        self.thumbs.insert(key, (stamp, picture.clone()));
        picture
    }

    /// Picture and one-line summary for a Saved or Recent host, from the last
    /// time discovery heard it.
    fn row_extras(&mut self, addr: &HostAddr) -> (AnyElement, String) {
        let row = self.roster.find(addr).cloned();
        let picture = row
            .as_ref()
            .and_then(|row| self.thumbnail_image(row.key, row.thumbnail.as_deref()));
        let summary = match &row {
            Some(row) => row_summary(row.caps.as_ref(), row.release.as_ref()),
            None => "Not heard on this network yet".into(),
        };
        let (caps, release) = match &row {
            Some(row) => (row.caps.clone(), row.release.clone()),
            None => (None, None),
        };
        (host_lead(picture, caps.as_ref(), release.as_ref()), summary)
    }

    /// The host Connect will dial, by name when one was picked.
    fn target_label(&self) -> String {
        if let Some(name) = &self.target_name {
            return name.clone();
        }
        if let Some(addr) = self.direct {
            return addr.display();
        }
        match self.peer.parse() {
            Some(mac) => self
                .book
                .saved
                .iter()
                .find(|entry| entry.addr.same_host(&HostAddr::Mac(mac)))
                .map(|entry| entry.name.clone())
                .or_else(|| self.roster.find(&HostAddr::Mac(mac)).map(|row| row.title()))
                .unwrap_or_else(|| "Typed address".into()),
            None => "No host picked".into(),
        }
    }

    fn via_label(&self) -> String {
        let path = if let Some(addr) = self.direct {
            format!("via {}", addr.kind())
        } else {
            let row = self
                .peer
                .parse()
                .and_then(|mac| self.roster.find(&HostAddr::Mac(mac)));
            let (v4, v6) = row
                .map(|row| (row.ipv4.clone(), row.ipv6.clone()))
                .unwrap_or_default();
            match active_layer(self.layer, &v4, &v6) {
                Layer::L2 => match etherquic::l2_unavailable_reason() {
                    Some(why) => format!("L2 unavailable: {why}"),
                    None => "via Ethernet (L2)".into(),
                },
                Layer::V4(ip) => format!("via IPv4 {ip}"),
                Layer::V6(ip) => format!("via IPv6 {ip}"),
            }
        };
        format!("{path} · {}", self.carrier_summary())
    }

    fn selected_carrier(&self) -> u8 {
        self.peer
            .parse()
            .and_then(|mac| self.connections.get(&mac_key(mac)))
            .and_then(|over| over.carrier)
            .unwrap_or(etherquic::carrier::QUIC_STREAMS)
    }

    fn saved_profile(&self) -> Option<String> {
        self.peer
            .parse()
            .and_then(|mac| self.connections.get(&mac_key(mac)))
            .and_then(|over| over.profile.clone())
    }

    /// A saved `plain` or `gcm` selects both UDP/RTP and ST 2110. No pin restores the defaults.
    fn apply_saved_profile(&self, peer: [u8; 6]) {
        match self
            .connections
            .get(&mac_key(peer))
            .and_then(|over| over.profile.as_deref())
        {
            Some("plain") => etherquic::carrier::choose_profile(true, false),
            Some("gcm") => etherquic::carrier::choose_profile(false, true),
            _ => etherquic::carrier::choose_profile(false, false),
        }
    }

    fn carrier_summary(&self) -> String {
        etherquic::carrier::connect_row_now(self.selected_carrier())
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
        let v4 = device
            .as_ref()
            .map(|device| device.ipv4.clone())
            .unwrap_or_default();
        let v6 = device
            .as_ref()
            .map(|device| device.ipv6.clone())
            .unwrap_or_default();
        let active = active_layer(self.layer, &v4, &v6);
        let l2_why = etherquic::l2_unavailable_reason();
        let l2_ok = l2_why.is_none();
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
            matches!(active, Layer::L2) && l2_ok,
            l2_ok,
            l2_why,
            cx.listener(move |desk, _, _, cx| {
                if let Some(why) = l2_why {
                    desk.message = Some(why.into());
                } else {
                    desk.layer = Layer::L2;
                }
                cx.notify();
            }),
        ));
        for (index, ip) in v4.into_iter().enumerate() {
            let on = matches!(active, Layer::V4(chosen) if chosen == ip);
            chips = chips.child(path_chip(
                SharedString::from(format!("path-v4-{index}")),
                format!("IPv4 {ip}"),
                on,
                true,
                None,
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
                true,
                None,
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

    fn carrier_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_carrier();
        let choices = [
            ("carrier-quic", "QUIC streams", etherquic::carrier::QUIC_STREAMS),
            (
                "carrier-dgram",
                "QUIC datagrams",
                etherquic::carrier::QUIC_DATAGRAMS,
            ),
            ("carrier-rtp", "UDP/RTP", etherquic::carrier::UDP_RTP),
            ("carrier-avb", "AVB", etherquic::carrier::AVB),
            ("carrier-st", "ST 2110", etherquic::carrier::ST2110),
        ];
        let mut chips = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
        for (id, label, value) in choices {
            chips = chips.child(path_chip(
                id,
                label,
                selected == value,
                true,
                None,
                cx.listener(move |desk, _, _, cx| {
                    if let Some(mac) = desk.peer.parse() {
                        desk.edit_connection(mac, |over| over.carrier = Some(value));
                    } else {
                        desk.message =
                            Some("Type a host address before choosing a carrier.".into());
                    }
                    cx.notify();
                }),
            ));
        }
        let latency_open = self.open.latency;
        let mut column = div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(section_label("Carrier"))
                    .child(crate::latency_help::button(
                        latency_open,
                        cx.listener(|desk, _, _, cx| {
                            desk.open.latency = !desk.open.latency;
                            cx.notify();
                        }),
                    )),
            )
            .child(chips)
            .when(latency_open, |column| column.child(crate::latency_help::panel()));
        if selected == etherquic::carrier::UDP_RTP || selected == etherquic::carrier::ST2110 {
            let saved = self.saved_profile();
            let plain_on = if selected == etherquic::carrier::ST2110 {
                saved.as_deref() != Some("gcm")
            } else {
                saved.as_deref() == Some("plain")
            };
            let mut profiles = div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_2()
                .w_full()
                .min_w_0();
            for (id, label, on, plain) in [
                ("profile-plain", "Plain", plain_on, true),
                ("profile-gcm", "GCM", !plain_on, false),
            ] {
                profiles = profiles.child(path_chip(
                    id,
                    label,
                    on,
                    true,
                    None,
                    cx.listener(move |desk, _, _, cx| {
                        if let Some(mac) = desk.peer.parse() {
                            desk.edit_connection(mac, |over| {
                                over.profile = Some(if plain { "plain" } else { "gcm" }.into());
                            });
                            etherquic::carrier::choose_profile(plain, !plain);
                        } else {
                            desk.message =
                                Some("Type a host address before choosing a profile.".into());
                        }
                        cx.notify();
                    }),
                ));
            }
            column = column
                .child(section_label("Profile"))
                .child(profiles);
        }
        column.child(muted_line(self.carrier_summary()))
    }

    fn guest_lines(&self) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_1().w_full().min_w_0();
        if self.service_guests.is_empty() {
            col = col.child(muted_line(
                "No computers are connected to the host service.".into(),
            ));
        } else {
            for guest in &self.service_guests {
                col = col.child(muted_line(crate::desk_model::guest_line(guest)));
            }
        }
        col
    }

    fn listen_block(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_1().w_full().min_w_0();
        col = col.child(section_label("Listen on"));
        col = col.child(muted_line(
            "All physical and virtual adapters start on. Loopback stays off. A capture device such as any is not a link.".into(),
        ));
        let mut list = div().flex().flex_col().items_start().gap_1().w_full().min_w_0();
        list = list.child(listen_row(
            "listen-all",
            "All".into(),
            self.listen.is_all().then_some(HOST_GREEN),
            true,
            cx.listener(|desk, _, _, cx| {
                desk.listen.set_all();
                desk.apply_listen();
                cx.notify();
            }),
        ));
        let rows: Vec<_> = self
            .listen
            .rows()
            .iter()
            .map(|row| {
                (
                    row.name.clone(),
                    row.label(),
                    row.listen,
                    row.kind == etherquic::IfaceKind::Capture,
                )
            })
            .collect();
        for (index, (name, label, on, capture)) in rows.into_iter().enumerate() {
            list = list.child(listen_row(
                SharedString::from(format!("listen-{index}")),
                label.into(),
                on.then_some(if capture { MUTED } else { HOST_GREEN }),
                !capture,
                cx.listener(move |desk, _, _, cx| {
                    desk.toggle_listen(name.clone());
                    cx.notify();
                }),
            ));
        }
        col.child(list)
    }

    fn active_block(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .outbound
            .iter()
            .map(|row| {
                (
                    row.id,
                    row.label.clone(),
                    row.phase.clone(),
                    row.group,
                )
            })
            .collect();
        let mut list = div().flex().flex_col().gap_1().w_full().min_w_0();
        if rows.is_empty() {
            list = list.child(muted_line(
                "No active connections. A connection leaves this list when it closes.".into(),
            ));
        }
        for (id, label, phase, group) in rows {
            let selected = self.inspect == Some(id);
            list = list.child(
                div()
                    .id(SharedString::from(format!("active-{id}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(if selected { 0x243044 } else { CARD }))
                    .cursor_pointer()
                    .child(div().flex_1().min_w_0().child(crate::desk_model::outbound_line(
                        &label, &phase, group,
                    )))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |desk, ev: &MouseDownEvent, window, cx| {
                            desk.on_session_click(id, ev.click_count, window);
                            cx.notify();
                        }),
                    ),
            );
        }
        if self.trace_log.on {
            list = list.child(section_label("Trace"));
            let lines: Vec<String> = self.trace_log.lines().iter().rev().take(8).cloned().collect();
            if lines.is_empty() {
                list = list.child(muted_line(
                    "Network trace is on. Latency, loss, and packet errors show up here.".into(),
                ));
            }
            for line in lines {
                list = list.child(muted_line(line));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .child(section_label("Active connections"))
            .child(list)
            .child(self.quick_panel(cx))
    }

    fn quick_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.quick;
        let inspect = self.inspect;
        let mut panel = div().flex().flex_col().gap_1().w_full().min_w_0();
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(small_chip(
                    "quick-toggle",
                    if shown {
                        "Hide quick settings"
                    } else {
                        "Quick settings"
                    },
                    Some(PLAYER_BLUE),
                    true,
                    cx.listener(|desk, _, _, cx| {
                        desk.quick = !desk.quick;
                        cx.notify();
                    }),
                ))
                .child(small_chip(
                    "quick-detach",
                    "New window",
                    None,
                    self.tabs.active_id().is_some(),
                    cx.listener(|desk, _, _, cx| {
                        desk.detach_current(cx);
                    }),
                ))
                .child(muted_line("Ctrl+Alt+S".into())),
        );
        if !shown {
            return panel.into_any_element();
        }
        let Some(id) = inspect else {
            return panel
                .child(muted_line(
                    "Left-click an active connection to change it. Double-click focuses its window."
                        .into(),
                ))
                .into_any_element();
        };
        let snap = self.session_live.get(&id).map(|slot| {
            (
                etherquic_play::snapshot(&slot.view),
                etherquic_play::snapshot_tune(&slot.tune),
            )
        });
        let Some((view, tune)) = snap else {
            return panel
                .child(muted_line(
                    "This connection has no live settings yet.".into(),
                ))
                .into_any_element();
        };
        let mut scales = div().flex().flex_row().flex_wrap().gap_1();
        for mode in etherquic_play::ScaleMode::ALL {
            let on = view.scale == mode;
            scales = scales.child(small_chip(
                SharedString::from(format!("live-scale-{}", mode.token())),
                mode.label(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.scale = mode);
                    cx.notify();
                }),
            ));
        }
        let mut codecs = div().flex().flex_row().flex_wrap().gap_1();
        for pref in PicturePref::order() {
            let on = tune
                .video
                .and_then(|video| PicturePref::from_token(video.token()))
                == Some(pref);
            codecs = codecs.child(small_chip(
                SharedString::from(format!("live-video-{}", pref.token())),
                pref.name(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |_, tune| {
                        tune.video = etherquic::negotiate::Video::from_token(pref.token());
                    });
                    cx.notify();
                }),
            ));
        }
        let mut carriers = div().flex().flex_row().flex_wrap().gap_1();
        for carrier in 1..=5 {
            let on = tune.carrier == Some(carrier);
            carriers = carriers.child(small_chip(
                SharedString::from(format!("live-carrier-{carrier}")),
                format!("Carrier {carrier}"),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |_, tune| tune.carrier = Some(carrier));
                    cx.notify();
                }),
            ));
        }
        let mut audio = div().flex().flex_row().flex_wrap().gap_1();
        for codec in etherquic::negotiate::Audio::ALL {
            let on = tune.audio == Some(codec);
            audio = audio.child(small_chip(
                SharedString::from(format!("live-audio-{}", codec.token())),
                codec.name(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |_, tune| tune.audio = Some(codec));
                    cx.notify();
                }),
            ));
        }
        let wishes = [
            etherquic::negotiate::FpsWish::Auto,
            etherquic::negotiate::FpsWish::Match,
            etherquic::negotiate::FpsWish::Fixed(15),
            etherquic::negotiate::FpsWish::Fixed(30),
            etherquic::negotiate::FpsWish::Fixed(60),
        ];
        let mut rates = div().flex().flex_row().flex_wrap().gap_1();
        for wish in wishes {
            let on = tune.fps == Some(wish);
            let label = match wish {
                etherquic::negotiate::FpsWish::Auto => "FPS auto".to_string(),
                etherquic::negotiate::FpsWish::Match => "FPS display".to_string(),
                etherquic::negotiate::FpsWish::Fixed(fps) => format!("FPS {fps}"),
            };
            rates = rates.child(small_chip(
                SharedString::from(format!("live-fps-{}", wish.token())),
                label,
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |_, tune| tune.fps = Some(wish));
                    cx.notify();
                }),
            ));
        }
        let mut yuvs = div().flex().flex_row().flex_wrap().gap_1();
        for format in etherquic::negotiate::YuvFormat::ALL {
            let on = view.yuv == format;
            yuvs = yuvs.child(small_chip(
                SharedString::from(format!("live-yuv-{}", format.token())),
                format.label(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.yuv = format);
                    cx.notify();
                }),
            ));
        }
        let mut looks = div().flex().flex_row().flex_wrap().gap_1();
        for look in etherquic_play::Look::ALL {
            let on = view.look == look;
            looks = looks.child(small_chip(
                SharedString::from(format!("live-look-{}", look.token())),
                look.label(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.look = look);
                    cx.notify();
                }),
            ));
        }
        let mut dithers = div().flex().flex_row().flex_wrap().gap_1();
        for dither in etherquic_play::Dither::ALL {
            let on = view.dither == dither;
            dithers = dithers.child(small_chip(
                SharedString::from(format!("live-dither-{}", dither.token())),
                dither.label(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.dither = dither);
                    cx.notify();
                }),
            ));
        }
        let mut cursors = div().flex().flex_row().flex_wrap().gap_1();
        for cursor in etherquic_play::CursorMode::ALL {
            let on = view.cursor == cursor;
            cursors = cursors.child(small_chip(
                SharedString::from(format!("live-cursor-{}", cursor.token())),
                cursor.label(),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.cursor = cursor);
                    cx.notify();
                }),
            ));
        }
        let mut upscales = div().flex().flex_row().flex_wrap().gap_1();
        for mode in etherquic_play::Upscale::ALL {
            let on = view.upscale == mode;
            upscales = upscales.child(small_chip(
                SharedString::from(format!("live-upscale-{}", mode.token())),
                format!("Upscale {}", mode.label()),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.upscale = mode);
                    cx.notify();
                }),
            ));
        }
        let mut frames = div().flex().flex_row().flex_wrap().gap_1();
        for mode in etherquic_play::FrameGen::ALL {
            let on = view.frame_gen == mode;
            frames = frames.child(small_chip(
                SharedString::from(format!("live-fruc-{}", mode.token())),
                format!("Frame gen {}", mode.label()),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, move |view, _| view.frame_gen = mode);
                    cx.notify();
                }),
            ));
        }
        let layer_peer = self.session_live.get(&id).map(|slot| slot.peer);
        let (v4s, v6s) = layer_peer
            .and_then(|mac| {
                self.scan.board().devices.iter().find(|device| device.l2_mac == mac).map(
                    |device| (device.ipv4.clone(), device.ipv6.clone()),
                )
            })
            .unwrap_or_default();
        let mut layers = div().flex().flex_row().flex_wrap().gap_1();
        let l2_on = matches!(self.layer, Layer::L2);
        layers = layers.child(small_chip(
            "live-layer-l2",
            "L2",
            l2_on.then_some(PLAYER_BLUE),
            true,
            cx.listener(|desk, _, _, cx| {
                desk.layer = Layer::L2;
                cx.notify();
            }),
        ));
        for (index, ip) in v4s.into_iter().enumerate() {
            let on = matches!(self.layer, Layer::V4(chosen) if chosen == ip);
            layers = layers.child(small_chip(
                SharedString::from(format!("live-layer-v4-{index}")),
                format!("IPv4 {ip}"),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.layer = Layer::V4(ip);
                    cx.notify();
                }),
            ));
        }
        for (index, ip) in v6s.into_iter().enumerate() {
            let on = matches!(self.layer, Layer::V6(chosen) if chosen == ip);
            layers = layers.child(small_chip(
                SharedString::from(format!("live-layer-v6-{index}")),
                format!("IPv6 {ip}"),
                on.then_some(PLAYER_BLUE),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.layer = Layer::V6(ip);
                    cx.notify();
                }),
            ));
        }
        panel
            .child(section_label("This connection"))
            .child(scales)
            .child(codecs)
            .child(audio)
            .child(rates)
            .child(carriers)
            .child(yuvs)
            .child(looks)
            .child(dithers)
            .child(cursors)
            .child(upscales)
            .child(frames)
            .child(layers)
            .child({
                let mut quality = div().flex().flex_row().flex_wrap().gap_1();
                for value in [40u8, 70, 90] {
                    let on = view.quality == value;
                    quality = quality.child(small_chip(
                        SharedString::from(format!("live-quality-{value}")),
                        format!("Quality {value}"),
                        on.then_some(PLAYER_BLUE),
                        true,
                        cx.listener(move |desk, _, _, cx| {
                            desk.edit_session(id, move |view, _| view.quality = value);
                            cx.notify();
                        }),
                    ));
                }
                quality.child(small_chip(
                    "live-keyboard",
                    if view.send_keyboard {
                        "Keyboard on"
                    } else {
                        "Keyboard off"
                    },
                    view.send_keyboard.then_some(HOST_GREEN),
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_session(id, |view, _| view.send_keyboard = !view.send_keyboard);
                        cx.notify();
                    }),
                ))
                .child(small_chip(
                    "live-mouse",
                    if view.send_mouse {
                        "Mouse on"
                    } else {
                        "Mouse off"
                    },
                    view.send_mouse.then_some(HOST_GREEN),
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_session(id, |view, _| view.send_mouse = !view.send_mouse);
                        cx.notify();
                    }),
                ))
            })
            .child(small_chip(
                "live-dirty",
                if view.dirty_rects {
                    "Dirty rects on"
                } else {
                    "Dirty rects off"
                },
                view.dirty_rects.then_some(HOST_GREEN),
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.edit_session(id, |view, _| view.dirty_rects = !view.dirty_rects);
                    cx.notify();
                }),
            ))
            .child(small_chip(
                "live-redial",
                "Apply interface / layer",
                Some(DANGER),
                true,
                cx.listener(|desk, _, _, cx| {
                    desk.redial_inspected(cx);
                }),
            ))
            .child(muted_line(
                "Scale, filters, and dirty rects change the open view. Codec and carrier are sent again on the same connection. Interface and L2/L3 open the connection again.".into(),
            ))
            .into_any_element()
    }

    fn detach_current(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let group = self.layout.detach(id);
        if self.shelves.contains(&group) {
            return;
        }
        self.shelves.push(group);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(format!("etherquic · window {group}").into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| Shelf { group }),
        )
        .ok();
        cx.notify();
    }

    fn nic_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let all = self.listen.is_all();
        let listening: Vec<String> = self
            .listen
            .rows()
            .iter()
            .filter(|row| row.listen)
            .map(|row| row.name.clone())
            .collect();
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
        row = row.child(nic_chip(
            "nic-auto",
            "Auto",
            all,
            cx.listener(|desk, _, _, cx| {
                desk.listen.set_all();
                desk.apply_listen();
                cx.notify();
            }),
        ));
        for (index, nic) in self.nics.iter().enumerate() {
            if !nic_shown(nic) {
                continue;
            }
            let name = nic.name.clone();
            let on = !all && listening.iter().any(|row| row == &nic.name);
            let label = nic_label(nic);
            row = row.child(nic_chip(
                SharedString::from(format!("nic-{index}")),
                label,
                on,
                cx.listener(move |desk, _, _, cx| {
                    if desk.listen.select_only(&name) {
                        desk.apply_listen();
                    }
                    cx.notify();
                }),
            ));
        }
        row
    }

    /// Saved, Recent, and Discovered in one picker with a filter. The list
    /// scrolls on its own so a long roster does not push the page around.
    fn host_picker(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let board = self.scan.board();
        let now = now_secs();
        let idle = self.link.role() == Role::Idle;
        let filter = self.filter.clone();
        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .flex_none();
        let mut shown = 0usize;
        let mut empty = String::new();
        match self.tab {
            HostTab::Saved => {
                for index in self.book.saved_order() {
                    let entry = self.book.saved[index].clone();
                    if self.here.owns_addr(&entry.addr) {
                        continue;
                    }
                    let address = format!("{} · {}", entry.addr.display(), entry.addr.kind());
                    if !matches_filter(&filter, &[&entry.name, &address]) {
                        continue;
                    }
                    shown += 1;
                    let presence = self.roster.presence(&entry.addr);
                    let row = self.saved_row(
                        index,
                        entry.name.clone(),
                        entry.addr,
                        address,
                        presence,
                        now,
                        idle,
                        window,
                        cx,
                    );
                    list = list.child(row);
                }
                if shown == 0 {
                    empty = if self
                        .book
                        .saved
                        .iter()
                        .any(|entry| !self.here.owns_addr(&entry.addr))
                    {
                        "No saved host matches the filter.".into()
                    } else {
                        "Nothing saved yet. Use Add host, or Save on a Recent or Discovered host."
                            .into()
                    };
                }
            }
            HostTab::Recent => {
                for (index, entry) in self.book.recent.clone().into_iter().enumerate() {
                    if self.here.owns_addr(&entry.addr) {
                        continue;
                    }
                    let address = format!("{} · {}", entry.addr.display(), entry.addr.kind());
                    if !matches_filter(&filter, &[&entry.name, &address]) {
                        continue;
                    }
                    shown += 1;
                    let presence = self.roster.presence(&entry.addr);
                    let saved = self.book.saved_index(&entry.addr).is_some();
                    let status = format!("{} · used {}", presence.label(now), ago(now, entry.at));
                    let selected = self.is_target(&entry.addr);
                    let name = entry.name.clone();
                    let addr = entry.addr;
                    let actions = div()
                        .flex()
                        .flex_row()
                        .flex_none()
                        .gap_1()
                        .child(save_chip(
                            SharedString::from(format!("recent-save-{index}")),
                            saved,
                            cx.listener({
                                let name = name.clone();
                                move |desk, _, _, cx| {
                                    cx.stop_propagation();
                                    desk.book.save(&name, addr, now_secs());
                                    desk.persist_hosts();
                                    cx.notify();
                                }
                            }),
                        ))
                        .child(small_chip(
                            SharedString::from(format!("recent-remove-{index}")),
                            "Remove",
                            None,
                            true,
                            cx.listener(move |desk, _, _, cx| {
                                cx.stop_propagation();
                                desk.book.remove_recent(index);
                                desk.persist_hosts();
                                cx.notify();
                            }),
                        ))
                        .child(self.connect_chip(
                            format!("recent-go-{index}"),
                            name.clone(),
                            addr,
                            idle,
                            cx,
                        ));
                    let (lead, summary) = self.row_extras(&addr);
                    list = list.child(host_row(
                        SharedString::from(format!("recent-{index}")),
                        Some(lead),
                        row_title(name.clone()),
                        address,
                        summary,
                        status,
                        presence_color(presence),
                        selected,
                        actions,
                        cx.listener(move |desk, _: &ClickEvent, _, cx| {
                            desk.pick(&name, addr);
                            cx.notify();
                        }),
                    ));
                }
                if shown == 0 {
                    empty = if self
                        .book
                        .recent
                        .iter()
                        .any(|entry| !self.here.owns_addr(&entry.addr))
                    {
                        "No recent host matches the filter.".into()
                    } else {
                        "Hosts you connect to show up here.".into()
                    };
                }
            }
            HostTab::Discovered => {
                let details = self.open.devices;
                let rows: Vec<_> = self.roster.ordered().into_iter().cloned().collect();
                for row in rows {
                    if self.here.owns_row(&row) {
                        continue;
                    }
                    let title = row.title();
                    let summary = row_summary(row.caps.as_ref(), row.release.as_ref());
                    let address = row.address_line();
                    if !matches_filter(&filter, &[&title, &address, &summary]) {
                        continue;
                    }
                    shown += 1;
                    let mac = row.mac;
                    let key = format_mac(row.key).replace(':', "");
                    let found = board
                        .devices
                        .iter()
                        .find(|device| device.key == row.key)
                        .cloned();
                    let picture = self.thumbnail_image(row.key, row.thumbnail.as_deref());
                    let caps_lines = row.caps.as_ref().map(caps_details).unwrap_or_default();
                    // A host heard only over UDP is dialed by IP: raw Ethernet is
                    // not working here, and every host shares one L2 identity.
                    let udp_only = found
                        .as_ref()
                        .is_some_and(|device| device.via_udp && !device.via_l2);
                    let target = match (udp_only, row.ipv4.first(), row.ipv6.first()) {
                        (true, Some(ip), _) => HostAddr::Ip {
                            ip: IpAddr::V4(*ip),
                            port: row.port,
                        },
                        (true, None, Some(ip)) => HostAddr::Ip {
                            ip: IpAddr::V6(*ip),
                            port: row.port,
                        },
                        _ => HostAddr::Mac(mac),
                    };
                    let status = match (&found, row.online) {
                        (Some(device), true) => format!("Online · {}", ms(device.stats.avg_ms)),
                        _ => Presence::Seen(row.last_seen).label(now),
                    };
                    let color = if row.online { HOST_GREEN } else { MUTED };
                    let selected = self.is_target(&target);
                    let saved = self.book.saved_index(&target).is_some();
                    let extra = match (&found, details) {
                        (Some(device), true) => {
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
                            Some(device_details(device, &selection, &caps_lines).into_any_element())
                        }
                        (None, true) => Some(
                            muted_line("Not in the latest discovery burst.".into())
                                .into_any_element(),
                        ),
                        _ => None,
                    };
                    let actions = div()
                        .flex()
                        .flex_row()
                        .flex_none()
                        .gap_1()
                        .child(save_chip(
                            SharedString::from(format!("found-save-{key}")),
                            saved,
                            cx.listener({
                                let title = title.clone();
                                move |desk, _, _, cx| {
                                    cx.stop_propagation();
                                    desk.book.save(&title, target, now_secs());
                                    desk.persist_hosts();
                                    cx.notify();
                                }
                            }),
                        ))
                        .child(self.connect_chip(
                            format!("found-go-{key}"),
                            title.clone(),
                            target,
                            idle,
                            cx,
                        ));
                    let row_el = host_row(
                        SharedString::from(format!("found-{key}")),
                        Some(host_lead(picture, row.caps.as_ref(), row.release.as_ref())),
                        row_title(title.clone()),
                        address,
                        summary,
                        status,
                        color,
                        selected,
                        actions,
                        cx.listener(move |desk, _: &ClickEvent, _, cx| {
                            desk.pick(&title, target);
                            cx.notify();
                        }),
                    );
                    list = list.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w_full()
                            .min_w_0()
                            .child(row_el)
                            .when_some(extra, |col, extra| {
                                col.child(div().w_full().min_w_0().px_3().pb_2().child(extra))
                            }),
                    );
                }
                if shown == 0 {
                    let only_this_pc = !board.devices.is_empty()
                        && board.devices.iter().all(|device| {
                            self.here.owns(
                                device.name.trim(),
                                device.l2_mac,
                                device.key,
                                device.caps.as_ref().and_then(|caps| caps.nic_mac),
                                &device.ipv4,
                                &device.ipv6,
                            )
                        });
                    empty = if let Some(err) = board.error.clone() {
                        err
                    } else if !self.roster.is_empty() {
                        "No discovered host matches the filter.".into()
                    } else if only_this_pc {
                        "No other computers answered.".into()
                    } else if board.looking {
                        "Looking for hosts…".into()
                    } else {
                        "No hosts answered.".into()
                    };
                }
            }
        }
        let scan_error = board.error.clone();
        let empty_color = if self.tab == HostTab::Discovered && scan_error.is_some() {
            DANGER
        } else {
            MUTED
        };
        if shown == 0 {
            list = list.child(div().w_full().min_w_0().py_2().child(self.copyable_line(
                "scan-empty",
                empty,
                empty_color,
                cx,
            )));
        } else if self.tab == HostTab::Discovered {
            if let Some(err) = scan_error.clone() {
                list = list.child(div().w_full().min_w_0().py_1().child(self.copyable_line(
                    "scan-error",
                    err,
                    DANGER,
                    cx,
                )));
            }
        }
        let tabs = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .min_w_0()
            .child(tab_chip(
                "tab-saved",
                format!("Saved {}", self.saved_count()),
                self.tab == HostTab::Saved,
                cx.listener(|desk, _, _, cx| desk.set_tab(HostTab::Saved, cx)),
            ))
            .child(tab_chip(
                "tab-recent",
                format!("Recent {}", self.recent_count()),
                self.tab == HostTab::Recent,
                cx.listener(|desk, _, _, cx| desk.set_tab(HostTab::Recent, cx)),
            ))
            .child(tab_chip(
                "tab-found",
                if scan_error.is_some() {
                    "Discovered !".to_string()
                } else {
                    format!(
                        "Discovered {}/{}",
                        self.roster.online_count(),
                        self.roster.len()
                    )
                },
                self.tab == HostTab::Discovered,
                cx.listener(|desk, _, _, cx| desk.set_tab(HostTab::Discovered, cx)),
            ));
        let filter_field = text_field(
            "host-filter",
            &self.filter_focus,
            self.filter_focus.is_focused(window),
            &self.filter,
            "Filter by name or address",
        )
        .flex_1()
        .on_click(cx.listener(|desk, _: &ClickEvent, window, cx| {
            window.focus(&desk.filter_focus);
            cx.notify();
        }))
        .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, _, cx| {
            match edit_text(&mut desk.filter, ev, cx) {
                Edit::Cancel => desk.filter.clear(),
                Edit::Changed | Edit::Submit | Edit::Ignored => {}
            }
            desk.list_scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }));
        let add_form = self.add_open.then(|| self.add_form(window, cx));
        let looking = board.looking;
        let details_on = self.open.devices;
        let on_found = self.tab == HostTab::Discovered;
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
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(tabs)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            // Fixed width so "Looking…" coming and going moves nothing.
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_color(rgb(MUTED))
                                    .when(looking, |row| row.child("Looking…")),
                            )
                            .when(on_found, |row| {
                                row.child(small_chip(
                                    "found-details",
                                    if details_on {
                                        "Hide details"
                                    } else {
                                        "Details"
                                    },
                                    None,
                                    true,
                                    cx.listener(|desk, _, _, cx| {
                                        desk.open.devices = !desk.open.devices;
                                        cx.notify();
                                    }),
                                ))
                            })
                            .child(refresh_chip(cx.listener(|desk, _, _, cx| {
                                desk.scan.refresh(desk.nic.clone());
                                cx.notify();
                            }))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(filter_field)
                    .child(small_chip(
                        "add-host",
                        if self.add_open { "Close" } else { "Add host" },
                        Some(PLAYER_BLUE),
                        true,
                        cx.listener(|desk, _, window, cx| {
                            desk.add_open = !desk.add_open;
                            desk.add_error = None;
                            if desk.add_open {
                                window.focus(&desk.add_name_focus);
                            }
                            cx.notify();
                        }),
                    )),
            )
            .when_some(add_form, |col, form| col.child(form))
            .child(
                div()
                    .relative()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .child(
                        div()
                            .id("host-list")
                            .flex()
                            .flex_col()
                            .w_full()
                            // Fixed, so switching tabs or a short list moves nothing below.
                            .h(px(HOST_LIST_MAX_H))
                            .overflow_y_scroll()
                            .overflow_x_hidden()
                            .track_scroll(&self.list_scroll)
                            .on_scroll_wheel(cx.listener(|desk, _: &ScrollWheelEvent, _, cx| {
                                // The list scrolled; keep the page where it is.
                                if max_scroll(&desk.list_scroll) > 0.5 {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(div().w_full().pr(px(BAR_W + 2.)).child(list)),
                    )
                    .child(self.scrollbar(Bar::List, cx)),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn saved_row(
        &mut self,
        index: usize,
        name: String,
        addr: HostAddr,
        address: String,
        presence: Presence,
        now: u64,
        idle: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.is_target(&addr);
        let renaming = self
            .renaming
            .as_ref()
            .filter(|(at, _)| *at == index)
            .map(|(_, text)| text.clone());
        let confirming = self.confirm_delete == Some(index);
        let title: AnyElement = match &renaming {
            Some(text) => text_field(
                "rename-field",
                &self.rename_focus,
                self.rename_focus.is_focused(window),
                text,
                "Name",
            )
            .h(px(26.))
            .on_click(cx.listener(|desk, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                window.focus(&desk.rename_focus);
            }))
            .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, _, cx| {
                let Some((at, text)) = desk.renaming.as_mut() else {
                    return;
                };
                let at = *at;
                match edit_text(text, ev, cx) {
                    Edit::Submit => desk.finish_rename(at),
                    Edit::Cancel => desk.renaming = None,
                    Edit::Changed | Edit::Ignored => {}
                }
                cx.notify();
            }))
            .into_any_element(),
            None => row_title(name.to_string()).into_any_element(),
        };
        let actions = if confirming {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap_1()
                .child(div().text_color(rgb(DANGER)).child("Delete?"))
                .child(small_chip(
                    SharedString::from(format!("saved-del-yes-{index}")),
                    "Delete",
                    Some(DANGER),
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.book.delete_saved(index);
                        desk.confirm_delete = None;
                        desk.renaming = None;
                        desk.persist_hosts();
                        cx.notify();
                    }),
                ))
                .child(small_chip(
                    SharedString::from(format!("saved-del-no-{index}")),
                    "Keep",
                    None,
                    true,
                    cx.listener(|desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.confirm_delete = None;
                        cx.notify();
                    }),
                ))
        } else if renaming.is_some() {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .gap_1()
                .child(small_chip(
                    SharedString::from(format!("saved-ren-ok-{index}")),
                    "Save",
                    Some(PLAYER_BLUE),
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.finish_rename(index);
                        cx.notify();
                    }),
                ))
                .child(small_chip(
                    SharedString::from(format!("saved-ren-no-{index}")),
                    "Cancel",
                    None,
                    true,
                    cx.listener(|desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.renaming = None;
                        cx.notify();
                    }),
                ))
        } else {
            let current = name.to_string();
            div()
                .flex()
                .flex_row()
                .flex_none()
                .gap_1()
                .child(small_chip(
                    SharedString::from(format!("saved-ren-{index}")),
                    "Rename",
                    None,
                    true,
                    cx.listener(move |desk, _, window, cx| {
                        cx.stop_propagation();
                        desk.confirm_delete = None;
                        desk.renaming = Some((index, current.clone()));
                        window.focus(&desk.rename_focus);
                        cx.notify();
                    }),
                ))
                .child(small_chip(
                    SharedString::from(format!("saved-del-{index}")),
                    "Delete",
                    None,
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.renaming = None;
                        desk.confirm_delete = Some(index);
                        cx.notify();
                    }),
                ))
                .child(self.connect_chip(format!("saved-go-{index}"), name.clone(), addr, idle, cx))
        };
        let pick_name = name.to_string();
        let (lead, summary) = self.row_extras(&addr);
        host_row(
            SharedString::from(format!("saved-{index}")),
            Some(lead),
            title,
            address,
            summary,
            presence.label(now),
            presence_color(presence),
            selected,
            actions,
            cx.listener(move |desk, _: &ClickEvent, _, cx| {
                desk.pick(&pick_name, addr);
                cx.notify();
            }),
        )
        .into_any_element()
    }

    fn add_form(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = text_field(
            "add-name",
            &self.add_name_focus,
            self.add_name_focus.is_focused(window),
            &self.add_name,
            "Name (optional)",
        )
        .flex_1()
        .on_click(cx.listener(|desk, _: &ClickEvent, window, cx| {
            window.focus(&desk.add_name_focus);
            cx.notify();
        }))
        .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, window, cx| {
            if ev.keystroke.key == "tab" {
                window.focus(&desk.add_addr_focus);
            } else {
                match edit_text(&mut desk.add_name, ev, cx) {
                    Edit::Submit => desk.submit_add(),
                    Edit::Cancel => desk.add_open = false,
                    Edit::Changed | Edit::Ignored => {}
                }
            }
            cx.notify();
        }));
        let addr = text_field(
            "add-addr",
            &self.add_addr_focus,
            self.add_addr_focus.is_focused(window),
            &self.add_addr,
            "aa:bb:cc:dd:ee:ff or 192.168.0.5:4433",
        )
        .flex_1()
        .on_click(cx.listener(|desk, _: &ClickEvent, window, cx| {
            window.focus(&desk.add_addr_focus);
            cx.notify();
        }))
        .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, window, cx| {
            if ev.keystroke.key == "tab" {
                window.focus(&desk.add_name_focus);
            } else {
                match edit_text(&mut desk.add_addr, ev, cx) {
                    Edit::Submit => desk.submit_add(),
                    Edit::Cancel => desk.add_open = false,
                    Edit::Changed | Edit::Ignored => {}
                }
            }
            cx.notify();
        }));
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(name)
                    .child(addr)
                    .child(small_chip(
                        "add-save",
                        "Save",
                        Some(PLAYER_BLUE),
                        true,
                        cx.listener(|desk, _, _, cx| {
                            desk.submit_add();
                            cx.notify();
                        }),
                    )),
            )
            .child(match self.add_error.clone() {
                Some(err) => self.copyable_line("add-error", err, DANGER, cx).into_any_element(),
                None => div()
                    .w_full()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(
                    "A MAC address connects over Ethernet. An IP address, with an optional port, connects over UDP.",
                )
                    .into_any_element(),
            })
    }

    fn submit_add(&mut self) {
        match HostAddr::parse(&self.add_addr) {
            Ok(addr) => {
                if let Some(index) = self.book.saved_index(&addr) {
                    self.add_error = Some(format!(
                        "Already saved as \u{201c}{}\u{201d}.",
                        self.book.saved[index].name
                    ));
                    return;
                }
                self.book.save(&self.add_name, addr, now_secs());
                self.persist_hosts();
                self.add_name.clear();
                self.add_addr.clear();
                self.add_error = None;
                self.add_open = false;
                self.tab = HostTab::Saved;
            }
            Err(err) => self.add_error = Some(err),
        }
    }

    fn finish_rename(&mut self, index: usize) {
        if let Some((at, text)) = self.renaming.take() {
            if at == index {
                self.book.rename_saved(index, &text);
                self.persist_hosts();
            }
        }
    }

    fn set_tab(&mut self, tab: HostTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.renaming = None;
            self.confirm_delete = None;
            self.list_scroll.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    fn connect_chip(
        &mut self,
        id: String,
        name: String,
        addr: HostAddr,
        idle: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        small_chip(
            SharedString::from(id),
            "Connect",
            Some(PLAYER_BLUE),
            idle,
            cx.listener(move |desk, _, window, cx| {
                cx.stop_propagation();
                desk.pick(&name, addr);
                desk.open_typed_connection(cx);
                let _ = window;
            }),
        )
        .into_any_element()
    }

    /// Make this host the one Connect dials.
    fn pick(&mut self, name: &str, addr: HostAddr) {
        self.target_name = Some(name.to_string());
        match addr {
            HostAddr::Mac(mac) => {
                self.direct = None;
                self.peer = HexMac::from_bytes(mac);
                self.layer = Layer::L2;
            }
            HostAddr::Ip { .. } => {
                self.direct = Some(addr);
                self.peer = match self.roster.find(&addr) {
                    Some(row) => HexMac::from_bytes(row.mac),
                    None => HexMac::default(),
                };
            }
        }
    }

    fn is_target(&self, addr: &HostAddr) -> bool {
        match (self.direct, addr) {
            (Some(direct), _) => direct.same_host(addr),
            (None, HostAddr::Mac(mac)) => self.peer.parse() == Some(*mac),
            (None, HostAddr::Ip { .. }) => false,
        }
    }

    fn persist_hosts(&mut self) {
        if let Err(err) = store_hosts(&self.book) {
            self.message = Some(format!("Hosts were not saved: {err}"));
        }
    }

    fn saved_count(&self) -> usize {
        self.book
            .saved
            .iter()
            .filter(|entry| !self.here.owns_addr(&entry.addr))
            .count()
    }

    fn recent_count(&self) -> usize {
        self.book
            .recent
            .iter()
            .filter(|entry| !self.here.owns_addr(&entry.addr))
            .count()
    }

    /// Fold a finished discovery burst into the roster, and move a connect that
    /// came up into Recent. This computer's own answer is dropped.
    fn track_hosts(&mut self) {
        let board = self.scan.board();
        if board.burst != self.roster_burst {
            self.roster_burst = board.burst;
            self.here = ThisMachine::detect();
            let sightings: Vec<Sighting> = board
                .devices
                .iter()
                .map(|device| Sighting {
                    key: device.key,
                    mac: device.l2_mac,
                    caps: device.caps.clone(),
                    thumbnail: device.thumbnail.clone(),
                    release: device.release.clone(),
                    name: device.name.trim().to_string(),
                    ipv4: device.ipv4.clone(),
                    ipv6: device.ipv6.clone(),
                    port: device.l3_port,
                })
                .filter(|seen| !self.here.owns_sighting(seen))
                .collect();
            self.roster.update(&sightings, now_secs());
            self.roster.forget_self(&self.here);
        }
        if self.pending_recent.is_some() {
            match self.link.role() {
                Role::Client => {
                    if self.link.status().starts_with("Viewing ") {
                        if let Some((name, addr)) = self.pending_recent.take() {
                            self.book.record_recent(&name, addr, now_secs());
                            self.persist_hosts();
                        }
                    }
                }
                _ => self.pending_recent = None,
            }
        }
    }

    fn scroll_handle(&self, bar: Bar) -> &ScrollHandle {
        match bar {
            Bar::Page => &self.page_scroll,
            Bar::List => &self.list_scroll,
        }
    }

    /// Track and thumb drawn over the space the content leaves free on the right.
    fn scrollbar(&mut self, bar: Bar, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = self.scroll_handle(bar);
        let geometry = scrollbar::thumb(viewport(handle), max_scroll(handle), scrolled(handle));
        let dragging = self.drag.is_some_and(|drag| drag.bar == bar);
        let id = match bar {
            Bar::Page => "page-bar",
            Bar::List => "list-bar",
        };
        div()
            .id(id)
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(BAR_W))
            .when_some(geometry, |track, thumb| {
                track
                    .rounded_full()
                    .bg(rgb(0x1a1e27))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |desk, ev: &MouseDownEvent, _, cx| {
                            let handle = desk.scroll_handle(bar);
                            let top = f32::from(handle.bounds().origin.y);
                            let y = f32::from(ev.position.y) - top;
                            let down = y > thumb.top + thumb.height;
                            if y >= thumb.top && !down {
                                return;
                            }
                            let next = scrollbar::page(
                                scrolled(handle),
                                viewport(handle),
                                max_scroll(handle),
                                down,
                            );
                            handle.set_offset(point(px(0.), px(-next)));
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-thumb")))
                            .absolute()
                            .left(px(3.))
                            .w(px(BAR_W - 6.))
                            .top(px(thumb.top))
                            .h(px(thumb.height))
                            .rounded_full()
                            .bg(rgb(if dragging { 0x6b7587 } else { 0x434b5a }))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |desk, ev: &MouseDownEvent, _, cx| {
                                    let start = scrolled(desk.scroll_handle(bar));
                                    desk.drag = Some(Drag {
                                        bar,
                                        start_y: f32::from(ev.position.y),
                                        start,
                                    });
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ),
                    )
            })
    }

    fn set_quality(&mut self, quality: u8, persist: bool) {
        let quality = quality.clamp(1, 100);
        if self.settings.quality == quality {
            if persist {
                self.apply_settings(true);
            }
            return;
        }
        self.settings.quality = quality;
        self.apply_settings(persist);
    }

    fn set_quality_from_x(&mut self, x: f32, left: f32, width: f32, persist: bool) {
        let t = ((x - left) / width.max(1.)).clamp(0., 1.);
        self.set_quality(((t * 99.0).round() as u8).saturating_add(1), persist);
    }

    fn yuv_pick_row(&mut self, id: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let yuv = self.settings.yuv;
        div()
            .flex()
            .flex_row()
            .gap_2()
            .w_full()
            .child(pick_row(
                SharedString::from(format!("{id}-chroma")),
                format!("Chroma {}", yuv.chroma.label()),
                cx.listener(|desk, _, _, cx| {
                    let all = YuvChroma::ALL;
                    let i = all
                        .iter()
                        .position(|item| *item == desk.settings.yuv.chroma)
                        .unwrap_or(0);
                    desk.edit_settings(|settings| {
                        settings.yuv.chroma = all[(i + 1) % all.len()];
                    });
                    cx.notify();
                }),
            ))
            .child(pick_row(
                SharedString::from(format!("{id}-depth")),
                yuv.depth.label().to_string(),
                cx.listener(|desk, _, _, cx| {
                    let all = YuvDepth::ALL;
                    let i = all
                        .iter()
                        .position(|item| *item == desk.settings.yuv.depth)
                        .unwrap_or(0);
                    desk.edit_settings(|settings| {
                        settings.yuv.depth = all[(i + 1) % all.len()];
                    });
                    cx.notify();
                }),
            ))
    }

    fn chosen_nic(&self) -> Option<&NicChoice> {
        if let Some(name) = self.nic.as_deref() {
            return self.nics.iter().find(|nic| nic.name == name);
        }
        self.nics
            .iter()
            .find(|nic| nic_shown(nic) && nic.speed_mbps.is_some())
            .or_else(|| self.nics.iter().find(|nic| nic_shown(nic)))
    }

    fn path_peer_stats(&self) -> (String, f32, f32) {
        if let Some(mac) = self.peer.parse() {
            let device = self
                .scan
                .board()
                .devices
                .into_iter()
                .find(|device| device.l2_mac == mac);
            let computer = self
                .target_name
                .clone()
                .or_else(|| device.as_ref().map(|device| device.name.clone()))
                .or_else(|| self.roster.find(&HostAddr::Mac(mac)).map(|row| row.title()))
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format_mac(mac));
            let (latency_ms, jitter_ms) = device
                .map(|device| (device.stats.avg_ms, device.stats.jitter_ms))
                .unwrap_or((0.0, 0.0));
            return (computer, latency_ms, jitter_ms);
        }
        let computer = if self.share.name.trim().is_empty() {
            "this computer".into()
        } else {
            self.share.name.clone()
        };
        (computer, 0.0, 0.0)
    }

    fn estimate_path(&self) -> Option<crate::path_budget::Path> {
        let nic = self.chosen_nic()?;
        let bandwidth_mbps = nic.speed_mbps?;
        let (computer, latency_ms, jitter_ms) = self.path_peer_stats();
        Some(crate::path_budget::Path {
            nic: nic_label(nic),
            bandwidth_mbps,
            latency_ms,
            jitter_ms,
            computer,
        })
    }

    fn path_resolution(&self) -> (u32, u32) {
        if let Some(caps) = self.selected_row().and_then(|row| row.caps) {
            let monitor = caps
                .monitors
                .iter()
                .find(|monitor| monitor.primary)
                .or_else(|| caps.monitors.first());
            if let Some(monitor) = monitor {
                if monitor.width > 0 && monitor.height > 0 {
                    return (u32::from(monitor.width), u32::from(monitor.height));
                }
            }
        }
        (1920, 1080)
    }

    fn refresh_preview(&mut self) {
        let key = crate::quality_preview::Key {
            pref: self.preview_picture_pref(),
            quality: self.settings.quality.clamp(1, 100),
            yuv: self.settings.yuv,
            fps: self.preview_fps(),
        };
        if self.preview_key == Some(key) {
            return;
        }
        let sample = crate::quality_preview::simulate(key);
        self.preview_image = preview_render_image(&sample.rgb, sample.width, sample.height);
        self.preview_line = sample.line;
        self.preview_anchors = sample.rates.to_vec();
        self.preview_rates = sample
            .rates
            .iter()
            .map(|(w, h, kbps)| (format!("{w}×{h}"), *kbps))
            .collect();
        self.preview_key = Some(key);
    }

    fn picture_quality_slider(
        &mut self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.refresh_preview();
        let quality = self.settings.quality.clamp(1, 100);
        let track = Rc::new(Cell::new((0f32, 1f32)));
        let mark = Rc::clone(&track);
        let (width, height) = self.path_resolution();
        let kbps = crate::quality_preview::interpolate_kbps(&self.preview_anchors, width, height);
        let path = self.estimate_path();
        let load = path.as_ref().map(|path| path.load(kbps));
        let quality_color = load.as_ref().map(|load| load.color).unwrap_or(TEXT);
        let fill = load.as_ref().map(|load| load.color).unwrap_or(PLAYER_BLUE);
        let warn = load.and_then(|load| match load.level {
            crate::path_budget::WarnLevel::Ok => None,
            level => load.tip.map(|tip| (level, tip)),
        });
        let estimate = format!(
            "{width}×{height} · {}",
            crate::quality_preview::format_kbps(kbps)
        );
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(quality_color))
                            .child(format!("Quality {quality}%")),
                    )
                    .when_some(warn.clone(), |row, (level, tip)| {
                        row.child(warn_triangle(
                            SharedString::from(format!("{id}-warn")),
                            level,
                            tip,
                        ))
                    })
                    .child(choice_chip(
                        SharedString::from(format!("{id}-minus")),
                        "−",
                        false,
                        48.,
                        cx.listener(move |desk, _, _, cx| {
                            desk.set_quality(desk.settings.quality.saturating_sub(1).max(1), true);
                            cx.notify();
                        }),
                    ))
                    .child(choice_chip(
                        SharedString::from(format!("{id}-plus")),
                        "+",
                        false,
                        48.,
                        cx.listener(move |desk, _, _, cx| {
                            desk.set_quality(
                                desk.settings.quality.saturating_add(1).min(100),
                                true,
                            );
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .id(id)
                    .relative()
                    .h(px(22.))
                    .w_full()
                    .rounded_full()
                    .bg(rgb(0x1a1e27))
                    .border_1()
                    .border_color(rgb(CARD_EDGE))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let track = Rc::clone(&track);
                            move |desk, ev: &MouseDownEvent, _, cx| {
                                let (left, width) = track.get();
                                desk.quality_drag = Some((left, width.max(1.)));
                                desk.set_quality_from_x(
                                    f32::from(ev.position.x),
                                    left,
                                    width,
                                    false,
                                );
                                cx.stop_propagation();
                                cx.notify();
                            }
                        }),
                    )
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                mark.set((
                                    f32::from(bounds.origin.x),
                                    f32::from(bounds.size.width).max(1.),
                                ));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(relative(quality as f32 / 100.))
                            .rounded_full()
                            .bg(rgb(fill)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(quality_color))
                            .child(estimate),
                    )
                    .when_some(warn, |row, (level, tip)| {
                        row.child(warn_triangle(
                            SharedString::from(format!("{id}-rate-warn")),
                            level,
                            tip,
                        ))
                    }),
            )
    }

    fn preview_picture_pref(&self) -> PicturePref {
        if let Some(mac) = self.peer.parse() {
            if let Some(predicted) = self.predicted(mac) {
                return predicted.video;
            }
            if let Some(pin) = self.session_video.or_else(|| {
                self.connections
                    .get(&mac_key(mac))
                    .and_then(|over| over.video)
            }) {
                return pin;
            }
        }
        let encodes = self.link.role() == Role::Host;
        choose_codec(
            &self.video_pref,
            &self.local_video,
            &self.local_video,
            encodes,
            self.skip_without_hw,
        )
        .0
        .unwrap_or(PicturePref::Mjpeg)
    }

    fn preview_fps(&self) -> u32 {
        self.capture_fps()
    }

    fn quality_preview_row(&mut self) -> impl IntoElement {
        self.refresh_preview();
        let picture = self.preview_image.clone();
        let line = self.preview_line.clone();
        let rates = self.preview_rates.clone();
        let path = self.estimate_path();
        div()
            .flex()
            .flex_row()
            .gap_3()
            .w_full()
            .min_w_0()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(128.))
                    .h(px(128.))
                    .rounded_sm()
                    .overflow_hidden()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(CARD_EDGE))
                    .when_some(picture, |slot, picture| {
                        slot.child(
                            img(ImageSource::Render(picture))
                                .w(px(128.))
                                .h(px(128.))
                                .object_fit(ObjectFit::Fill),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_color(rgb(TEXT))
                            .font_weight(FontWeight::BOLD)
                            .child(line),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_x_4()
                            .gap_y_0()
                            .children(rates.into_iter().enumerate().map(|(index, (size, kbps))| {
                                let load = path.as_ref().map(|path| path.load(kbps));
                                let color = load.as_ref().map(|load| load.color).unwrap_or(TEXT);
                                let warn = load.and_then(|load| match load.level {
                                    crate::path_budget::WarnLevel::Ok => None,
                                    level => load.tip.map(|tip| (level, tip)),
                                });
                                div()
                                    .flex()
                                    .flex_row()
                                    .w(px(188.))
                                    .items_center()
                                    .justify_between()
                                    .text_xs()
                                    .child(div().text_color(rgb(MUTED)).child(size))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_color(rgb(color))
                                                    .child(crate::quality_preview::format_kbps(kbps)),
                                            )
                                            .when_some(warn, |row, (level, tip)| {
                                                row.child(warn_triangle(
                                                    SharedString::from(format!(
                                                        "rate-warn-{index}"
                                                    )),
                                                    level,
                                                    tip,
                                                ))
                                            }),
                                    )
                            })),
                    )
                    .child(muted_line(
                        "128×128 crop of a default start menu. MJPEG, MJ2K, and PyroWave round-trip the crop; other codecs show colour plus JPEG compression. Bitrate is estimated at seven sizes; anything in between interpolates by pixel area.".into(),
                    )),
            )
    }

    fn on_drag_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.overlay_run.stick.is_some() {
            self.overlay_stick_drag(ev, cx);
            return;
        }
        if let Some((left, width)) = self.quality_drag {
            if ev.pressed_button != Some(MouseButton::Left) {
                self.quality_drag = None;
                self.apply_settings(true);
                cx.notify();
                return;
            }
            self.set_quality_from_x(f32::from(ev.position.x), left, width, false);
            cx.notify();
            return;
        }
        let Some(drag) = self.drag else {
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let handle = self.scroll_handle(drag.bar);
        let max = max_scroll(handle);
        let Some(thumb) = scrollbar::thumb(viewport(handle), max, scrolled(handle)) else {
            return;
        };
        let next = scrollbar::drag_to(
            drag.start,
            f32::from(ev.position.y) - drag.start_y,
            max,
            thumb.travel,
        );
        handle.set_offset(point(px(0.), px(-next)));
        cx.notify();
    }

    /// Scrollbar inputs come from the last layout. When they change, draw once
    /// more so the thumb matches the content that was just laid out.
    fn settle_scrollbars(&mut self, window: &mut Window) {
        let sig = [
            viewport(&self.page_scroll),
            max_scroll(&self.page_scroll),
            viewport(&self.list_scroll),
            max_scroll(&self.list_scroll),
        ];
        if sig != self.bar_sig {
            self.bar_sig = sig;
            window.request_animation_frame();
        }
    }

    fn apply_backend_order(&mut self) {
        self.backend = etherquic::pick_backend(&self.backend_pref, &self.backends);
        self.local_audio = audio_for(self.backends, self.backend);
    }

    fn preferences_now(&self) -> Preferences {
        Preferences {
            video: self.video_pref.clone(),
            video_off: self.video_off.clone(),
            audio: self.audio_pref.clone(),
            opus: self.backend_pref.clone(),
            skip_without_hw: self.skip_without_hw,
            trace_input: self.trace_input,
            latency_graph: self.latency_graph,
            latency_log: self.latency_log,
            fps: self.fps,
            audio_bitrate_kbps: self.audio_bitrate_kbps,
            audio_channels: self.audio_channels,
        }
    }

    fn applied(&self, mac: [u8; 6]) -> Applied {
        apply_override(&self.preferences_now(), self.connections.get(&mac_key(mac)))
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
        let mut over = self.connections.get(&key).cloned().unwrap_or_default();
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
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
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
        row = row.child(choice_chip(
            SharedString::from(format!("{id_prefix}-match")),
            "Match remote",
            selected == FrameRate::Match,
            140.,
            cx.listener(|desk, _, _, cx| {
                desk.fps = FrameRate::Match;
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
        let audio_line = device.as_ref().map(|device| {
            audio_match_line(&applied.audio, &self.local_audio, device.audio.as_deref())
        });
        let picture = self.picture_pin_row(mac, stored.as_ref().and_then(|over| over.video), cx);
        let sound = self.audio_pin_row(mac, stored.as_ref().and_then(|over| over.audio), cx);
        let rate = self.connection_fps_row(mac, stored.as_ref().and_then(|over| over.fps), cx);
        let skip_on = applied.skip_without_hw;
        let trace_on = applied.trace_input;
        let custom = applied.custom;
        let dirty_on = self.settings.dirty_rects;
        let quality = self.picture_quality_slider("conn-quality", cx);
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
            .child(section_label("Quality"))
            .child(quality)
            .child(muted_line(
                "Applies to an open view at once. MJPEG uses this as JPEG quality (7% is very small files). PyroWave and the other codecs scale their default bitrate from 40%.".into(),
            ))
            .child(self.yuv_pick_row("conn-yuv", cx))
            .child(muted_line(
                "PyroWave colour asked of this host. 4:2:2 is sent as 4:4:4 after a horizontal chroma upsample.".into(),
            ))
            .child(self.quality_preview_row())
            .child(toggle_row(
                "conn-dirty-rects",
                "Send only changed tiles",
                dirty_on,
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| settings.dirty_rects = !settings.dirty_rects);
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Asked of this host, like the picture codec. If it supports dirty rectangles it skips an unchanged picture, and with MJPEG or MJ2K it encodes only the tiles that moved.".into(),
            ))
            .child(section_label("Audio"))
            .child(sound)
            .when_some(audio_line, |panel, line| panel.child(muted_line(line)))
            .child(section_label("Rate"))
            .child(rate)
            .child(muted_line(format!(
                "Auto is {SOFTWARE_FPS} fps for software encode and {HARDWARE_FPS} fps for hardware. Match remote follows the host display. 60 and 120 are always available; the host captures at its display rate with no 30 fps cap."
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
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
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
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
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
        let mut row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .w_full()
            .min_w_0();
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
        row = row.child(choice_chip(
            "conn-fps-match",
            "Match remote",
            pin == Some(FrameRate::Match),
            140.,
            cx.listener(move |desk, _, _, cx| {
                desk.edit_connection(mac, |over| over.fps = Some(FrameRate::Match));
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
                    etherquic::set_capture_carrier(self.selected_carrier());
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

    /// The Settings summary's codec part: the last session's real codecs with
    /// the picked host, else what negotiation will pick, else the top of the list.
    fn summary_codecs(&self) -> String {
        if let Some(mac) = self.peer.parse() {
            if let Some(pair) = last_session(mac, None) {
                return format!("last session {pair}");
            }
            if let Some(predicted) = self.predicted(mac) {
                return format!("{} · {}", predicted.video.name(), predicted.audio.name());
            }
        }
        self.video_pref
            .first()
            .map(|pref| pref.name().to_string())
            .unwrap_or_else(|| "no video".into())
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
        if let Some(predicted) = self.predicted(mac) {
            let note = if predicted.negotiates {
                ""
            } else {
                " (host does not negotiate)"
            };
            return format!(
                "{} · {}{note} · {}",
                predicted.video.name(),
                predicted.audio.name(),
                crate::fps_headline(
                    applied.fps,
                    None,
                    matches!(
                        predicted.video,
                        PicturePref::Av1Hw | PicturePref::H265Hw | PicturePref::H264Hw
                    )
                ),
            );
        }
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

    /// Re-read this computer's sound devices and controllers.
    /// List sound devices and controllers off the UI thread. On macOS the
    /// first CoreAudio probe can wait on the microphone permission prompt, and
    /// the window must not wait with it.
    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        if self.devices_loading {
            return;
        }
        self.devices_loading = true;
        let task = cx.background_executor().spawn(async move {
            let (outputs, inputs) = etherquic_play::audio_devices();
            (outputs, inputs, etherquic_play::hostinfo::local_pads())
        });
        cx.spawn(async move |this, cx| {
            let (outputs, inputs, pads) = task.await;
            let _ = this.update(cx, |desk, cx| {
                desk.local_outputs = outputs;
                desk.local_inputs = inputs;
                desk.local_pads = pads;
                desk.devices_loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Push Settings to whatever reads them now: the open view, the next shared
    /// session's sound source, and the network driver. `persist` also saves them.
    fn apply_settings(&mut self, persist: bool) {
        let view = crate::live_view();
        let next = self.settings.view();
        match view.lock() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
        crate::set_host_audio_device(self.settings.share_device.clone());
        etherquic_play::etherquic_media::host::set_dirty_rects(self.settings.dirty_rects);
        let driver = self.settings.network.l2_id();
        etherquic::set_l2_preference(driver);
        let capture = match self.fps.wish() {
            etherquic::negotiate::FpsWish::Fixed(fps) => fps,
            _ => 0,
        };
        etherquic_play::etherquic_media::host::set_capture_fps(capture);
        if let Some(id) = self.tabs.active_id() {
            if self.link.role() == Role::Client {
                self.push_session(id, true);
            }
        }
        self.write_host_adjust();
        if persist {
            if let Err(err) = store_settings(&self.settings) {
                self.message = Some(format!("Settings were not saved: {err}"));
            }
        }
    }

    fn tune_now(&self) -> etherquic_play::SessionTune {
        etherquic_play::SessionTune {
            video: self
                .session_video
                .and_then(|pref| etherquic::negotiate::Video::from_token(pref.token())),
            audio: self
                .session_audio
                .and_then(|audio| etherquic::negotiate::Audio::from_token(audio.token())),
            carrier: None,
            fps: Some(self.fps.wish()),
        }
    }

    fn stage_session(&mut self, id: u64, peer: [u8; 6], udp: Option<std::net::SocketAddr>) {
        self.apply_saved_profile(peer);
        let view = etherquic_play::live(self.settings.view());
        let tune = etherquic_play::live_tune(self.tune_now());
        let (tx, rx) = std::sync::mpsc::channel();
        self.send_live(&view, &tune, &tx);
        self.session_live.insert(
            id,
            SessionLive {
                view: view.clone(),
                tune: tune.clone(),
                tx,
                peer,
                udp,
            },
        );
        crate::stage_live(view, tune, Some(rx));
    }

    fn send_live(
        &self,
        view: &etherquic_play::LiveView,
        tune: &etherquic_play::LiveTune,
        tx: &std::sync::mpsc::Sender<String>,
    ) {
        let view = etherquic_play::snapshot(view);
        let tune = etherquic_play::snapshot_tune(tune);
        let yes = |on: bool| if on { "yes" } else { "no" };
        let mut lines = vec![
            format!("set scale={}", view.scale.token()),
            format!("set look={}", view.look.token()),
            format!("set dither={}", view.dither.token()),
            format!("set quality={}", view.quality),
            format!("set dirty-rects={}", yes(view.dirty_rects)),
            format!("set debug-dirty-rects={}", yes(view.debug_dirty_rects)),
            format!("set cursor={}", view.cursor.token()),
            format!("set keyboard={}", yes(view.send_keyboard)),
            format!("set mouse={}", yes(view.send_mouse)),
            format!("set pads={}", yes(view.send_pads)),
            format!("set upscale={}", view.upscale.token()),
            format!("set frame-gen={}", view.frame_gen.token()),
            format!("set yuv={}", view.yuv.token()),
        ];
        if let Some(video) = tune.video {
            lines.push(format!("set video={}", video.token()));
        }
        if let Some(audio) = tune.audio {
            lines.push(format!("set audio={}", audio.token()));
        }
        if let Some(carrier) = tune.carrier {
            lines.push(format!("set carrier={carrier}"));
        }
        if let Some(fps) = tune.fps {
            lines.push(format!("set fps={}", fps.token()));
        }
        if self.trace_log.on {
            lines.push("set debug-trace=yes".into());
        }
        for line in lines {
            let _ = tx.send(line);
        }
    }

    fn push_session(&mut self, id: u64, from_settings: bool) {
        let Some(slot) = self.session_live.get(&id) else {
            return;
        };
        if from_settings {
            let next = self.settings.view();
            let tune = self.tune_now();
            match slot.view.lock() {
                Ok(mut guard) => *guard = next,
                Err(poisoned) => *poisoned.into_inner() = next,
            }
            match slot.tune.lock() {
                Ok(mut guard) => *guard = tune,
                Err(poisoned) => *poisoned.into_inner() = tune,
            }
        }
        let view = slot.view.clone();
        let tune = slot.tune.clone();
        let tx = slot.tx.clone();
        self.send_live(&view, &tune, &tx);
    }

    fn edit_session(
        &mut self,
        id: u64,
        edit: impl FnOnce(&mut etherquic_play::ViewOptions, &mut etherquic_play::SessionTune),
    ) {
        let Some(slot) = self.session_live.get(&id) else {
            return;
        };
        {
            let mut view = match slot.view.lock() {
                Ok(view) => view,
                Err(poisoned) => poisoned.into_inner(),
            };
            let mut tune = match slot.tune.lock() {
                Ok(tune) => tune,
                Err(poisoned) => poisoned.into_inner(),
            };
            edit(&mut view, &mut tune);
        }
        self.push_session(id, false);
    }

    fn drop_session(&mut self, id: u64) {
        self.session_live.remove(&id);
        if self.inspect == Some(id) {
            self.inspect = None;
        }
    }

    fn write_host_adjust(&self) {
        let adjust = etherquic::HostAdjust {
            dirty_rects: Some(self.settings.dirty_rects),
            capture_fps: match self.fps.wish() {
                etherquic::negotiate::FpsWish::Fixed(fps) => Some(fps),
                _ => None,
            },
            // Empty is Auto. All and any multi-select clear a nic the service already bound.
            nic: Some(self.listen.dial_nic().unwrap_or_default()),
            audio_device: self.settings.share_device.clone(),
        };
        let path = etherquic::adjust_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, etherquic::render_adjust(&adjust));
    }

    fn refresh_host_face(&mut self) {
        if self.host_face_at.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.host_face_at = Instant::now();
        let presence = etherquic::load_presence();
        self.service_running = presence.running;
        self.service_hosting = presence.hosting;
        self.service_guests = presence.guests;
        let installed = crate::desk_model::service_is_installed(
            cfg!(target_os = "macos") && crate::macos_service::service_installed(),
            etherquic::host_conf_installed(),
            self.service_running,
        );
        let peers = if self.link.role() == Role::Host {
            self.link.peers()
        } else {
            self.parked
                .values()
                .find(|link| link.role() == Role::Host)
                .map(|link| link.peers())
                .unwrap_or(0)
        };
        self.host_summary = crate::desk_model::HostFace {
            process_up: self.link.role() == Role::Host || self.parked.values().any(|link| link.role() == Role::Host),
            process_peers: peers,
            service_installed: installed,
            service_running: self.service_running,
            service_hosting: self.service_hosting,
            guests: self.service_guests.clone(),
        }
        .summary();
    }

    fn sync_outbound(&mut self) {
        let active = self.tabs.active_id();
        let mut open = Vec::new();
        for tab in self.tabs.iter() {
            let status = if Some(tab.id) == active {
                self.link.status()
            } else {
                self.parked
                    .get(&tab.id)
                    .map(|link| link.status())
                    .unwrap_or_else(|| "closed".into())
            };
            if self.trace_log.on {
                self.trace_log.observe(&status);
            }
            open.push((tab.id, tab.label.clone(), status));
        }
        for line in etherquic_play::debug_lines() {
            self.trace_log.observe(&line);
        }
        self.outbound = crate::desk_model::sync_outbound(&open, &mut self.layout);
    }

    fn on_session_click(&mut self, id: u64, clicks: usize, window: &mut Window) {
        match crate::desk_model::click_action(clicks) {
            crate::desk_model::Click::Select => {
                self.switch_connection(id);
                self.inspect = Some(id);
                self.quick = true;
            }
            crate::desk_model::Click::Focus => {
                self.switch_connection(id);
                let picture = self.link.picture_id();
                if picture != 0 {
                    etherquic_play::switch_picture(picture);
                }
                etherquic_play::request_raise();
                window.activate_window();
            }
        }
    }

    fn apply_shelf_cmds(&mut self, window: &mut Window) {
        for cmd in crate::desk_model::take_cmds() {
            match cmd {
                crate::desk_model::ShareCmd::Select(id) => {
                    self.switch_connection(id);
                    self.inspect = Some(id);
                    self.quick = true;
                }
                crate::desk_model::ShareCmd::Focus(id) => {
                    self.on_session_click(id, 2, window);
                }
                crate::desk_model::ShareCmd::Close(id) => self.close_connection(id),
                crate::desk_model::ShareCmd::ToggleQuick => self.quick = !self.quick,
            }
        }
    }

    fn publish_shelf(&self) {
        let active = self.tabs.active_id();
        let tabs = self
            .outbound
            .iter()
            .map(|row| crate::desk_model::TabSnap {
                id: row.id,
                label: row.label.clone(),
                badge: crate::desk_model::phase_badge(&row.phase),
                current: Some(row.id) == active,
                group: row.group,
            })
            .collect();
        crate::desk_model::publish_tabs(tabs, self.quick);
    }

    fn apply_listen(&mut self) {
        let nic = self.listen.dial_nic();
        let changed = self.nic != nic;
        self.nic = nic.clone();
        self.scan.refresh_nics(&self.listen.probe_names());
        self.refresh_share();
        self.write_host_adjust();
        if changed {
            self.restart_window_host(nic);
        }
    }

    fn restart_window_host(&mut self, nic: Option<String>) {
        let host_id = if self.link.role() == Role::Host {
            self.tabs.active_id()
        } else {
            self.parked
                .iter()
                .find(|(_, link)| link.role() == Role::Host)
                .map(|(id, _)| *id)
        };
        let Some(id) = host_id else {
            return;
        };
        self.close_connection(id);
        let id = self.begin_tab("This computer".into());
        if let Err(err) = self.link.host(nic, self.backend, self.fps.wish()) {
            self.abort_tab(id);
            self.message = Some(err);
        }
    }

    fn toggle_listen(&mut self, name: String) {
        if !self.listen.toggle(&name) {
            self.message = Some("At least one adapter stays on.".into());
            return;
        }
        self.apply_listen();
    }

    /// Open the inspected player again with the current adapter and layer.
    fn redial_inspected(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.inspect.or_else(|| self.tabs.active_id()) else {
            return;
        };
        let (peer, udp) = match self.session_live.get(&id) {
            Some(slot) => (slot.peer, slot.udp),
            None => {
                self.open_typed_connection(cx);
                return;
            }
        };
        match crate::desk_model::plan_redial(self.link.role() == Role::Client, peer, udp) {
            crate::desk_model::Redial::NotClient => {
                self.message = Some("Interface and layer apply to a player connection.".into());
                cx.notify();
            }
            crate::desk_model::Redial::Address(addr) => {
                if let Some(current) = self.tabs.active_id() {
                    self.close_connection(current);
                }
                self.connect_direct(addr.ip(), addr.port(), cx);
            }
            crate::desk_model::Redial::Peer(peer) => {
                if let Some(current) = self.tabs.active_id() {
                    self.close_connection(current);
                }
                self.direct = None;
                self.peer = HexMac::from_bytes(peer);
                self.open_typed_connection(cx);
            }
        }
    }

    fn edit_settings(&mut self, edit: impl FnOnce(&mut Settings)) {
        edit(&mut self.settings);
        self.apply_settings(true);
    }

    fn check_updates(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.update,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. }
        ) {
            return;
        }
        self.update = UpdateStatus::Checking;
        let beta = self.settings.beta_releases;
        let task = cx
            .background_executor()
            .spawn(async move { update::check(beta) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |desk, cx| {
                desk.update = match result {
                    Ok(status) => status,
                    Err(err) => UpdateStatus::Failed(err),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateStatus::Available(offer) = self.update.clone() else {
            return;
        };
        let tag = offer.tag.clone();
        self.update = UpdateStatus::Downloading { tag: tag.clone() };
        let task = cx
            .background_executor()
            .spawn(async move { update::install(&offer) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |desk, cx| {
                desk.update = match result {
                    Ok(detail) => UpdateStatus::Installed { tag, detail },
                    Err(err) => UpdateStatus::Failed(err),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn open_update_page(&mut self) {
        let url = match &self.update {
            UpdateStatus::Available(offer) => offer.page.clone(),
            UpdateStatus::NoPackage { page, .. } => page.clone(),
            _ => format!("https://github.com/{}/releases", update::REPO),
        };
        if let Err(err) = update::open_url(&url) {
            self.update = UpdateStatus::Failed(err);
        }
    }

    /// Make the current value of `fields` the user's default.
    fn make_default(&mut self, fields: &[Field]) {
        for field in fields {
            self.defaults.take(&self.settings, *field);
        }
        if let Err(err) = store_defaults(&self.defaults) {
            self.message = Some(format!("Defaults were not saved: {err}"));
        }
    }

    /// Put a section, or everything with `None`, back to the user's defaults.
    /// `factory` also forgets those defaults first.
    fn reset_settings(&mut self, section: Option<Section>, factory: bool) {
        if factory {
            self.defaults = Settings::default();
            if let Err(err) = store_defaults(&self.defaults) {
                self.message = Some(format!("Defaults were not saved: {err}"));
            }
        }
        let base = self.defaults.clone();
        let sections: Vec<Section> = match section {
            Some(section) => vec![section],
            None => Section::ALL.to_vec(),
        };
        for section in sections {
            self.settings.reset(section, &base);
            match section {
                Section::Capture => self.fps = FrameRate::Auto,
                Section::Sound => {
                    self.backend_pref = etherquic::default_backend_order().to_vec();
                    self.apply_backend_order();
                    self.audio_bitrate_kbps = 0;
                    self.audio_channels = 0;
                }
                Section::Codecs => {
                    self.video_pref = default_video();
                    self.video_off.clear();
                    self.audio_pref = default_audio();
                    self.skip_without_hw = false;
                    self.session_video = None;
                    self.session_audio = None;
                }
                Section::Input => {
                    self.trace_input = false;
                    self.latency_graph = true;
                    self.latency_log = false;
                    etherquic_play::set_graph(true);
                    etherquic_play::set_log(false);
                }
                Section::Appearance | Section::Network | Section::Language | Section::Updates => {}
            }
        }
        self.persist();
        self.apply_settings(true);
    }

    /// Scroll the page so Settings starts near the top, once it is laid out.
    fn follow_settings_scroll(&mut self, window: &mut Window) {
        if self.scroll_to_settings == 0 {
            return;
        }
        self.scroll_to_settings -= 1;
        if self.scroll_to_settings > 0 {
            window.request_animation_frame();
            return;
        }
        let handle = &self.page_scroll;
        let top = f32::from(handle.bounds().origin.y);
        let now = -f32::from(handle.offset().y);
        let want = (self.settings_y.get() - top + now - 8.).clamp(0., max_scroll(handle).max(0.));
        handle.set_offset(point(px(0.), px(-want)));
        window.request_animation_frame();
    }

    /// The host Connect will dial, as discovery last heard it.
    fn selected_row(&self) -> Option<RosterRow> {
        if let Some(addr) = self.direct {
            return self.roster.find(&addr).cloned();
        }
        let mac = self.peer.parse()?;
        self.roster.find(&HostAddr::Mac(mac)).cloned()
    }

    /// Ethernet, IPv4, and IPv6 for the picked host. Picking one fills the field.
    fn transport_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = self.selected_row();
        let mac = row
            .as_ref()
            .map(|row| row.mac)
            .or_else(|| self.peer.parse());
        let (direct_v4, direct_v6, direct_port) = match self.direct {
            Some(HostAddr::Ip {
                ip: IpAddr::V4(ip),
                port,
            }) => (Some(ip), None, port),
            Some(HostAddr::Ip {
                ip: IpAddr::V6(ip),
                port,
            }) => (None, Some(ip), port),
            _ => (None, None, 0),
        };
        let v4 = row
            .as_ref()
            .and_then(|row| row.ipv4.first().copied())
            .or(direct_v4);
        let v6 = row
            .as_ref()
            .and_then(|row| row.ipv6.first().copied())
            .or(direct_v6);
        let port = row
            .as_ref()
            .map(|row| row.port)
            .filter(|port| *port != 0)
            .unwrap_or(direct_port);
        let (all_v4, all_v6) = row
            .as_ref()
            .map(|row| (row.ipv4.clone(), row.ipv6.clone()))
            .unwrap_or_default();
        let active = match self.direct {
            Some(HostAddr::Ip {
                ip: IpAddr::V4(_), ..
            }) => 1,
            Some(HostAddr::Ip {
                ip: IpAddr::V6(_), ..
            }) => 2,
            _ => match active_layer(self.layer, &all_v4, &all_v6) {
                Layer::L2 => 0,
                Layer::V4(_) => 1,
                Layer::V6(_) => 2,
            },
        };
        let tab = |id: &'static str,
                   (kind, address): (&'static str, String),
                   on: bool,
                   enabled: bool,
                   why: Option<&'static str>| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h(px(38.))
                .px_2()
                .justify_center()
                .overflow_hidden()
                .rounded_sm()
                .border_1()
                .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
                .bg(rgb(if on { 0x243044 } else { BG }))
                .text_xs()
                .text_color(rgb(if on { TEXT } else { MUTED }))
                .when(!enabled, |tab| tab.opacity(0.45))
                .when(enabled, |tab| tab.cursor_pointer())
                .when_some(why, |tab, why| tab.tooltip(why_tip(why)))
                .child(
                    div()
                        .w_full()
                        .h(px(16.))
                        .line_height(px(16.))
                        .overflow_hidden()
                        .when(on, |line| line.font_weight(FontWeight::BOLD))
                        .child(kind),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(16.))
                        .line_height(px(16.))
                        .overflow_hidden()
                        .whitespace_normal()
                        .child(address),
                )
        };
        let l2_why = etherquic::l2_unavailable_reason();
        let ether_ok = mac.is_some() && l2_why.is_none();
        let ether_label = (
            "Ethernet",
            mac.map(format_mac)
                .unwrap_or_else(|| "no MAC address".into()),
        );
        let v4_label = (
            "IPv4",
            v4.map(|ip| ip.to_string())
                .unwrap_or_else(|| "not announced".into()),
        );
        let v6_label = (
            "IPv6",
            v6.map(|ip| ip.to_string())
                .unwrap_or_else(|| "not announced".into()),
        );
        div()
            .flex()
            .flex_row()
            .gap_1()
            .w_full()
            .min_w_0()
            .child({
                let tab = tab(
                    "via-l2",
                    ether_label,
                    active == 0 && ether_ok,
                    ether_ok,
                    l2_why,
                );
                if ether_ok {
                    if let Some(mac) = mac {
                        tab.on_click(cx.listener(move |desk, _, _, cx| {
                            desk.direct = None;
                            desk.layer = Layer::L2;
                            desk.peer = HexMac::from_bytes(mac);
                            cx.notify();
                        }))
                    } else {
                        tab
                    }
                } else if let Some(why) = l2_why {
                    tab.on_click(cx.listener(move |desk, _, _, cx| {
                        desk.message = Some(why.into());
                        cx.notify();
                    }))
                } else {
                    tab
                }
            })
            .child(
                tab("via-v4", v4_label, active == 1, v4.is_some(), None).when_some(
                    v4,
                    |tab, ip| {
                        tab.on_click(cx.listener(move |desk, _, _, cx| {
                            desk.direct = Some(HostAddr::Ip {
                                ip: IpAddr::V4(ip),
                                port,
                            });
                            desk.layer = Layer::V4(ip);
                            cx.notify();
                        }))
                    },
                ),
            )
            .child(
                tab("via-v6", v6_label, active == 2, v6.is_some(), None).when_some(
                    v6,
                    |tab, ip| {
                        tab.on_click(cx.listener(move |desk, _, _, cx| {
                            desk.direct = Some(HostAddr::Ip {
                                ip: IpAddr::V6(ip),
                                port,
                            });
                            desk.layer = Layer::V6(ip);
                            cx.notify();
                        }))
                    },
                ),
            )
    }

    /// The discovery record of the picked host, for its codecs.
    fn selected_device(&self) -> Option<Found> {
        let mac = self.peer.parse()?;
        self.scan
            .board()
            .devices
            .into_iter()
            .find(|device| device.l2_mac == mac)
    }

    /// The offer for `mac`: the applied order with this connection's pins first.
    fn codec_offer(&self, mac: Option<[u8; 6]>) -> etherquic_play::CodecChoice {
        let applied = match mac {
            Some(mac) => self.applied(mac),
            None => apply_override(&self.preferences_now(), None),
        };
        let saved = mac
            .and_then(|mac| self.connections.get(&mac_key(mac)).cloned())
            .unwrap_or_default();
        codec_choice_for(
            &applied,
            self.session_video.or(saved.video),
            self.session_audio.or(saved.audio),
            self.settings.quality,
            self.settings.dirty_rects,
            self.settings.yuv,
            self.settings.view_only,
            self.settings.exclusive,
            saved.carrier,
        )
    }

    /// What negotiation will pick with a discovered host. `None` when the
    /// host has not been seen or this computer's probe is still running.
    fn predicted(&self, mac: [u8; 6]) -> Option<Predicted> {
        let device = self
            .scan
            .board()
            .devices
            .into_iter()
            .find(|device| device.l2_mac == mac)?;
        predicted_pair(&self.codec_offer(Some(mac)), device.caps.as_ref())
    }

    /// Video and audio this connection will use, with the unsaved pick first.
    fn codec_choice(&self) -> (Option<PicturePref>, Option<AudioId>, bool, bool) {
        let mac = self.peer.parse();
        let applied = match mac {
            Some(mac) => self.applied(mac),
            None => apply_override(&self.preferences_now(), None),
        };
        let pins = mac.and_then(|mac| self.connections.get(&mac_key(mac)).cloned());
        let mut video = applied.video.clone();
        if let Some(pick) = self.session_video {
            move_to_front(&mut video, pick);
        }
        let mut audio = applied.audio.clone();
        if let Some(pick) = self.session_audio {
            move_to_front(&mut audio, pick);
        }
        let device = self.selected_device();
        // Auto names what negotiation will pick with this host, once this
        // computer's probe is done and the host has been seen.
        if let Some(predicted) = mac.and_then(|mac| self.predicted(mac)) {
            let pinned_v = pins.as_ref().is_some_and(|over| over.video.is_some());
            let pinned_a = pins.as_ref().is_some_and(|over| over.audio.is_some());
            return (
                Some(predicted.video),
                Some(predicted.audio),
                pinned_v,
                pinned_a,
            );
        }
        let chosen_v = chosen_video(
            &video,
            &self.local_video,
            device.as_ref().and_then(|device| device.video.as_deref()),
            applied.skip_without_hw,
        );
        let chosen_a = chosen_audio(
            &audio,
            &self.local_audio,
            device.as_ref().and_then(|device| device.audio.as_deref()),
        );
        let pinned_v = pins.as_ref().is_some_and(|over| over.video.is_some());
        let pinned_a = pins.as_ref().is_some_and(|over| over.audio.is_some());
        (chosen_v, chosen_a, pinned_v, pinned_a)
    }

    /// One codec dropdown with its "Save for" choice when it was changed.
    fn codec_row(&mut self, which: Menu, cx: &mut Context<Self>) -> impl IntoElement {
        let (video, audio, pinned_v, pinned_a) = self.codec_choice();
        let (label, value, overridden, pinned) = match which {
            Menu::Video => (
                "Video",
                video.map(|pref| pref.name()).unwrap_or("none fits"),
                self.session_video.is_some(),
                pinned_v,
            ),
            Menu::Audio => (
                "Audio",
                audio.map(|codec| codec.name()).unwrap_or("none fits"),
                self.session_audio.is_some(),
                pinned_a,
            ),
            Menu::Fps
            | Menu::AudioBitrate
            | Menu::AudioChannels
            | Menu::QuickFps
            | Menu::Look
            | Menu::Upscale
            | Menu::FrameGen => ("", "none", false, false),
        };
        let shown = if overridden {
            value.to_string()
        } else if pinned {
            format!("{value} · saved for host")
        } else {
            format!("Auto · {value}")
        };
        let choices_ready = self.connect_choices_ready();
        let open = self.menu == Some(which) && choices_ready;
        let id = match which {
            Menu::Video => "codec-video",
            Menu::Audio => "codec-audio",
            Menu::Fps => "connect-fps",
            Menu::AudioBitrate => "connect-audio-kbps",
            Menu::AudioChannels => "connect-audio-ch",
            Menu::QuickFps => "quick-fps",
            Menu::Look => "quick-look",
            Menu::Upscale => "quick-upscale",
            Menu::FrameGen => "quick-frame-gen",
        };
        let mac = self.peer.parse();
        let name = self.target_label();
        // The host encodes. This computer decodes. A host that has not been
        // seen, or that sent no host-video lines, is not blamed for a missing encoder.
        let device = self.selected_device();
        let host_caps = device.as_ref().and_then(|device| {
            device
                .caps
                .as_ref()
                .and_then(|caps| HostCaps::from_text(&caps.codecs))
        });
        let heard = device
            .as_ref()
            .map(|device| remote_audio(device.audio.as_deref()));
        let skip_without_hw = match mac {
            Some(mac) => self.applied(mac).skip_without_hw,
            None => self.skip_without_hw,
        };
        let facts = OverrideFacts {
            probe: etherquic_play::etherquic_media::caps::ready(),
            host: host_caps.as_ref(),
            host_known: device.is_some(),
            skip_without_hw,
            local: &self.local_video,
            local_audio: &self.local_audio,
            remote_audio: heard.as_ref(),
        };
        let items: Vec<(String, Option<usize>)> = match which {
            Menu::Video => std::iter::once(("Auto".to_string(), None))
                .chain(
                    PicturePref::order()
                        .into_iter()
                        .enumerate()
                        .map(|(i, pref)| (pref.name().to_string(), Some(i))),
                )
                .collect(),
            Menu::Audio => std::iter::once(("Auto".to_string(), None))
                .chain(
                    AudioId::order()
                        .into_iter()
                        .enumerate()
                        .map(|(i, codec)| (codec.name().to_string(), Some(i))),
                )
                .collect(),
            Menu::Fps
            | Menu::AudioBitrate
            | Menu::AudioChannels
            | Menu::QuickFps
            | Menu::Look
            | Menu::Upscale
            | Menu::FrameGen => Vec::new(),
        };
        let mut menu = div()
            .id(SharedString::from(format!("{id}-menu")))
            .flex()
            .flex_col()
            .w(px(200.))
            .p_1()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(PLAYER_BLUE))
            .rounded_md()
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|desk, _, _, cx| {
                desk.menu = None;
                cx.notify();
            }));
        for (index, (text, pick)) in items.into_iter().enumerate() {
            let on = match (pick, which) {
                (None, _) => !overridden && !pinned,
                (Some(i), Menu::Video) => {
                    overridden && self.session_video == PicturePref::order().get(i).copied()
                }
                (Some(i), Menu::Audio) => {
                    overridden && self.session_audio == AudioId::order().get(i).copied()
                }
                (Some(_), _) => false,
            };
            let why = match (which, pick) {
                (Menu::Video, Some(i)) => PicturePref::order()
                    .get(i)
                    .copied()
                    .and_then(|pref| video_override_reason(pref, &facts)),
                (Menu::Audio, Some(i)) => AudioId::order()
                    .get(i)
                    .copied()
                    .and_then(|codec| audio_override_reason(codec, &facts)),
                _ => None,
            };
            menu = menu.child(
                div()
                    .id(SharedString::from(format!("{id}-item-{index}")))
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .text_color(rgb(if why.is_some() { MUTED } else { TEXT }))
                    .when(on, |item| {
                        item.bg(rgb(0x243044)).font_weight(FontWeight::BOLD)
                    })
                    .when(why.is_none(), |item| {
                        item.cursor_pointer()
                            .hover(|item| item.bg(rgb(0x2a3242)))
                            .on_click(cx.listener(move |desk, _, _, cx| {
                                match (which, pick) {
                                    (Menu::Video, None) => {
                                        desk.session_video = None;
                                        if let Some(mac) = mac {
                                            desk.edit_connection(mac, |over| over.video = None);
                                        }
                                    }
                                    (Menu::Audio, None) => {
                                        desk.session_audio = None;
                                        if let Some(mac) = mac {
                                            desk.edit_connection(mac, |over| over.audio = None);
                                        }
                                    }
                                    (Menu::Video, Some(i)) => {
                                        desk.session_video = PicturePref::order().get(i).copied()
                                    }
                                    (Menu::Audio, Some(i)) => {
                                        desk.session_audio = AudioId::order().get(i).copied()
                                    }
                                    _ => {}
                                }
                                desk.menu = None;
                                cx.notify();
                            }))
                    })
                    .when_some(why, |item, why| {
                        item.opacity(0.6)
                            .cursor_default()
                            .tooltip(shared_tip(why))
                    })
                    .justify_between()
                    .child(text)
                    .when(pick.is_none(), |item| item.child(faint_default())),
            );
        }
        let save_for = overridden.then(|| {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap_1()
                .text_xs()
                .child(div().text_color(rgb(MUTED)).child("Save for:"))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-save-host")))
                        .max_w(px(100.))
                        .h(px(24.))
                        .px_2()
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(CARD_EDGE))
                        .when(mac.is_none(), |chip| chip.opacity(0.45))
                        .when(mac.is_some(), |chip| chip.cursor_pointer())
                        .child(
                            div()
                                .h(px(16.))
                                .line_height(px(16.))
                                .overflow_hidden()
                                .whitespace_normal()
                                .child(name.clone()),
                        )
                        .when_some(mac, |chip, mac| {
                            chip.on_click(cx.listener(move |desk, _, _, cx| {
                                match which {
                                    Menu::Video => {
                                        let pick = desk.session_video.take();
                                        desk.edit_connection(mac, |over| over.video = pick);
                                    }
                                    Menu::Audio => {
                                        let pick = desk.session_audio.take();
                                        desk.edit_connection(mac, |over| over.audio = pick);
                                    }
                                    _ => {}
                                }
                                cx.notify();
                            }))
                        }),
                )
                .child(div().text_color(rgb(MUTED)).child("|"))
                .child(small_chip(
                    SharedString::from(format!("{id}-save-global")),
                    "Global",
                    None,
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        match which {
                            Menu::Video => {
                                if let Some(pick) = desk.session_video.take() {
                                    move_to_front(&mut desk.video_pref, pick);
                                }
                            }
                            Menu::Audio => {
                                if let Some(pick) = desk.session_audio.take() {
                                    move_to_front(&mut desk.audio_pref, pick);
                                }
                            }
                            _ => {}
                        }
                        desk.persist();
                        cx.notify();
                    }),
                ))
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_1()
            .min_w_0()
            .h(px(30.))
            .child(
                div()
                    .w(px(40.))
                    .flex_none()
                    .text_color(rgb(MUTED))
                    .child(label),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        div()
                            .id(id)
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w(px(150.))
                            .h(px(28.))
                            .px_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(
                                if choices_ready && overridden {
                                    PLAYER_BLUE
                                } else {
                                    CARD_EDGE
                                },
                            ))
                            .bg(rgb(BG))
                            .text_color(rgb(if choices_ready { TEXT } else { MUTED }))
                            .when(!choices_ready, |chip| {
                                chip.opacity(0.45).cursor_default().tooltip(why_tip(
                                    "Pick a computer or enter its MAC address first",
                                ))
                            })
                            .when(choices_ready, |chip| {
                                chip.cursor_pointer().on_click(cx.listener(
                                    move |desk, _, _, cx| {
                                        desk.menu = if desk.menu == Some(which) {
                                            None
                                        } else {
                                            Some(which)
                                        };
                                        cx.notify();
                                    },
                                ))
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .h(px(18.))
                                    .line_height(px(18.))
                                    .overflow_hidden()
                                    .whitespace_normal()
                                    .child(shown),
                            )
                            .child(chevron(open)),
                    )
                    .when(open, |anchor| {
                        anchor.child(div().absolute().top(px(30.)).left_0().child(
                            deferred(anchored().snap_to_window().child(menu)).with_priority(1),
                        ))
                    }),
            )
            .when_some(save_for, |row, save| row.child(save))
    }

    fn side_pick_shown(&self, which: Menu) -> (String, f32) {
        match which {
            Menu::Fps | Menu::QuickFps => {
                let shown = match self.fps {
                    FrameRate::Auto => "Auto".to_string(),
                    FrameRate::Match => "Match".to_string(),
                    FrameRate::Fixed(fps) => format!("{fps}"),
                };
                (shown, 88.)
            }
            Menu::AudioBitrate => {
                let shown = if self.audio_bitrate_kbps == 0 {
                    "Auto".to_string()
                } else {
                    format!(
                        "{} kbps",
                        scale_audio_kbps(self.audio_bitrate_kbps, self.audio_channels)
                    )
                };
                (shown, 108.)
            }
            Menu::AudioChannels => {
                let shown = if self.audio_channels == 0 {
                    "Auto".to_string()
                } else {
                    audio_layout_name(self.audio_channels).to_string()
                };
                (shown, 88.)
            }
            Menu::Video | Menu::Audio => ("Auto".to_string(), 88.),
            Menu::Look => (self.settings.look.label().to_string(), 140.),
            Menu::Upscale => (self.settings.upscale.label().to_string(), 88.),
            Menu::FrameGen => (self.settings.frame_gen.label().to_string(), 72.),
        }
    }

    fn side_pick_items(&self, which: Menu) -> Vec<(String, bool, bool)> {
        match which {
            Menu::Fps | Menu::QuickFps => {
                let mut items = vec![
                    ("Auto".to_string(), self.fps == FrameRate::Auto, true),
                    (
                        "Match remote".to_string(),
                        self.fps == FrameRate::Match,
                        false,
                    ),
                ];
                for fps in FPS_CHOICES {
                    items.push((format!("{fps}"), self.fps == FrameRate::Fixed(fps), false));
                }
                if let FrameRate::Fixed(fps) = self.fps {
                    if !FPS_CHOICES.contains(&fps) {
                        items.push((format!("{fps}"), true, false));
                    }
                }
                items
            }
            Menu::AudioBitrate => {
                let mut items = vec![(
                    format!("Auto · {} kbps", scale_audio_kbps(0, self.audio_channels)),
                    self.audio_bitrate_kbps == 0,
                    true,
                )];
                for kbps in AUDIO_BITRATE_CHOICES {
                    let shown = scale_audio_kbps(kbps, self.audio_channels);
                    items.push((
                        format!("{shown} kbps"),
                        self.audio_bitrate_kbps == kbps,
                        false,
                    ));
                }
                if self.audio_bitrate_kbps != 0
                    && !AUDIO_BITRATE_CHOICES.contains(&self.audio_bitrate_kbps)
                {
                    let shown = scale_audio_kbps(self.audio_bitrate_kbps, self.audio_channels);
                    items.push((format!("{shown} kbps"), true, false));
                }
                items
            }
            Menu::AudioChannels => AUDIO_LAYOUTS
                .iter()
                .map(|layout| {
                    let label = if layout.channels == 0 {
                        "Auto · Stereo".to_string()
                    } else {
                        layout.name.to_string()
                    };
                    (
                        label,
                        self.audio_channels == layout.channels,
                        layout.channels == 0,
                    )
                })
                .collect(),
            Menu::Video | Menu::Audio => Vec::new(),
            Menu::Look => Look::ALL
                .into_iter()
                .map(|look| {
                    (
                        look.label().to_string(),
                        self.settings.look == look,
                        look == Look::Off,
                    )
                })
                .collect(),
            Menu::Upscale => Upscale::ALL
                .into_iter()
                .map(|mode| {
                    (
                        mode.label().to_string(),
                        self.settings.upscale == mode,
                        mode == Upscale::Off,
                    )
                })
                .collect(),
            Menu::FrameGen => FrameGen::ALL
                .into_iter()
                .map(|mode| {
                    (
                        mode.label().to_string(),
                        self.settings.frame_gen == mode,
                        mode == FrameGen::Off,
                    )
                })
                .collect(),
        }
    }

    fn apply_side_pick(&mut self, which: Menu, index: usize) {
        match which {
            Menu::Fps | Menu::QuickFps => {
                self.fps = match index {
                    0 => FrameRate::Auto,
                    1 => FrameRate::Match,
                    i => {
                        let extra = if let FrameRate::Fixed(fps) = self.fps {
                            (!FPS_CHOICES.contains(&fps)).then_some(fps)
                        } else {
                            None
                        };
                        if let Some(fps) = FPS_CHOICES.get(i.saturating_sub(2)).copied() {
                            FrameRate::Fixed(fps)
                        } else if let Some(fps) = extra {
                            FrameRate::Fixed(fps)
                        } else {
                            self.fps
                        }
                    }
                };
            }
            Menu::AudioBitrate => {
                self.audio_bitrate_kbps = if index == 0 {
                    0
                } else {
                    AUDIO_BITRATE_CHOICES
                        .get(index - 1)
                        .copied()
                        .unwrap_or(self.audio_bitrate_kbps)
                };
            }
            Menu::AudioChannels => {
                if let Some(layout) = AUDIO_LAYOUTS.get(index) {
                    self.audio_channels = layout.channels;
                }
            }
            Menu::Video | Menu::Audio => {}
            Menu::Look => {
                if let Some(look) = Look::ALL.get(index).copied() {
                    self.edit_settings(|settings| settings.look = look);
                }
                self.menu = None;
                return;
            }
            Menu::Upscale => {
                if let Some(mode) = Upscale::ALL.get(index).copied() {
                    self.edit_settings(|settings| settings.upscale = mode);
                }
                self.menu = None;
                return;
            }
            Menu::FrameGen => {
                if let Some(mode) = FrameGen::ALL.get(index).copied() {
                    self.edit_settings(|settings| settings.frame_gen = mode);
                }
                self.menu = None;
                return;
            }
        }
        self.menu = None;
        self.persist();
    }

    fn side_pick(&mut self, which: Menu, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, width) = self.side_pick_shown(which);
        let items = self.side_pick_items(which);
        let session_menu = matches!(
            which,
            Menu::Fps | Menu::AudioBitrate | Menu::AudioChannels
        );
        let choices_ready = !session_menu || self.connect_choices_ready();
        let open = self.menu == Some(which) && choices_ready;
        let id = match which {
            Menu::Fps => "connect-fps",
            Menu::AudioBitrate => "connect-audio-kbps",
            Menu::AudioChannels => "connect-audio-ch",
            Menu::Video => "codec-video",
            Menu::Audio => "codec-audio",
            Menu::QuickFps => "quick-fps",
            Menu::Look => "quick-look",
            Menu::Upscale => "quick-upscale",
            Menu::FrameGen => "quick-frame-gen",
        };
        let mut menu = div()
            .id(SharedString::from(format!("{id}-menu")))
            .flex()
            .flex_col()
            .w(px(width.max(140.)))
            .p_1()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(PLAYER_BLUE))
            .rounded_md()
            .shadow_lg()
            .when(which == Menu::Look, |menu| {
                menu.max_h(px(320.)).overflow_y_scroll()
            })
            .on_mouse_down_out(cx.listener(|desk, _, _, cx| {
                desk.menu = None;
                cx.notify();
            }));
        for (index, (text, on, is_default)) in items.into_iter().enumerate() {
            menu = menu.child(
                div()
                    .id(SharedString::from(format!("{id}-item-{index}")))
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(on, |item| {
                        item.bg(rgb(0x243044)).font_weight(FontWeight::BOLD)
                    })
                    .hover(|item| item.bg(rgb(0x2a3242)))
                    .justify_between()
                    .child(text)
                    .when(is_default, |item| item.child(faint_default()))
                    .on_click(cx.listener(move |desk, _, _, cx| {
                        desk.apply_side_pick(which, index);
                        cx.notify();
                    })),
            );
        }
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(id)
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w(px(width))
                    .h(px(28.))
                    .px_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(CARD_EDGE))
                    .bg(rgb(BG))
                    .text_color(rgb(if choices_ready { TEXT } else { MUTED }))
                    .when(!choices_ready, |chip| {
                        chip.opacity(0.45).cursor_default().tooltip(why_tip(
                            "Pick a computer or enter its MAC address first",
                        ))
                    })
                    .when(choices_ready, |chip| {
                        chip.cursor_pointer().on_click(cx.listener(move |desk, _, _, cx| {
                            desk.menu = if desk.menu == Some(which) {
                                None
                            } else {
                                Some(which)
                            };
                            cx.notify();
                        }))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .h(px(18.))
                            .line_height(px(18.))
                            .overflow_hidden()
                            .whitespace_normal()
                            .child(shown),
                    )
                    .child(chevron(open)),
            )
            .when(open, |anchor| {
                anchor.child(
                    div()
                        .absolute()
                        .top(px(30.))
                        .left_0()
                        .child(deferred(anchored().snap_to_window().child(menu)).with_priority(1)),
                )
            })
    }

    fn quick_label(&self, text: &'static str) -> impl IntoElement {
        div()
            .flex_none()
            .text_xs()
            .text_color(rgb(MUTED))
            .child(text)
    }

    /// A Settings section: title, the backend this part runs on, and Reset.
    fn section(
        &mut self,
        section: Section,
        backends: Option<AnyElement>,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reset = self.reset_control(Some(section), cx);
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .w(px(120.))
                            .font_weight(FontWeight::BOLD)
                            .child(section.title()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .when_some(backends, |row, list| row.child(list)),
                    )
                    .child(reset),
            )
            .child(body)
    }

    /// Reset, then "Reset? / Cancel" in the same fixed box, so nothing moves.
    fn reset_control(
        &mut self,
        target: Option<Section>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = match target {
            Some(section) => section.title().to_ascii_lowercase().replace(' ', "-"),
            None => "all".into(),
        };
        let asking = self.confirm_reset == Some(target);
        let custom = self.defaults != Settings::default();
        let label: &'static str = if target.is_none() {
            "Reset all"
        } else {
            "Reset"
        };
        let mut slot = div()
            .flex()
            .flex_row()
            .flex_none()
            .justify_end()
            .items_center()
            .gap_1()
            .w(px(if target.is_none() { 300. } else { 190. }));
        if asking {
            let factory_too = target.is_none() && custom;
            slot = slot
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(DANGER))
                        .child(if factory_too {
                            "Reset to:"
                        } else {
                            "Reset to defaults?"
                        }),
                )
                .child(small_chip(
                    SharedString::from(format!("reset-yes-{key}")),
                    if factory_too { "My defaults" } else { "Reset" },
                    Some(DANGER),
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        desk.reset_settings(target, false);
                        desk.confirm_reset = None;
                        cx.notify();
                    }),
                ))
                .when(factory_too, |slot| {
                    slot.child(small_chip(
                        "reset-factory",
                        "Factory",
                        Some(DANGER),
                        true,
                        cx.listener(|desk, _, _, cx| {
                            desk.reset_settings(None, true);
                            desk.confirm_reset = None;
                            cx.notify();
                        }),
                    ))
                })
                .child(small_chip(
                    SharedString::from(format!("reset-no-{key}")),
                    "Cancel",
                    None,
                    true,
                    cx.listener(|desk, _, _, cx| {
                        desk.confirm_reset = None;
                        cx.notify();
                    }),
                ));
        } else {
            slot = slot.child(small_chip(
                SharedString::from(format!("reset-{key}")),
                label,
                None,
                true,
                cx.listener(move |desk, _, _, cx| {
                    desk.confirm_reset = Some(target);
                    cx.notify();
                }),
            ));
        }
        slot
    }

    /// A setting's label, and "Make default" when its value is not the default.
    /// The row has a fixed height, so the button appearing moves nothing.
    fn field_head(
        &mut self,
        label: &'static str,
        fields: &'static [Field],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let changed = fields
            .iter()
            .any(|field| !self.settings.same(&self.defaults, *field));
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(24.))
            .min_w_0()
            .child(section_label(label))
            .when(changed, |row| {
                row.child(small_chip(
                    SharedString::from(format!("make-default-{label}")),
                    "Make default",
                    None,
                    true,
                    cx.listener(move |desk, _, _, cx| {
                        desk.make_default(fields);
                        cx.notify();
                    }),
                ))
            })
    }

    /// Backends for one part. Only a real choice is clickable; the rest are
    /// shown greyed with why.
    fn backend_row(&mut self, part: Part, cx: &mut Context<Self>) -> AnyElement {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let list = backend_list(part, wayland);
        let mut row = div().flex().flex_row().flex_wrap().gap_1().min_w_0();
        for (index, backend) in list.into_iter().enumerate() {
            let id = SharedString::from(format!("backend-{part:?}-{index}"));
            let (on, enabled, note) = match &backend.state {
                BackendState::Active => (true, true, None),
                BackendState::Available => {
                    let driver = match backend.label {
                        "Membrane" => NetworkDriver::Membrane,
                        "Npcap" => NetworkDriver::Npcap,
                        _ => NetworkDriver::Automatic,
                    };
                    let note = (self.defaults.network == driver).then_some("default");
                    (self.settings.network == driver, true, note)
                }
                BackendState::Unavailable(why) => (false, false, Some(*why)),
            };
            let selectable = matches!(backend.state, BackendState::Available);
            let label = backend.label;
            row = row.child(
                backend_chip(id, label, note, on, enabled)
                    .when(selectable, |chip| {
                        chip.cursor_pointer()
                            .on_click(cx.listener(move |desk, _, _, cx| {
                                let driver = match label {
                                    "Membrane" => NetworkDriver::Membrane,
                                    "Npcap" => NetworkDriver::Npcap,
                                    _ => NetworkDriver::Automatic,
                                };
                                desk.edit_settings(|settings| settings.network = driver);
                                cx.notify();
                            }))
                    })
                    .when(!enabled, |chip| {
                        chip.when_some(note, |chip, why| {
                            chip.on_click(cx.listener(move |desk, _, _, cx| {
                                desk.message = Some(why.into());
                                cx.notify();
                            }))
                        })
                    }),
            );
        }
        row.into_any_element()
    }

    /// The media backend is the Opus tool order. The first installed one runs.
    fn media_backend_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div().flex().flex_row().flex_wrap().gap_1().min_w_0();
        for (index, tool) in etherquic::default_backend_order()
            .iter()
            .copied()
            .enumerate()
        {
            let present = self.backends.has(tool);
            let on = present && tool == self.backend;
            let note = (!present).then_some("not installed");
            row = row.child(
                backend_chip(
                    SharedString::from(format!("backend-media-{index}")),
                    backend_label(tool),
                    note,
                    on,
                    present,
                )
                .when(present, |chip| {
                    chip.cursor_pointer()
                        .on_click(cx.listener(move |desk, _, _, cx| {
                            move_to_front(&mut desk.backend_pref, tool);
                            desk.apply_backend_order();
                            desk.persist();
                            cx.notify();
                        }))
                }),
            );
        }
        row.into_any_element()
    }

    fn settings_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let remote = self.selected_row();
        let remote_caps = remote.as_ref().and_then(|row| row.caps.clone());
        let remote_name = remote
            .as_ref()
            .map(|row| row.title())
            .unwrap_or_else(|| self.target_label());
        let reset_all = self.reset_control(None, cx);

        // Appearance
        let render = self.backend_row(Part::Render, cx);
        let scale = self.settings.scale;
        let size_match = self.settings.size_match;
        let upscale = self.settings.upscale;
        let frame_gen = self.settings.frame_gen;
        let look = self.settings.look;
        let dither = self.settings.dither;
        let pixel_size = self.settings.pixel_size;
        let dither_strength = self.settings.dither_strength;
        let default_scale = self.defaults.scale;
        let default_size = self.defaults.size_match;
        let default_upscale = self.defaults.upscale;
        let default_frame_gen = self.defaults.frame_gen;
        let default_look = self.defaults.look;
        let default_dither = self.defaults.dither;
        let default_pixel_size = self.defaults.pixel_size;
        let default_strength = self.defaults.dither_strength;
        let mut scales = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for mode in ScaleMode::ALL {
            scales = scales.child(with_default(
                choice_chip(
                    SharedString::from(format!("scale-{}", mode.token())),
                    mode.label(),
                    scale == mode,
                    96.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.scale = mode);
                        cx.notify();
                    }),
                ),
                default_scale == mode,
            ));
        }
        let mut sizes = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for choice in SizeMatch::ALL {
            let chip = choice_chip(
                SharedString::from(format!("size-{}", choice.token())),
                choice.label(),
                size_match == choice,
                200.,
                cx.listener(move |desk, _, _, cx| {
                    if choice.available() {
                        desk.edit_settings(|settings| settings.size_match = choice);
                    }
                    cx.notify();
                }),
            );
            sizes = sizes.child(with_default(
                div()
                    .when(!choice.available(), |chip| chip.opacity(0.45))
                    .child(chip),
                default_size == choice,
            ));
        }
        let mut upscales = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for mode in Upscale::ALL {
            upscales = upscales.child(with_default(
                choice_chip(
                    SharedString::from(format!("upscale-{}", mode.token())),
                    mode.label(),
                    upscale == mode,
                    88.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.upscale = mode);
                        cx.notify();
                    }),
                ),
                default_upscale == mode,
            ));
        }
        let mut frame_gens = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for mode in FrameGen::ALL {
            frame_gens = frame_gens.child(with_default(
                choice_chip(
                    SharedString::from(format!("frame-gen-{}", mode.token())),
                    mode.label(),
                    frame_gen == mode,
                    88.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.frame_gen = mode);
                        cx.notify();
                    }),
                ),
                default_frame_gen == mode,
            ));
        }
        let mut looks = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for choice in Look::ALL {
            looks = looks.child(with_default(
                choice_chip(
                    SharedString::from(format!("look-{}", choice.token())),
                    choice.label(),
                    look == choice,
                    140.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.look = choice);
                        cx.notify();
                    }),
                ),
                default_look == choice,
            ));
        }
        let mut dithers = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for choice in Dither::ALL {
            dithers = dithers.child(with_default(
                choice_chip(
                    SharedString::from(format!("dither-{}", choice.token())),
                    choice.label(),
                    dither == choice,
                    160.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.dither = choice);
                        cx.notify();
                    }),
                ),
                default_dither == choice,
            ));
        }
        let mut sizes_px = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for size in [1u8, 2, 4, 8, 16] {
            sizes_px = sizes_px.child(with_default(
                choice_chip(
                    SharedString::from(format!("pixel-size-{size}")),
                    format!("{size} px"),
                    pixel_size == size,
                    72.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.pixel_size = size);
                        cx.notify();
                    }),
                ),
                default_pixel_size == size,
            ));
        }
        let mut strengths = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
        for amount in [0u8, 30, 60, 100] {
            strengths = strengths.child(with_default(
                choice_chip(
                    SharedString::from(format!("dither-strength-{amount}")),
                    format!("{amount}%"),
                    dither_strength == amount,
                    72.,
                    cx.listener(move |desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.dither_strength = amount);
                        cx.notify();
                    }),
                ),
                default_strength == amount,
            ));
        }
        let scale_head = self.field_head("Scale to fit", &[Field::Scale], cx);
        let size_head = self.field_head("Resolution matching", &[Field::SizeMatch], cx);
        let upscale_head = self.field_head("Client upscale", &[Field::Upscale], cx);
        let frame_gen_head = self.field_head("Frame generation", &[Field::FrameGen], cx);
        let look_head = self.field_head("Picture look", &[Field::Look], cx);
        let dither_head = self.field_head("Dither", &[Field::Dither], cx);
        let pixel_head = self.field_head("Pixel size", &[Field::PixelSize], cx);
        let strength_head = self.field_head("Dither strength", &[Field::DitherStrength], cx);
        let appearance_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(scale_head)
            .child(scales)
            .child(size_head)
            .child(sizes)
            .child(upscale_head)
            .child(upscales)
            .child(frame_gen_head)
            .child(frame_gens)
            .child(look_head)
            .child(looks)
            .child(pixel_head)
            .child(sizes_px)
            .child(dither_head)
            .child(dithers)
            .child(strength_head)
            .child(strengths)
            .child(muted_line(
                "Scale and resolution matching apply to an open view at once. Host matches window is not available: hosts do not change their resolution yet.".into(),
            ))
            .child(muted_line(
                "Client upscale asks the host for 720p and fills this window on the player. Auto is FSR, DLSS on NVIDIA, or XeSS on Intel. DLSS and XeSS use the FSR spatial path until those runtimes are present. Upscale also sits in the top-right of this window.".into(),
            ))
            .child(muted_line(
                "Frame generation synthesises one extra picture between arrivals so a 30 fps stream can paint at 60. Auto turns on when a discrete GPU is present. The player warps the latest picture along motion from the one before it; it does not wait for the next real frame. NVIDIA Vulkan FRUC (fruc_vulkan) needs FFmpeg master and Ampere optical flow; this path is a Vulkan compute interpolator that runs on Turing and later.".into(),
            ))
            .child(muted_line(
                "Picture look is painted by the host before it encodes, and it applies to an open view at once. Off leaves the capture. Blocks only downsamples. Poster flattens each channel. The other names are Lospec palettes from Image-to-Pixel. Pixel size is source pixels per block. Ordered dither (Bayer, cluster, lines, blue noise) is stable on video; error diffusion (Floyd–Steinberg and the rest) can flicker between frames. Ordered looks run as a GPU compute shader when a discrete adapter is present; a palette LUT keeps the CPU path under a millisecond. FPS and Shader also sit in the top-right of this window.".into(),
            ));
        let appearance = self.section(Section::Appearance, Some(render), appearance_body, cx);

        // Display Capture
        let capture_backends = self.backend_row(Part::Capture, cx);
        let capture = self.capture_fps();
        let rates = self.global_fps_row("pref-fps", cx);
        let dirty_rects = self.settings.dirty_rects;
        let dirty_head = self.field_head("Dirty rectangles", &[Field::DirtyRects], cx);
        let debug_dirty = self.settings.debug_dirty_rects;
        let debug_head = self.field_head("Show dirty tiles", &[Field::DebugDirtyRects], cx);
        let capture_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(section_label("Rate"))
            .child(rates)
            .child(muted_line(format!(
                "This PC captures at the display refresh (or 60 fps if that rate cannot be read). Encode Auto is {SOFTWARE_FPS} fps software / {HARDWARE_FPS} fps hardware. Match remote encodes every captured frame. The current encode estimate is {capture} fps."
            )))
            .child(dirty_head)
            .child(toggle_row(
                "dirty-rects",
                "Send only changed tiles",
                dirty_rects,
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| settings.dirty_rects = !settings.dirty_rects);
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "The same control as on the Connect card. When you view a host, it is sent in the offer. A host that supports it skips an unchanged picture. With MJPEG or MJ2K, only the changed tiles are encoded. Inter-frame codecs still encode a full picture when anything moved. When this PC hosts, it also applies for a player that does not send the request. Off by default.".into(),
            ))
            .child(section_label("Advanced"))
            .child(debug_head)
            .child(toggle_row(
                "debug-dirty-rects",
                "Show dirty tiles in the viewer",
                debug_dirty,
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| {
                        settings.debug_dirty_rects = !settings.debug_dirty_rects
                    });
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Draws a square on each 16-pixel tile that changed in the last picture. Turn on Send only changed tiles, then watch the viewer: a small cursor move lights a few tiles; a codec that always sends a full picture lights most of them.".into(),
            ));
        let capture_section =
            self.section(Section::Capture, Some(capture_backends), capture_body, cx);

        // Sound
        let media = self.media_backend_row(cx);
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
        let share_pick = self.settings.share_device.clone();
        let share_default = self.defaults.share_device.clone();
        let out_default = self.defaults.output_device.clone();
        let share_head = self.field_head(
            "This computer: input (shared when hosting)",
            &[Field::ShareDevice],
            cx,
        );
        let out_head = self.field_head(
            "This computer: output (remote sound plays here)",
            &[Field::OutputDevice],
            cx,
        );
        let mut shared = div().flex().flex_col().gap_1().w_full().min_w_0();
        shared = shared.child(device_chip(
            "share-auto".into(),
            "Desktop audio (automatic)".into(),
            share_pick.is_none(),
            share_default.is_none(),
            true,
            cx.listener(|desk, _, _, cx| {
                desk.edit_settings(|settings| settings.share_device = None);
                cx.notify();
            }),
        ));
        let mut sources: Vec<String> = self.local_inputs.clone();
        if cfg!(windows) {
            for name in &self.local_outputs {
                if !sources.contains(name) {
                    sources.push(name.clone());
                }
            }
        }
        let all_devices = self.open.all_devices;
        let hidden_sources = sources.len().saturating_sub(DEVICES_SHOWN);
        if !all_devices {
            sources.truncate(DEVICES_SHOWN);
        }
        for (index, name) in sources.into_iter().enumerate() {
            let on = share_pick.as_deref() == Some(name.as_str());
            let is_default = share_default.as_deref() == Some(name.as_str());
            let pick = name.clone();
            shared = shared.child(device_chip(
                SharedString::from(format!("share-{index}")),
                name,
                on,
                is_default,
                true,
                cx.listener(move |desk, _, _, cx| {
                    let pick = pick.clone();
                    desk.edit_settings(|settings| settings.share_device = Some(pick));
                    cx.notify();
                }),
            ));
        }
        let out_pick = self.settings.output_device.clone();
        let mut outputs = div().flex().flex_col().gap_1().w_full().min_w_0();
        outputs = outputs.child(device_chip(
            "out-default".into(),
            "System default".into(),
            out_pick.is_none(),
            out_default.is_none(),
            true,
            cx.listener(|desk, _, _, cx| {
                desk.edit_settings(|settings| settings.output_device = None);
                cx.notify();
            }),
        ));
        let mut sinks = self.local_outputs.clone();
        let hidden_sinks = sinks.len().saturating_sub(DEVICES_SHOWN);
        if !all_devices {
            sinks.truncate(DEVICES_SHOWN);
        }
        for (index, name) in sinks.into_iter().enumerate() {
            let on = out_pick.as_deref() == Some(name.as_str());
            let is_default = out_default.as_deref() == Some(name.as_str());
            let pick = name.clone();
            outputs = outputs.child(device_chip(
                SharedString::from(format!("out-{index}")),
                name,
                on,
                is_default,
                true,
                cx.listener(move |desk, _, _, cx| {
                    let pick = pick.clone();
                    desk.edit_settings(|settings| settings.output_device = Some(pick));
                    cx.notify();
                }),
            ));
        }
        let remote_list = |names: Option<&Vec<String>>, empty: &str| {
            let mut col = div().flex().flex_col().gap_1().w_full().min_w_0();
            match names.filter(|names| !names.is_empty()) {
                Some(names) => {
                    for (index, name) in names.iter().enumerate() {
                        col = col.child(info_chip(name.clone(), index == 0));
                    }
                }
                None => col = col.child(muted_line(empty.to_string())),
            }
            col
        };
        let none_heard = if remote_caps.is_some() {
            "None reported"
        } else {
            "Pick a discovered host to see its devices"
        };
        let remote_in = remote_list(
            remote_caps.as_ref().map(|caps| &caps.audio_inputs),
            none_heard,
        );
        let remote_out = remote_list(
            remote_caps.as_ref().map(|caps| &caps.audio_outputs),
            none_heard,
        );
        let sound_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(two_columns(
                div().flex().flex_col().gap_1().w_full().min_w_0().child(share_head).child(shared),
                div().flex().flex_col().gap_1().w_full().min_w_0().child(out_head).child(outputs),
            ))
            .child(two_columns(
                column_owned(format!("{remote_name}: input"), remote_in),
                column_owned(format!("{remote_name}: output (what it shares)"), remote_out),
            ))
            .child(muted_line(
                "The output applies when a view opens its sound. The input applies the next time this PC hosts. The remote's devices are chosen on that computer.".into(),
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(small_chip(
                        "sound-refresh",
                        "Refresh devices",
                        None,
                        !self.devices_loading,
                        cx.listener(|desk, _, _, cx| {
                            desk.refresh_devices(cx);
                            cx.notify();
                        }),
                    ))
                    .when(all_devices || hidden_sources + hidden_sinks > 0, |row| {
                        row.child(small_chip(
                            "sound-all",
                            if all_devices { "Show fewer" } else { "Show all devices" },
                            None,
                            true,
                            cx.listener(|desk, _, _, cx| {
                                desk.open.all_devices = !desk.open.all_devices;
                                cx.notify();
                            }),
                        ))
                    }),
            )
            .child(section_label("Opus tools"))
            .child(self.copyable_line(
                "opus-tools",
                backend_line(self.backends, self.backend),
                MUTED,
                cx,
            ))
            .child(tools)
            .child(muted_line(
                "The first installed tool in this order is announced, and used the next time this computer shares.".into(),
            ));
        let sound = self.section(Section::Sound, Some(media), sound_body, cx);

        // Codecs
        let mut video = div().flex().flex_col().gap_2().w_full().min_w_0();
        for (index, codec) in self.video_pref.iter().copied().enumerate() {
            let off = self.video_off.contains(&codec);
            // MJPEG is the floor every session can fall back to.
            let switchable = codec != PicturePref::Mjpeg;
            let row = pref_row(
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
            );
            video = video.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .when(off, |this| this.opacity(0.5))
                    .when(switchable, |this| {
                        this.child(order_chip(
                            SharedString::from(format!("vid-{index}-on")),
                            if off { "Off" } else { "On" },
                            cx.listener(move |desk, _, _, cx| {
                                if let Some(at) =
                                    desk.video_off.iter().position(|row| *row == codec)
                                {
                                    desk.video_off.remove(at);
                                } else {
                                    desk.video_off.push(codec);
                                }
                                desk.persist();
                                cx.notify();
                            }),
                        ))
                    })
                    .child(div().flex_1().min_w_0().child(row)),
            );
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
        let skip_without_hw = self.skip_without_hw;
        let quality_head = self.field_head("Picture quality", &[Field::Quality], cx);
        let quality_slider = self.picture_quality_slider("settings-quality", cx);
        let codecs_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(muted_line(
                "The default order for every computer. The first row both sides can run is the match. A computer can have its own pick: use Save for in the Connect card.".into(),
            ))
            .child(muted_line(self.this_pc_audio_line()))
            .child(self.codec_support_table("settings-codecs", cx))
            .child(two_columns(column("Video", video), column("Audio", audio)))
            .child(toggle_row(
                "skip-without-hw",
                "Skip when there's no HW encoder",
                skip_without_hw,
                cx.listener(|desk, _, _, cx| {
                    desk.skip_without_hw = !desk.skip_without_hw;
                    desk.persist();
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "A codec without a hardware encoder is skipped. MJPEG stays available. The desktop picture stays MJPEG, and desktop audio stays PCM.".into(),
            ))
            .child(quality_head)
            .child(quality_slider)
            .child(muted_line(
                "Applies to an open view at once. MJPEG uses this percent as JPEG quality. PyroWave and H.264/H.265/AV1 scale their default bitrate from 40%.".into(),
            ))
            .child(self.field_head("PyroWave colour", &[Field::Yuv], cx))
            .child(self.yuv_pick_row("settings-yuv", cx))
            .child(muted_line(
                "4:4:4 keeps every chroma sample. 4:2:2 is sent as 4:4:4 after a horizontal upsample. 10-bit feeds the encoder finer YUV. Older players stay on 4:2:0 8-bit.".into(),
            ))
            .child(self.quality_preview_row());
        let codecs = self.section(Section::Codecs, None, codecs_body, cx);

        // Input
        let input_backends = self.backend_row(Part::Input, cx);
        let keyboard = self.settings.send_keyboard;
        let mouse = self.settings.send_mouse;
        let pads = self.settings.send_pads;
        let clipboard_as_keystrokes = self.settings.clipboard_as_keystrokes;
        let cursor_mode = self.settings.cursor;
        let trace_input = self.trace_input;
        let send_head = self.field_head(
            "Send",
            &[
                Field::SendKeyboard,
                Field::SendMouse,
                Field::SendPads,
                Field::ClipboardAsKeystrokes,
            ],
            cx,
        );
        let slots_head = self.field_head("Controller to player slot", &[Field::PadSlots], cx);
        let default_pins = self.defaults.clone();
        let mut slots = div().flex().flex_col().gap_1().w_full().min_w_0();
        if self.local_pads.is_empty() {
            slots = slots.child(muted_line(
                "No controller is plugged in. Plug one in and press Refresh devices.".into(),
            ));
        }
        for (pad_index, name) in self.local_pads.clone().into_iter().enumerate() {
            let pinned = self.settings.pinned_slot(&name);
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(18.))
                        .line_height(px(18.))
                        .overflow_hidden()
                        .whitespace_normal()
                        .child(name.clone()),
                );
            let auto_name = name.clone();
            row = row.child(choice_chip(
                SharedString::from(format!("pad-{pad_index}-auto")),
                "Auto",
                pinned.is_none(),
                56.,
                cx.listener(move |desk, _, _, cx| {
                    let name = auto_name.clone();
                    desk.edit_settings(|settings| settings.pin_pad(&name, None));
                    cx.notify();
                }),
            ));
            for slot in 0..PAD_SLOTS {
                let slot_name = name.clone();
                row = row.child(choice_chip(
                    SharedString::from(format!("pad-{pad_index}-{slot}")),
                    format!("P{}", slot + 1),
                    pinned == Some(slot),
                    44.,
                    cx.listener(move |desk, _, _, cx| {
                        let name = slot_name.clone();
                        desk.edit_settings(|settings| settings.pin_pad(&name, Some(slot)));
                        cx.notify();
                    }),
                ));
            }
            let default_slot = default_pins.pinned_slot(&name);
            row = row.child(
                div()
                    .flex_none()
                    .w(px(64.))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .opacity(0.6)
                    .child(match default_slot {
                        Some(slot) => format!("default P{}", slot + 1),
                        None => "default Auto".to_string(),
                    }),
            );
            slots = slots.child(row);
        }
        let remote_pads = remote_caps.as_ref().map(|caps| {
            let virtual_pad = match caps.virtual_pad {
                etherquic::discover::VPAD_VIGEM => "ViGEmBus",
                etherquic::discover::VPAD_UINPUT => "uinput",
                _ => "none",
            };
            format!(
                "{remote_name}: virtual controller {virtual_pad}, {} controller(s) of its own.",
                caps.pads.len()
            )
        });
        let input_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(send_head)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .child(toggle_chip("send-keyboard", "Send keyboard", keyboard, Some(self.defaults.send_keyboard), cx.listener(|desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.send_keyboard = !settings.send_keyboard);
                        cx.notify();
                    })))
                    .child(toggle_chip("send-mouse", "Send mouse", mouse, Some(self.defaults.send_mouse), cx.listener(|desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.send_mouse = !settings.send_mouse);
                        cx.notify();
                    })))
                    .child(toggle_chip("send-pads", "Send controllers", pads, Some(self.defaults.send_pads), cx.listener(|desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.send_pads = !settings.send_pads);
                        cx.notify();
                    })))
                    .child(toggle_chip("trace-input", "Trace input timing", trace_input, Some(false), cx.listener(|desk, _, _, cx| {
                        desk.trace_input = !desk.trace_input;
                        desk.persist();
                        cx.notify();
                    })))
                    .child(toggle_chip("latency-graph", "Latency graph", self.latency_graph, Some(true), cx.listener(|desk, _, _, cx| {
                        desk.latency_graph = !desk.latency_graph;
                        etherquic_play::set_graph(desk.latency_graph);
                        let on = if desk.latency_graph { "yes" } else { "no" };
                        for slot in desk.session_live.values() {
                            let _ = slot.tx.send(format!("set latency-graph={on}"));
                        }
                        desk.persist();
                        cx.notify();
                    })))
                    .child(toggle_chip("latency-log", "Save latency log", self.latency_log, Some(false), cx.listener(|desk, _, _, cx| {
                        desk.latency_log = !desk.latency_log;
                        etherquic_play::set_log(desk.latency_log);
                        let on = if desk.latency_log { "yes" } else { "no" };
                        for slot in desk.session_live.values() {
                            let _ = slot.tx.send(format!("set latency-log={on}"));
                        }
                        desk.persist();
                        cx.notify();
                    })))
                    .child(toggle_chip("net-trace", "Network trace", self.trace_log.on, Some(false), cx.listener(|desk, _, _, cx| {
                        desk.trace_log.toggle();
                        etherquic_play::set_debug_trace(desk.trace_log.on);
                        let on = if desk.trace_log.on { "yes" } else { "no" };
                        for slot in desk.session_live.values() {
                            let _ = slot.tx.send(format!("set debug-trace={on}"));
                        }
                        cx.notify();
                    }))),
            )
            .child(self.field_head("Mouse cursor", &[Field::Cursor], cx))
            .child({
                let mut cursors = div().flex().flex_row().flex_wrap().gap_2().min_w_0();
                for mode in CursorMode::ALL {
                    cursors = cursors.child(choice_chip(
                        SharedString::from(format!("cursor-{}", mode.token())),
                        mode.label(),
                        cursor_mode == mode,
                        200.,
                        cx.listener(move |desk, _, _, cx| {
                            desk.edit_settings(|settings| settings.cursor = mode);
                            cx.notify();
                        }),
                    ));
                }
                cursors
            })
            .child(muted_line(
                "Native keeps this computer's cursor and changes its shape when the remote desktop would: a hand on a link, arrows on a window edge. Copy uses the remote cursor's picture for that. Remote hides this cursor and draws the remote one, including where it sits. The choice applies to an open view at once.".into(),
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(toggle_chip(
                        "clipboard-as-keystrokes",
                        "Send clipboard as keystrokes",
                        clipboard_as_keystrokes,
                        Some(self.defaults.clipboard_as_keystrokes),
                        cx.listener(|desk, _, _, cx| {
                            desk.edit_settings(|settings| {
                                settings.clipboard_as_keystrokes = !settings.clipboard_as_keystrokes;
                            });
                            cx.notify();
                        }),
                    ))
                    .child(small_chip(
                        "paste-clipboard",
                        "Paste clipboard",
                        Some(PLAYER_BLUE),
                        true,
                        cx.listener(|desk, _, _, cx| {
                            desk.paste_local_clipboard(cx);
                            cx.notify();
                        }),
                    )),
            )
            .child(muted_line(
                "Send toggles apply to an open view at once. Trace input asks the remote to confirm each key and click and prints that on the title. The latency graph draws input until that confirmation, picture from capture until this window shows it, audio until the samples are heard, and how much data the session is moving. Save latency log writes those samples under the temp folder and deletes files older than 24 hours.".into(),
            ))
            .child(muted_line(
                "Paste clipboard, and Ctrl+V in the picture, send this computer's clipboard text into the open view. Send clipboard as keystrokes types that text. The switch is saved.".into(),
            ))
            .child(slots_head)
            .child(slots)
            .child(muted_line(
                "Auto gives the next free slot in plug-in order. A pinned controller always arrives as that player on the host.".into(),
            ))
            .when_some(remote_pads, |col, line| col.child(muted_line(line)));
        let input = self.section(Section::Input, Some(input_backends), input_body, cx);

        // Network
        let network_backends = self.backend_row(Part::Network, cx);
        let capturing = self.capturing;
        let network_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(muted_line(if cfg!(windows) {
                "Automatic uses Membrane when its driver is installed and Npcap otherwise. A change applies to the next session.".into()
            } else if cfg!(target_os = "macos") {
                "Raw Ethernet uses BPF. The host service runs as root so BPF is available, and so players can connect when you are signed out. Membrane and Npcap are Windows drivers.".into()
            } else {
                "Raw Ethernet goes through libpcap here. Membrane and Npcap are Windows drivers.".into()
            }))
            .when(cfg!(target_os = "macos"), |col| {
                let installed = crate::macos_service::service_installed();
                col.child(muted_line(if installed {
                    "Host service: installed (LaunchDaemon dev.etherquic.host).".into()
                } else {
                    "Host service: not installed.".into()
                }))
                .child(action_button(
                    if installed {
                        "host-svc-remove"
                    } else {
                        "host-svc-settings-install"
                    },
                    if installed {
                        "Remove host service"
                    } else {
                        "Install host service"
                    },
                    true,
                    if installed { DANGER } else { HOST_GREEN },
                    cx.listener(move |desk, _, _, cx| {
                        if installed {
                            desk.remove_host_service(cx);
                        } else {
                            desk.install_host_service(cx);
                        }
                    }),
                ))
            })
            .child(toggle_row(
                "capture-traffic",
                "Capture traffic",
                capturing,
                cx.listener(|desk, _, _, cx| {
                    desk.toggle_capture();
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Writes ether.pcap and app.pcap for Wireshark. This stays off until you turn it on, and it is not saved.".into(),
            ));
        let network = self.section(Section::Network, Some(network_backends), network_body, cx);

        // Language
        let language_body = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("lang-en")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(PLAYER_BLUE))
                    .bg(rgb(0x243044))
                    .cursor_pointer()
                    .child(flag_en())
                    .child("English")
                    .when(self.defaults.language == "en", |chip| {
                        chip.child(faint_default())
                    })
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.edit_settings(|settings| settings.language = "en".into());
                        cx.notify();
                    })),
            )
            .child(muted_line("More languages are not translated yet.".into()));
        let language = self.section(Section::Language, None, language_body, cx);

        let beta = self.settings.beta_releases;
        let checking = matches!(
            self.update,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. }
        );
        let installable = matches!(self.update, UpdateStatus::Available(_));
        let update_line = match &self.update {
            UpdateStatus::Idle => format!("This build is {}.", etherquic::RELEASE),
            UpdateStatus::Checking => "Checking GitHub tagged releases…".into(),
            UpdateStatus::Current { latest } => {
                format!(
                    "This build is {}. {} is the latest tag.",
                    etherquic::RELEASE,
                    latest
                )
            }
            UpdateStatus::Available(offer) => {
                let kind = if offer.prerelease { "beta" } else { "release" };
                format!(
                    "{} {} is available ({}). This build is {}.",
                    offer.tag,
                    kind,
                    offer.asset_name,
                    etherquic::RELEASE
                )
            }
            UpdateStatus::NoPackage { tag, .. } => format!(
                "{} is newer than {}, and GitHub has no installer for this computer.",
                tag,
                etherquic::RELEASE
            ),
            UpdateStatus::Downloading { tag } => format!("Downloading {tag}…"),
            UpdateStatus::Installed { detail, .. } => detail.clone(),
            UpdateStatus::Failed(err) => err.clone(),
        };
        let updates_body = div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(self.copyable_line(
                "update-line",
                update_line,
                if matches!(self.update, UpdateStatus::Failed(_)) {
                    DANGER
                } else {
                    MUTED
                },
                cx,
            ))
            .child(toggle_chip(
                "beta-releases",
                "Beta releases",
                beta,
                Some(self.defaults.beta_releases),
                cx.listener(|desk, _, _, cx| {
                    desk.edit_settings(|settings| settings.beta_releases = !settings.beta_releases);
                    cx.notify();
                }),
            ))
            .child(muted_line(
                "Off uses the latest GitHub release. On also considers pre-release tags.".into(),
            ))
            .child(action_button(
                "check-updates",
                "Check for updates",
                !checking,
                PLAYER_BLUE,
                cx.listener(|desk, _, _, cx| {
                    desk.check_updates(cx);
                    cx.notify();
                }),
            ))
            .child(action_button(
                "install-update",
                "Download and install",
                installable && !checking,
                HOST_GREEN,
                cx.listener(|desk, _, _, cx| {
                    desk.install_update(cx);
                    cx.notify();
                }),
            ))
            .child(small_chip(
                SharedString::from("open-releases"),
                "Open on GitHub",
                None,
                true,
                cx.listener(|desk, _, _, cx| {
                    desk.open_update_page();
                    cx.notify();
                }),
            ));
        let updates = self.section(Section::Updates, None, updates_body, cx);

        // This computer's own picks, from its discovery record.
        let computer = self
            .peer
            .parse()
            .map(|mac| self.connection_controls(mac, cx));
        let monitors = remote_caps.as_ref().map(|caps| {
            let mut row = div().flex().flex_row().flex_wrap().gap_1().min_w_0();
            for (index, m) in caps.monitors.iter().enumerate() {
                let label = format!("{}: {}×{}", index + 1, m.width, m.height);
                let label = if m.primary {
                    format!("{label} · primary")
                } else {
                    label
                };
                row = row.child(backend_chip(
                    SharedString::from(format!("remote-mon-{index}")),
                    label,
                    (!m.primary).then_some("not shared"),
                    m.primary,
                    m.primary,
                ));
            }
            row
        });
        let mut details = remote_caps.as_ref().map(caps_details).unwrap_or_default();
        if let Some(device) = self.selected_device() {
            details.push(remote_hardware_line(device.video.as_deref()));
            details.push(remote_backend_line(device.audio.as_deref()));
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
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
                    .child(muted_line(
                        "Changes save at once and apply to an open view where that is safe.".into(),
                    ))
                    .child(reset_all),
            )
            .child(appearance)
            .child(capture_section)
            .child(sound)
            .child(codecs)
            .child(input)
            .child(network)
            .child(language)
            .child(updates)
            .child(
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
                    .child(div().font_weight(FontWeight::BOLD).child(format!("This host: {remote_name}")))
                    .when(computer.is_none(), |col| {
                        col.child(muted_line(
                            "Select a computer to choose its picture, audio, and rate.".into(),
                        ))
                    })
                    .when_some(monitors, |col, row| {
                        col.child(section_label("Remote monitors"))
                            .child(row)
                            .child(muted_line(
                                "The host shares its primary monitor. Picking another is not available yet.".into(),
                            ))
                    })
                    .when(!details.is_empty(), |col| {
                        let mut lines = div().flex().flex_col().w_full().min_w_0();
                        for (index, line) in details.clone().into_iter().enumerate() {
                            lines = lines.child(self.copyable_line(
                                format!("host-detail-{index}"),
                                line,
                                MUTED,
                                cx,
                            ));
                        }
                        col.child(section_label("What it reported")).child(lines)
                    })
                    .when_some(computer, |col, controls| col.child(controls)),
            )
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reap_links();
        self.refresh_host_face();
        self.sync_outbound();
        self.apply_shelf_cmds(window);
        self.publish_shelf();
        if !self.shelves.is_empty() {
            window.request_animation_frame();
        }
        install_tray(window, cx);
        if crate::tray::take_hide() {
            window.minimize_window();
        }
        if crate::tray::take_show() {
            window.activate_window();
        }
        if self.start_in_tray && crate::tray::installed() {
            self.start_in_tray = false;
            crate::tray::hide_main();
            window.minimize_window();
        }
        self.sync_taskbar(window, cx);
        self.sync_icons();
        self.track_hosts();
        self.settle_scrollbars(window);
        let status = self.status_line();
        let status_color = self.status_color();
        let host = self.host_card(cx);
        let connect = self.connect_card(window, cx);
        let picker = self.host_picker(window, cx);
        let settings_open = self.open.settings;
        let settings = settings_open.then(|| self.settings_panel(cx));
        let settings_summary = format!(
            "{} · {} · {}",
            self.settings.scale.label(),
            self.summary_codecs(),
            match self.peer.parse() {
                Some(mac) if self.applied(mac).custom =>
                    format!("{} has its own picks", self.target_label()),
                _ => "your defaults".into(),
            }
        );
        self.follow_settings_scroll(window);
        let details_open = self.open.details;
        let nics = details_open.then(|| self.nic_row(cx));
        let nic_error = self.nic_error.clone();
        let adapter = self.nic.clone().unwrap_or_else(|| "Auto".into());
        let page_bar = self.scrollbar(Bar::Page, cx);

        let content = div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w_0()
            .pt_3()
            .pb_4()
            .pl_4()
            // The page scrollbar sits in this right margin, so it never covers content.
            .pr(px(16. + BAR_W))
            .gap_3()
            .when(self.tabs.len() > 0, |col| {
                col.child(self.connection_bar(cx))
            })
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
            .when_some(nic_error.clone(), |col, err| {
                col.child(self.copyable_line("nic-error", err, DANGER, cx))
            })
            .child(picker)
            .child(self.active_block(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .child({
                        let mark = Rc::clone(&self.settings_y);
                        canvas(
                            move |bounds, _, _| mark.set(f32::from(bounds.origin.y)),
                            |_, _, _, _| {},
                        )
                        .w_full()
                        .h(px(0.))
                    })
                    .child(disclosure(
                        "settings-more",
                        "Settings",
                        settings_summary,
                        settings_open,
                        cx.listener(|desk, _, _, cx| {
                            desk.open.settings = !desk.open.settings;
                            desk.confirm_reset = None;
                            cx.notify();
                        }),
                    ))
                    .when_some(settings, |col, panel| col.child(panel)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .child(disclosure(
                        "details-more",
                        "Details",
                        format!("This PC, adapter ({adapter}), lanes"),
                        details_open,
                        cx.listener(|desk, _, _, cx| {
                            desk.open.details = !desk.open.details;
                            cx.notify();
                        }),
                    ))
                    .when(details_open, |col| {
                        col.child(this_pc_section(&self.share))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .w_full()
                                    .min_w_0()
                                    .child(section_label("Adapter"))
                                    .when_some(nics, |row, nics| row.child(nics)),
                            )
                            .child(lane_row())
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .w_full()
                                    .min_w_0()
                                    .child(section_label("Status"))
                                    .child(muted_line(status.clone())),
                            )
                    }),
            );

        let playing = self.link.role() == Role::Client;
        let extra_keys = (cfg!(target_os = "android") && playing).then(|| self.extra_keys_bar(cx));
        let overlay =
            (cfg!(target_os = "android") && self.overlay_active()).then(|| self.overlay_layer(cx));
        self.sync_touch_pointer();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, window, cx| {
                let key = &ev.keystroke;
                let typing = desk.filter_focus.is_focused(window)
                    || desk.add_name_focus.is_focused(window)
                    || desk.add_addr_focus.is_focused(window)
                    || desk.rename_focus.is_focused(window);
                if !typing
                    && crate::desk_model::quick_hotkey(
                        key.modifiers.control,
                        key.modifiers.alt,
                        key.modifiers.shift,
                        &key.key,
                    )
                {
                    desk.quick = !desk.quick;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if desk.forward_session_key(ev) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|desk, ev: &MouseMoveEvent, _, cx| {
                desk.on_drag_move(ev, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|desk, _: &MouseUpEvent, _, cx| {
                    desk.overlay_stick_up(cx);
                    let quality = desk.quality_drag.take().is_some();
                    let bar = desk.drag.take().is_some();
                    if quality {
                        desk.apply_settings(true);
                    }
                    if quality || bar {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|desk, _: &MouseUpEvent, _, cx| {
                    desk.overlay_stick_up(cx);
                    let quality = desk.quality_drag.take().is_some();
                    let bar = desk.drag.take().is_some();
                    if quality {
                        desk.apply_settings(true);
                    }
                    if quality || bar {
                        cx.notify();
                    }
                }),
            )
            // Pinned: the title, preferences, and a status line of fixed height.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap_1()
                    .w_full()
                    .min_w_0()
                    .pt_4()
                    .px_4()
                    .pb_2()
                    .border_b_1()
                    .border_color(rgb(CARD_EDGE))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .w_full()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_baseline()
                                    .gap_3()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xl()
                                            .font_weight(FontWeight::BOLD)
                                            .child("etherquic"),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(rgb(MUTED))
                                            .child("Share this desktop, or open a remote one."),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .flex_none()
                                    .child(self.quick_label("FPS"))
                                    .child(self.side_pick(Menu::QuickFps, cx))
                                    .child(self.quick_label("Shader"))
                                    .child(self.side_pick(Menu::Look, cx))
                                    .child(self.quick_label("Upscale"))
                                    .child(self.side_pick(Menu::Upscale, cx))
                                    .child(self.quick_label("Frame gen"))
                                    .child(self.side_pick(Menu::FrameGen, cx))
                                    .child(prefs_button(
                                        settings_open,
                                        cx.listener(|desk, _, _, cx| {
                                            // Open Settings and bring it into view; a second
                                            // press closes it. Scroll after layout, not here:
                                            // request_animation_frame is only legal during paint.
                                            if desk.open.settings {
                                                desk.open.settings = false;
                                            } else {
                                                desk.open.settings = true;
                                                desk.scroll_to_settings = 2;
                                            }
                                            cx.notify();
                                        }),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("status")
                            .w_full()
                            .min_w_0()
                            .h(px(STATUS_H))
                            .flex_none()
                            .overflow_hidden()
                            .child(self.copyable_line("status", status, status_color, cx)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        div()
                            .id("desk")
                            .flex()
                            .flex_col()
                            .size_full()
                            .overflow_y_scroll()
                            .overflow_x_hidden()
                            .track_scroll(&self.page_scroll)
                            .child(content),
                    )
                    .child(page_bar)
                    .when_some(overlay, |layer, pad| layer.child(pad)),
            )
            .when_some(extra_keys, |root, bar| root.child(bar))
    }
}

#[derive(Clone, Copy, Default)]
struct Panels {
    host: bool,
    connect: bool,
    /// Latency, addresses, and codecs under each discovered host.
    devices: bool,
    /// The Settings sections and this computer's own picks.
    settings: bool,
    /// Every sound device, not only the first few.
    all_devices: bool,
    details: bool,
    /// Link, carrier, and codec comparison under the Carrier heading.
    latency: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Video,
    Audio,
    Fps,
    AudioBitrate,
    AudioChannels,
    QuickFps,
    Look,
    Upscale,
    FrameGen,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostTab {
    Saved,
    Recent,
    Discovered,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bar {
    Page,
    List,
}

#[derive(Clone, Copy)]
struct Drag {
    bar: Bar,
    start_y: f32,
    start: f32,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn viewport(handle: &ScrollHandle) -> f32 {
    f32::from(handle.bounds().size.height)
}

fn max_scroll(handle: &ScrollHandle) -> f32 {
    f32::from(handle.max_offset().height)
}

fn scrolled(handle: &ScrollHandle) -> f32 {
    -f32::from(handle.offset().y)
}

fn presence_color(presence: Presence) -> u32 {
    match presence {
        Presence::Online => HOST_GREEN,
        Presence::Seen(_) | Presence::Unseen => MUTED,
    }
}

enum Edit {
    Changed,
    Submit,
    Cancel,
    Ignored,
}

const FIELD_MAX: usize = 128;

/// One-line editing: type, Backspace, Ctrl+Backspace clears, Ctrl+V pastes,
/// Enter submits, Escape cancels.
fn edit_text(text: &mut String, ev: &KeyDownEvent, cx: &App) -> Edit {
    let key = ev.keystroke.key.as_str();
    let mods = &ev.keystroke.modifiers;
    match key {
        "enter" => return Edit::Submit,
        "escape" => return Edit::Cancel,
        "backspace" => {
            if mods.control || mods.alt || mods.platform {
                text.clear();
            } else {
                text.pop();
            }
            return Edit::Changed;
        }
        _ => {}
    }
    if (mods.control || mods.platform) && key == "v" {
        if let Some(pasted) = cx.read_from_clipboard().and_then(|item| item.text()) {
            push_clean(text, &pasted);
            return Edit::Changed;
        }
        return Edit::Ignored;
    }
    if mods.control || mods.alt || mods.platform {
        return Edit::Ignored;
    }
    let typed = match ev.keystroke.key_char.as_deref() {
        Some(chars) => chars.to_string(),
        None if key == "space" => " ".into(),
        None if key.chars().count() == 1 => key.to_string(),
        None => return Edit::Ignored,
    };
    push_clean(text, &typed);
    Edit::Changed
}

fn push_clean(text: &mut String, typed: &str) {
    for ch in typed.chars() {
        if text.chars().count() >= FIELD_MAX {
            break;
        }
        if !ch.is_control() {
            text.push(ch);
        }
    }
}

fn text_field(
    id: &'static str,
    focus: &FocusHandle,
    focused: bool,
    text: &str,
    placeholder: &'static str,
) -> Stateful<Div> {
    let empty = text.is_empty();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .min_w_0()
        .h(px(30.))
        .px_2()
        .overflow_hidden()
        .bg(rgb(BG))
        .border_1()
        .border_color(rgb(if focused { PLAYER_BLUE } else { CARD_EDGE }))
        .rounded_sm()
        .cursor_text()
        .track_focus(focus)
        .when(!empty, |field| {
            field.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(TEXT))
                    .child(text.to_string()),
            )
        })
        .when(focused, |field| {
            field.child(div().flex_none().w(px(1.)).h(px(16.)).bg(rgb(TEXT)))
        })
        .when(empty, |field| {
            field.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(MUTED))
                    .child(placeholder),
            )
        })
}

/// A section header that opens and closes what follows it.
fn disclosure(
    id: &'static str,
    title: &'static str,
    summary: String,
    open: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .h(px(28.))
        .cursor_pointer()
        .child(chevron(open))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(TEXT))
                .child(title),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(rgb(MUTED))
                .child(summary),
        )
        .on_click(on_click)
}

/// A small drawn triangle, so no font needs the glyph.
/// Muted name beside a Connect menu. Same size as the Video and Audio names.
fn row_label(text: &'static str) -> impl IntoElement {
    div().flex_none().text_color(rgb(MUTED)).child(text)
}

fn chevron(open: bool) -> impl IntoElement {
    // Bars of 2 px stepping down by 2 px on each side make a clean triangle.
    let bars: [f32; 5] = [10., 8., 6., 4., 2.];
    let shape = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(12.))
        .h(px(12.));
    if open {
        let mut col = div().flex().flex_col().items_center();
        for width in bars {
            col = col.child(div().w(px(width)).h(px(1.)).bg(rgb(MUTED)));
        }
        shape.child(col)
    } else {
        let mut row = div().flex().flex_row().items_center();
        for height in bars {
            row = row.child(div().w(px(1.)).h(px(height)).bg(rgb(MUTED)));
        }
        shape.child(row)
    }
}

fn extra_session_chip(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    extra_key_chip_label(id, label, on, on_click)
}

fn extra_key_chip_label(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_1()
        .h(px(36.))
        .min_w(px(36.))
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { 0x12141a }))
        .text_color(rgb(if on { PLAYER_BLUE } else { TEXT }))
        .font_weight(if on {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        })
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

fn overlay_column(
    side: Side,
    items: &[OverlayItem],
    edit: bool,
    stick: Option<overlay::StickTouch>,
    cx: &Context<Desk>,
) -> impl IntoElement {
    let mut col = div().flex().flex_col().gap_2().items_center();
    for (index, item) in items.iter().copied().enumerate() {
        col = col.child(overlay_widget(side, index, item, edit, stick, cx));
    }
    col
}

fn overlay_widget(
    side: Side,
    index: usize,
    item: OverlayItem,
    edit: bool,
    stick: Option<overlay::StickTouch>,
    cx: &Context<Desk>,
) -> AnyElement {
    let body = if item.is_stick() {
        let (nx, ny) = match stick {
            Some(touch) if touch.side == side && touch.index == index => (touch.nx, touch.ny),
            _ => (0.0, 0.0),
        };
        overlay_stick(side, index, nx, ny, cx).into_any_element()
    } else if item.is_cluster() {
        overlay_cluster(item, cx).into_any_element()
    } else {
        overlay_button(item, cx).into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .child(body)
        .when(edit, |wrap| {
            wrap.child(
                div()
                    .id(SharedString::from(format!("ov-x-{}-{index}", side.token())))
                    .px_2()
                    .text_color(rgb(DANGER))
                    .cursor_pointer()
                    .child("×")
                    .on_click(cx.listener(move |desk, _, _, cx| {
                        desk.overlay_remove(side, index);
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
}

fn overlay_stick(
    side: Side,
    index: usize,
    nx: f32,
    ny: f32,
    cx: &Context<Desk>,
) -> impl IntoElement {
    let thumb_x = 56.0 + nx * 36.0 - 18.0;
    let thumb_y = 56.0 - ny * 36.0 - 18.0;
    div()
        .id(SharedString::from(format!(
            "ov-stick-{}-{index}",
            side.token()
        )))
        .relative()
        .w(px(112.))
        .h(px(112.))
        .rounded_full()
        .bg(rgb(0x1a1e27))
        .border_2()
        .border_color(rgb(CARD_EDGE))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, ev: &MouseDownEvent, _, cx| {
                desk.overlay_stick_down(side, index, ev);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(
            div()
                .absolute()
                .left(px(thumb_x))
                .top(px(thumb_y))
                .w(px(36.))
                .h(px(36.))
                .rounded_full()
                .bg(rgb(PLAYER_BLUE)),
        )
}

fn overlay_cluster(item: OverlayItem, cx: &Context<Desk>) -> impl IntoElement {
    let cell = |dir: Dir, label: &'static str| {
        overlay_hold_cell(
            SharedString::from(format!("ov-{}-{}", item.token(), label)),
            label,
            cx.listener(move |desk, ev: &MouseDownEvent, _, cx| {
                let _ = ev;
                desk.overlay_dir(item, dir, true, cx);
                cx.stop_propagation();
            }),
            cx.listener(move |desk, _: &MouseUpEvent, _, cx| {
                desk.overlay_dir(item, dir, false, cx);
                cx.stop_propagation();
            }),
        )
    };
    let (up, left, down, right) = match item {
        OverlayItem::Wasd => ("W", "A", "S", "D"),
        OverlayItem::Arrows => ("↑", "←", "↓", "→"),
        _ => ("▲", "◀", "▼", "▶"),
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(overlay_spacer())
                .child(cell(Dir::Up, up))
                .child(overlay_spacer()),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(cell(Dir::Left, left))
                .child(cell(Dir::Down, down))
                .child(cell(Dir::Right, right)),
        )
}

fn overlay_button(item: OverlayItem, cx: &Context<Desk>) -> AnyElement {
    let label = item.label();
    overlay_hold_cell(
        SharedString::from(format!("ov-btn-{}", item.token())),
        label,
        cx.listener(move |desk, _: &MouseDownEvent, _, cx| {
            desk.overlay_hold(item, true, cx);
            cx.stop_propagation();
        }),
        cx.listener(move |desk, _: &MouseUpEvent, _, cx| {
            desk.overlay_hold(item, false, cx);
            cx.stop_propagation();
        }),
    )
    .into_any_element()
}

fn overlay_spacer() -> impl IntoElement {
    div().w(px(44.)).h(px(44.))
}

fn overlay_hold_cell(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    on_up: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_up = Rc::new(on_up);
    let up = on_up.clone();
    div()
        .id(id)
        .flex()
        .w(px(44.))
        .h(px(44.))
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(rgb(CARD_EDGE))
        .bg(rgb(0x1a1e27))
        .text_color(rgb(TEXT))
        .cursor_pointer()
        .child(label.into())
        .on_mouse_down(MouseButton::Left, on_down)
        .on_mouse_up(MouseButton::Left, move |ev, window, cx| up(ev, window, cx))
        .on_mouse_up_out(MouseButton::Left, move |ev, window, cx| {
            on_up(ev, window, cx)
        })
}

fn extra_key_chip(
    key: ExtraKey,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = if key == ExtraKey::Keyboard {
        if on {
            "HIDE KB"
        } else {
            "KEYBOARD"
        }
    } else {
        key.label()
    };
    div()
        .id(key.id())
        .flex()
        .flex_1()
        .h(px(36.))
        .min_w(px(36.))
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { 0x12141a }))
        .text_color(rgb(if on { PLAYER_BLUE } else { TEXT }))
        .font_weight(if on {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        })
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

/// One full-width listen row. The name, addresses, and kind share a left edge.
fn listen_row(
    id: impl Into<gpui::ElementId>,
    label: SharedString,
    accent: Option<u32>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let filled = accent.is_some();
    div()
        .id(id)
        .flex()
        .w_full()
        .min_w_0()
        .h(px(26.))
        .px_2()
        .items_center()
        .justify_start()
        .rounded_sm()
        .border_1()
        .border_color(rgb(accent.unwrap_or(CARD_EDGE)))
        .bg(rgb(accent.unwrap_or(CARD)))
        .text_color(rgb(if filled { INK } else { TEXT }))
        .overflow_hidden()
        .whitespace_nowrap()
        .when(filled, |row| row.font_weight(FontWeight::BOLD))
        .when(enabled, |row| row.cursor_pointer())
        .when(!enabled, |row| row.opacity(0.4).cursor_default())
        .child(label)
        .when(enabled, |row| row.on_click(on_click))
}

fn small_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    accent: Option<u32>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let filled = accent.is_some();
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(26.))
        .px_2()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(accent.unwrap_or(CARD_EDGE)))
        .bg(rgb(accent.unwrap_or(CARD)))
        .text_color(rgb(if filled { INK } else { TEXT }))
        .when(filled, |chip| chip.font_weight(FontWeight::BOLD))
        .when(enabled, |chip| chip.cursor_pointer())
        .when(!enabled, |chip| chip.opacity(0.4).cursor_default())
        .child(label.into())
        .when(enabled, |chip| chip.on_click(on_click))
}

/// Save, or a disabled "Saved" once the host is in the Saved list.
fn save_chip(
    id: impl Into<gpui::ElementId>,
    saved: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    small_chip(
        id,
        if saved { "Saved" } else { "Save" },
        None,
        !saved,
        on_click,
    )
}

fn tab_chip(
    id: &'static str,
    label: String,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(28.))
        .px_3()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { CARD }))
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .when(on, |chip| chip.font_weight(FontWeight::BOLD))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

/// One host: name, address, and what it has (monitors, inputs) on the left,
/// status and actions on the right. Every row is the same height, so the list
/// does not jump.
#[allow(clippy::too_many_arguments)]
fn host_row(
    id: SharedString,
    lead: Option<AnyElement>,
    title: impl IntoElement,
    address: String,
    summary: String,
    status: String,
    status_color: u32,
    selected: bool,
    actions: impl IntoElement,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // The text is the only child in the flow and spans the row, with room kept
    // at both ends; the picture, status, and buttons sit over that room, so no
    // row can move another. Each line wraps inside a box one line high and is
    // clipped there. A no-wrap line is measured once by this GPUI, at whatever
    // width the first pass offered, and could keep that size.
    const PAD: f32 = 12.;
    const GAP: f32 = 12.;
    const STATUS_W: f32 = 132.;
    let left = if lead.is_some() { LEAD_W + GAP } else { 0. };
    let right = STATUS_W + GAP + ACTIONS_W + GAP;
    let line = |text: String, small: bool, color: u32| {
        div()
            .w_full()
            .min_w_0()
            .h(px(if small { 16. } else { 18. }))
            .line_height(px(if small { 16. } else { 18. }))
            .overflow_hidden()
            .whitespace_normal()
            .when(small, |line| line.text_xs())
            .text_color(rgb(color))
            .child(text)
    };
    div()
        .id(id)
        .relative()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .w_full()
        .min_w_0()
        .h(px(HOST_ROW_H))
        .rounded_md()
        .border_1()
        .border_color(rgb(if selected { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if selected { 0x1f2736 } else { CARD }))
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .pl(px(PAD + left))
                .pr(px(PAD + right))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .h(px(20.))
                        .overflow_hidden()
                        .child(title),
                )
                .child(line(address, true, MUTED))
                .child(line(summary, true, TEXT)),
        )
        .when_some(lead, |row, lead| {
            row.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(PAD))
                    .flex()
                    .items_center()
                    .child(lead),
            )
        })
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(PAD))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(GAP))
                .child(
                    div()
                        .w(px(STATUS_W))
                        .h(px(32.))
                        .line_height(px(16.))
                        .overflow_hidden()
                        .whitespace_normal()
                        .text_xs()
                        .text_color(rgb(status_color))
                        .child(status),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .w(px(ACTIONS_W))
                        .child(actions),
                ),
        )
        .on_click(on_click)
}

/// Monitors and inputs in a few words. An older host sends no details.
fn row_summary(
    caps: Option<&etherquic::discover::Capabilities>,
    release: Option<&etherquic::discover::Release>,
) -> String {
    let hardware = match caps {
        Some(caps) => {
            let summary = caps_summary(caps);
            if caps.monitors.len() > 1 {
                let sizes: Vec<String> = caps
                    .monitors
                    .iter()
                    .map(|m| format!("{}×{}", m.width, m.height))
                    .collect();
                if summary.is_empty() {
                    sizes.join(" + ")
                } else {
                    format!("{summary} · {}", sizes.join(" + "))
                }
            } else if !summary.is_empty() {
                summary
            } else if caps.monitors.is_empty() {
                "No monitors reported".into()
            } else {
                String::new()
            }
        }
        None => String::new(),
    };
    let mut parts = Vec::new();
    if let Some(rel) = release {
        if !rel.version.trim().is_empty() {
            parts.push(format!("version {}", rel.version.trim()));
        }
        let os = rel.os_label();
        if !os.is_empty() {
            parts.push(os);
        }
    } else if let Some(caps) = caps {
        let label = caps.os_label.trim();
        if !label.is_empty() {
            parts.push(label.to_string());
        }
    }
    if !hardware.is_empty() {
        parts.push(hardware);
    }
    if parts.is_empty() {
        "No details from this host (older version)".into()
    } else {
        parts.join(" · ")
    }
}

/// A host's name in a row: bold, one line, clipped.
fn row_title(name: String) -> Div {
    div()
        .w_full()
        .min_w_0()
        .h(px(20.))
        .line_height(px(20.))
        .overflow_hidden()
        .whitespace_normal()
        .font_weight(FontWeight::BOLD)
        .child(name)
}

/// The fixed box a discovery thumbnail sits in, so rows line up whether or not
/// a host sent one.
fn thumb_slot(picture: Option<Arc<RenderImage>>) -> AnyElement {
    let slot = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(THUMB_W))
        .h(px(THUMB_H))
        .rounded_sm()
        .overflow_hidden()
        .bg(rgb(BG))
        .border_1()
        .border_color(rgb(CARD_EDGE));
    match picture {
        Some(picture) => slot
            .child(
                img(ImageSource::Render(picture))
                    .w(px(THUMB_W - 2.))
                    .h(px(THUMB_H - 2.))
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element(),
        None => slot.into_any_element(),
    }
}

fn preview_render_image(rgb: &[u8], width: u32, height: u32) -> Option<Arc<RenderImage>> {
    let mut bgra = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// RGBA from the decoder into the BGRA frame the renderer takes.
fn render_image(thumb: crate::Thumb) -> Option<Arc<RenderImage>> {
    let mut bgra = thumb.rgba;
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(thumb.width, thumb.height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
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

fn phone_share(qr: &Option<QrGrid>, ours: &etherquic::discover::Release) -> impl IntoElement {
    let mut code = div().flex().flex_col().items_center().gap_2().flex_none();
    if let Some(grid) = qr {
        code = code.child(
            div()
                .flex_none()
                .p_2()
                .bg(rgb(0xffffff))
                .rounded_sm()
                .child(qr_picture(grid)),
        );
    }
    code = code.child(
        div()
            .w(px(240.))
            .min_w_0()
            .text_center()
            .text_color(rgb(MUTED))
            .whitespace_normal()
            .child("Scan to join this PC."),
    );
    let face = etherquic::discover::OsFace::from_os(ours.os, &ours.os_name, &ours.os_version);
    let os = ours.os_label();
    let version = format!("version {}", ours.version.trim());
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_4()
        .w_full()
        .min_w_0()
        .child(code)
        .child(
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .min_w_0()
                .child(os_badge(face, &ours.os_version))
                .child(
                    div()
                        .text_color(rgb(TEXT))
                        .whitespace_normal()
                        .child(if os.is_empty() {
                            "This PC".to_string()
                        } else {
                            os
                        }),
                )
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .whitespace_normal()
                        .child(version),
                ),
        )
}

fn host_lead(
    picture: Option<Arc<RenderImage>>,
    caps: Option<&etherquic::discover::Capabilities>,
    release: Option<&etherquic::discover::Release>,
) -> AnyElement {
    let (face, subscript) = if let Some(rel) = release {
        (
            etherquic::discover::OsFace::from_os(rel.os, &rel.os_name, &rel.os_version),
            rel.os_version.clone(),
        )
    } else if let Some(caps) = caps {
        etherquic::discover::face_from_label(caps.os, &caps.os_label)
    } else {
        (etherquic::discover::OsFace::Other, String::new())
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .flex_none()
        .w(px(LEAD_W))
        .h(px(THUMB_H.max(BADGE_W)))
        .child(os_badge(face, &subscript))
        .child(thumb_slot(picture))
        .into_any_element()
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
    enabled: bool,
    why: Option<&'static str>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .max_w(px(280.))
        .h(px(28.))
        .px_2()
        .items_center()
        .overflow_hidden()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { CARD }))
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .when(enabled, |chip| chip.cursor_pointer())
        .when(!enabled, |chip| chip.opacity(0.45).cursor_default())
        .when_some(why, |chip, why| chip.tooltip(why_tip(why)))
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label.into()),
        )
        .on_click(on_click)
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

/// Addresses, codecs, and latency of one discovered host, under its row.
fn device_details(device: &Found, selection: &str, caps: &[String]) -> impl IntoElement {
    let mut col = div().flex().flex_col().gap_1().w_full().min_w_0();
    if let Some(rel) = &device.release {
        let line = rel.list_line();
        if !line.is_empty() {
            col = col.child(muted_line(line));
        }
    }
    for line in caps {
        col = col.child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .child(line.clone()),
        );
    }
    if device.key != device.l2_mac {
        col = col.child(muted_line(format!("Adapter {}", format_mac(device.key))));
    }
    col.child(muted_line(l2_line(device)))
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
}

fn codec_cell(width: f32, color: u32, text: impl Into<String>) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(width))
        .px_1()
        .text_xs()
        .text_color(rgb(color))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(text.into())
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

fn listen_set() -> crate::desk_model::ListenSet {
    let faces = etherquic::list_faces().unwrap_or_default();
    crate::desk_model::ListenSet::from_parts(faces.into_iter().map(|face| {
        let friendly = if face.description.trim().is_empty() {
            face.name.clone()
        } else {
            face.description
        };
        let mac = face.mac.map(format_mac).unwrap_or_default();
        let ipv4 = face.ipv4.iter().map(|ip| ip.to_string()).collect();
        let ipv6 = face.ipv6.iter().map(|ip| ip.to_string()).collect();
        (face.name, friendly, mac, ipv4, ipv6, face.kind)
    }))
}

struct Shelf {
    group: u64,
}

impl Render for Shelf {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (tabs, _quick) = crate::desk_model::published_tabs();
        let group = self.group;
        let mine: Vec<_> = tabs
            .into_iter()
            .filter(|tab| tab.group == group)
            .collect();
        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .p_3()
            .gap_2()
            .child(div().font_weight(FontWeight::BOLD).child(format!(
                "Window {group}"
            )))
            .child(muted_line(
                "Tabs in this window. Double-click focuses the picture, including a minimized window or another desktop.".into(),
            ));
        if mine.is_empty() {
            col = col.child(muted_line("No connections in this window.".into()));
        }
        for tab in mine {
            let id = tab.id;
            col = col.child(
                div()
                    .id(SharedString::from(format!("shelf-{group}-{id}")))
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(if tab.current { 0x243044 } else { CARD }))
                    .child(div().flex_1().child(format!("{}  {}", tab.label, tab.badge)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, ev: &MouseDownEvent, _, _| {
                            if ev.click_count >= 2 {
                                crate::desk_model::push_cmd(crate::desk_model::ShareCmd::Focus(id));
                            } else {
                                crate::desk_model::push_cmd(crate::desk_model::ShareCmd::Select(id));
                            }
                        }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("shelf-x-{group}-{id}")))
                            .px_1()
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, _, _, _| {
                                    crate::desk_model::push_cmd(crate::desk_model::ShareCmd::Close(id));
                                }),
                            ),
                    ),
            );
        }
        col.child(small_chip(
            SharedString::from(format!("shelf-quick-{group}")),
            "Quick settings",
            Some(PLAYER_BLUE),
            true,
            cx.listener(|_, _, _, _| {
                crate::desk_model::push_cmd(crate::desk_model::ShareCmd::ToggleQuick);
            }),
        ))
    }
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

/// A backend or device choice. Greyed ones say why and do nothing.
fn backend_chip(
    id: SharedString,
    label: impl Into<SharedString>,
    note: Option<&'static str>,
    on: bool,
    enabled: bool,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let text = match note {
        Some(note) => format!("{label} · {note}"),
        None => label.to_string(),
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(24.))
        .px_2()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { BG }))
        .text_xs()
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .when(!enabled, |chip| {
            chip.opacity(0.45)
                .when_some(note, |chip, why| chip.tooltip(why_tip(why)))
        })
        .child(text)
}

/// Hover text for a greyed control. Press writes the same sentence to status.
struct Tip {
    text: SharedString,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .max_w(px(420.))
            .rounded_sm()
            .border_1()
            .border_color(rgb(CARD_EDGE))
            .bg(rgb(CARD))
            .text_xs()
            .text_color(rgb(TEXT))
            .whitespace_normal()
            .child(self.text.clone())
    }
}

fn warn_triangle(
    id: SharedString,
    level: crate::path_budget::WarnLevel,
    tip: String,
) -> impl IntoElement {
    let color = match level {
        crate::path_budget::WarnLevel::Red => DANGER,
        crate::path_budget::WarnLevel::Yellow | crate::path_budget::WarnLevel::Ok => 0xe6c14a,
    };
    div()
        .id(id)
        .flex_none()
        .text_xs()
        .line_height(px(14.))
        .text_color(rgb(color))
        .cursor_default()
        .tooltip(shared_tip(tip))
        .child("▲")
}

fn shared_tip(why: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let why = why.into();
    move |_, cx| cx.new(|_| Tip { text: why.clone() }).into()
}

fn why_tip(why: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    shared_tip(why)
}

/// One sound device in a column. The name is clipped to the column.
fn device_chip(
    id: SharedString,
    label: String,
    on: bool,
    is_default: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .w_full()
        .min_w_0()
        .h(px(26.))
        .px_2()
        .items_center()
        .overflow_hidden()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { BG }))
        .text_xs()
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .when(!enabled, |chip| chip.opacity(0.45))
        .when(enabled, |chip| chip.cursor_pointer().on_click(on_click))
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h(px(16.))
                .line_height(px(16.))
                .overflow_hidden()
                .whitespace_normal()
                .child(label),
        )
        .when(is_default, |chip| chip.child(faint_default()))
}

/// The faint "default" mark on the option the user's defaults pick.
fn faint_default() -> impl IntoElement {
    div()
        .flex_none()
        .text_xs()
        .text_color(rgb(MUTED))
        .opacity(0.6)
        .child("default")
}

/// A choice with the faint "default" mark beside it when it is the default.
fn with_default(chip: impl IntoElement, is_default: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_1()
        .child(chip)
        .when(is_default, |row| row.child(faint_default()))
}

/// A remote device: shown, not picked here. The first is that computer's default.
fn info_chip(label: String, first: bool) -> impl IntoElement {
    div()
        .flex()
        .w_full()
        .min_w_0()
        .h(px(26.))
        .px_2()
        .items_center()
        .overflow_hidden()
        .rounded_sm()
        .border_1()
        .border_color(rgb(CARD_EDGE))
        .text_xs()
        .text_color(rgb(MUTED))
        .opacity(0.7)
        .child(
            div()
                .w_full()
                .h(px(16.))
                .line_height(px(16.))
                .overflow_hidden()
                .whitespace_normal()
                .child(if first {
                    format!("{label} (default)")
                } else {
                    label
                }),
        )
}

fn two_columns(left: impl IntoElement, right: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .w_full()
        .min_w_0()
        .child(div().flex_1().min_w_0().child(left))
        .child(div().flex_1().min_w_0().child(right))
}

fn column(title: &'static str, body: impl IntoElement) -> impl IntoElement {
    column_owned(title.to_string(), body)
}

fn column_owned(title: String, body: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .child(
            div()
                .w_full()
                .min_w_0()
                .h(px(16.))
                .line_height(px(16.))
                .overflow_hidden()
                .whitespace_normal()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(title),
        )
        .child(body)
}

/// A full-width on/off row.
fn pick_row(
    id: SharedString,
    label: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_1()
        .min_w_0()
        .h(px(32.))
        .px_2()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(CARD_EDGE))
        .bg(rgb(CARD))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

fn toggle_row(
    id: &'static str,
    label: &'static str,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .w_full()
        .min_w_0()
        .h(px(32.))
        .px_2()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { CARD }))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

/// A compact on/off chip with a check box.
fn toggle_chip(
    id: &'static str,
    label: &'static str,
    on: bool,
    default_on: Option<bool>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(28.))
        .px_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { CARD }))
        .cursor_pointer()
        .child(
            div()
                .w(px(12.))
                .h(px(12.))
                .rounded_sm()
                .border_1()
                .border_color(rgb(if on { PLAYER_BLUE } else { MUTED }))
                .when(on, |box_| box_.bg(rgb(PLAYER_BLUE))),
        )
        .child(label)
        .when_some(default_on, |chip, default_on| {
            chip.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .opacity(0.6)
                    .child(if default_on {
                        "default on"
                    } else {
                        "default off"
                    }),
            )
        })
        .on_click(on_click)
}

/// A small drawn English flag, so no emoji font is needed.
fn flag_en() -> impl IntoElement {
    let mut stripes = div().flex().flex_col().w(px(24.)).h(px(14.));
    for index in 0..7 {
        stripes = stripes.child(div().w_full().h(px(2.)).bg(rgb(if index % 2 == 0 {
            0xb22234
        } else {
            0xffffff
        })));
    }
    div()
        .relative()
        .flex_none()
        .w(px(24.))
        .h(px(14.))
        .overflow_hidden()
        .rounded_sm()
        .child(stripes)
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .w(px(10.))
                .h(px(8.))
                .bg(rgb(0x3c3b6e)),
        )
}

#[cfg(test)]
mod tests {
    use super::{Command, Launch};

    #[test]
    fn the_window_opens_with_an_empty_connect_field() {
        let launch = Launch::parse(std::iter::empty()).unwrap();
        assert!(launch.command == Command::Window);
        assert!(launch.peer.digits().is_empty());
        assert!(launch.peer.parse().is_none());
    }

    #[test]
    fn hosting_leaves_the_connect_field_empty() {
        let launch = Launch::parse(["host".into()].into_iter()).unwrap();
        assert!(launch.peer.digits().is_empty());
    }

    #[test]
    fn a_given_mac_fills_the_connect_field() {
        let launch = Launch::parse(
            [
                "connect".into(),
                "--peer-mac".into(),
                "ab:cd:ef:01:23:45".into(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(launch.peer.display(), "ab:cd:ef:01:23:45");
        assert!(launch.peer.parse().is_some());
    }
}
