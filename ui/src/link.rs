//! One L2 play session, either hosting or connecting.
//!
//! The NIC is opened on the caller thread so a missing adapter or Npcap
//! shows up before a worker starts. The Quinn endpoint is created inside
//! that worker's Tokio runtime.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
#[cfg(not(feature = "ui"))]
use std::time::Duration;

use std::net::SocketAddr;

#[cfg(not(feature = "ui"))]
use etherquic::{l2_client_endpoint, l2_server_endpoint, peer_to_socket_addr};
use etherquic::{L2Net, ETHERTYPE_QUIC};

use crate::session::{taskbar_for, Role, CLIENT_MAC, HOST_MAC, PLAY_SERVICE_ID};

const MTU: usize = 1500;

/// A NIC from `pcap` enumeration. `name` is what open uses; `description` is the label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NicChoice {
    pub name: String,
    pub description: String,
}

pub fn list_nics() -> Result<Vec<NicChoice>, String> {
    let nics = etherquic::pcap_link::list_nics().map_err(|e| e.to_string())?;
    Ok(nics
        .into_iter()
        .map(|nic| NicChoice {
            name: nic.name,
            description: nic.description,
        })
        .collect())
}

/// Host or player link. Drop stops the worker and joins it.
pub struct Link {
    role: Role,
    stop_flag: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    peers: Arc<AtomicUsize>,
    status: Arc<Mutex<String>>,
    thread: Option<JoinHandle<()>>,
    picture: Arc<AtomicU64>,
}

impl Link {
    pub fn idle() -> Self {
        Self {
            role: Role::Idle,
            stop_flag: Arc::new(AtomicBool::new(false)),
            up: Arc::new(AtomicBool::new(false)),
            peers: Arc::new(AtomicUsize::new(0)),
            status: Arc::new(Mutex::new(
                "Idle. Host this PC, or connect as the player.".into(),
            )),
            thread: None,
            picture: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Picture seat this player session is showing. `0` when it has none.
    pub fn picture_id(&self) -> u64 {
        self.picture.load(Ordering::SeqCst)
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn status(&self) -> String {
        read_status(&self.status)
    }

    pub fn peers(&self) -> usize {
        self.peers.load(Ordering::SeqCst)
    }

    /// True only when this process is the host and the endpoint was created.
    pub fn shows_taskbar(&self) -> bool {
        taskbar_for(self.role, self.up.load(Ordering::SeqCst))
    }

    /// Join a worker that already exited so a later host or connect can open the NIC.
    pub fn reap(&mut self) {
        let finished = self.thread.as_ref().is_some_and(|t| t.is_finished());
        if !finished {
            return;
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        self.up.store(false, Ordering::SeqCst);
        self.role = Role::Idle;
    }

    pub fn host(
        &mut self,
        nic: Option<String>,
        backend: etherquic::MediaBackend,
        fps: u32,
    ) -> Result<(), String> {
        self.reap();
        if self.thread.is_some() {
            return Err("stop the current session first".into());
        }
        let (cert, key) = cert_paths()?;
        let label = nic.clone().unwrap_or_else(|| "default adapter".into());
        let net = L2Net::open(nic.as_deref(), MTU, ETHERTYPE_QUIC).map_err(|e| e.to_string())?;
        self.arm(Role::Host, format!("Sharing this desktop on {label}…"));
        let stop = Arc::clone(&self.stop_flag);
        let up = Arc::clone(&self.up);
        let peers = Arc::clone(&self.peers);
        let status = Arc::clone(&self.status);
        let nic_for_discovery = nic.clone();
        match std::thread::Builder::new()
            .name("etherquic-host".into())
            .spawn(move || {
                host_thread(
                    net,
                    cert,
                    key,
                    nic_for_discovery,
                    backend,
                    fps,
                    stop,
                    up,
                    peers,
                    status,
                )
            }) {
            Ok(handle) => {
                self.thread = Some(handle);
                Ok(())
            }
            Err(e) => {
                self.role = Role::Idle;
                self.write_status(format!("Host failed: {e}"));
                Err(e.to_string())
            }
        }
    }

    pub fn connect(
        &mut self,
        nic: Option<String>,
        peer: [u8; 6],
        trace_input: bool,
        host_l2: u8,
        udp: Option<SocketAddr>,
        codecs: CodecOffer,
    ) -> Result<(), String> {
        self.reap();
        if self.thread.is_some() {
            return Err("stop the current session first".into());
        }
        let (net, opening) = if let Some(addr) = udp {
            (None, format!("Opening the view to {addr}…"))
        } else {
            let label = nic.clone().unwrap_or_else(|| "default adapter".into());
            let net =
                L2Net::open(nic.as_deref(), MTU, ETHERTYPE_QUIC).map_err(|e| e.to_string())?;
            let driver = etherquic::discover::l2_backend_label(host_l2);
            let driver = if driver.is_empty() {
                String::new()
            } else {
                format!(" Host L2: {driver}.")
            };
            (
                Some(net),
                format!(
                    "Opening the view to {} via {label}…{driver}",
                    crate::format_mac(peer)
                ),
            )
        };
        self.arm(Role::Client, opening);
        self.picture = Arc::new(AtomicU64::new(0));
        let stop = Arc::clone(&self.stop_flag);
        let status = Arc::clone(&self.status);
        let picture = Arc::clone(&self.picture);
        match std::thread::Builder::new()
            .name("etherquic-connect".into())
            .spawn(move || {
                client_thread(
                    net,
                    nic,
                    peer,
                    udp,
                    trace_input,
                    codecs,
                    stop,
                    status,
                    picture,
                )
            }) {
            Ok(handle) => {
                self.thread = Some(handle);
                Ok(())
            }
            Err(e) => {
                self.role = Role::Idle;
                self.write_status(format!("Connect failed: {e}"));
                Err(e.to_string())
            }
        }
    }

    /// Ask the worker to leave and return without joining it.
    ///
    /// A tab click uses this. That worker sits in `hold_picture` for its own
    /// seat. The picture window is a different thread. Joining either one
    /// here would freeze the click until the other desktop closed.
    pub fn release(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.up.store(false, Ordering::SeqCst);
        self.peers.store(0, Ordering::SeqCst);
        self.role = Role::Idle;
        self.write_status("Idle. Host this PC, or connect as the player.");
    }

    /// True while the worker thread has not exited.
    pub fn running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        self.up.store(false, Ordering::SeqCst);
        self.peers.store(0, Ordering::SeqCst);
        self.role = Role::Idle;
        self.write_status("Idle. Host this PC, or connect as the player.");
    }

    fn arm(&mut self, role: Role, status: String) {
        self.stop_flag.store(false, Ordering::SeqCst);
        self.up.store(false, Ordering::SeqCst);
        self.peers.store(0, Ordering::SeqCst);
        self.role = role;
        self.write_status(status);
    }

    fn write_status(&self, text: impl Into<String>) {
        write_status(&self.status, text.into());
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop();
    }
}

fn announced_media(
    backend: etherquic::MediaBackend,
) -> (
    Option<Vec<etherquic::discover::AnnouncedCodec>>,
    Option<Vec<etherquic::discover::AnnouncedCodec>>,
) {
    #[cfg(feature = "ui")]
    {
        let (video, audio) = etherquic::offer_media(backend);
        (Some(video), Some(audio))
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = backend;
        (None, None)
    }
}

/// Capabilities and a desktop thumbnail for version-3 offers, refreshed in
/// the background until `stop` is set.
fn discovery_extras(stop: Arc<AtomicBool>) -> Arc<dyn etherquic::discover::OfferExtras> {
    #[cfg(all(feature = "ui", not(target_os = "android")))]
    {
        etherquic_play::hostinfo::host_extras(stop)
    }
    #[cfg(not(all(feature = "ui", not(target_os = "android"))))]
    {
        let _ = stop;
        Arc::new(etherquic::discover::NoExtras)
    }
}

fn host_thread(
    net: Arc<L2Net>,
    cert: PathBuf,
    key: PathBuf,
    nic: Option<String>,
    backend: etherquic::MediaBackend,
    fps: u32,
    stop: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    peers: Arc<AtomicUsize>,
    status: Arc<Mutex<String>>,
) {
    let discovery_stop = Arc::new(AtomicBool::new(false));
    let discovery_flag = Arc::clone(&discovery_stop);
    let l2_backend = net.l2_backend();
    // Probed before this host binds, so it names the port the listeners take
    // (the next free one when another program holds the service number).
    let l3_port = etherquic::udp_port_for(PLAY_SERVICE_ID);
    let discovery = std::thread::Builder::new()
        .name("etherquic-announce".into())
        .spawn(move || {
            let (video, audio) = announced_media(backend);
            let card = etherquic::discover::HostCard {
                name: crate::share::machine_name(),
                l2_mac: HOST_MAC,
                l2_service: PLAY_SERVICE_ID,
                l3_port,
                video,
                audio,
                l2_backend,
            };
            let extras = discovery_extras(Arc::clone(&discovery_flag));
            // UDP answers too, so a player without raw Ethernet still finds this host.
            let udp = {
                let (card, extras, flag, nic) = (
                    card.clone(),
                    Arc::clone(&extras),
                    Arc::clone(&discovery_flag),
                    nic.clone(),
                );
                std::thread::Builder::new()
                    .name("etherquic-announce-udp".into())
                    .spawn(move || {
                        if let Err(err) =
                            etherquic::discover::serve_udp(nic.as_deref(), &card, &*extras, &flag)
                        {
                            eprintln!("discovery over UDP: {err}");
                        }
                    })
                    .ok()
            };
            if let Err(err) =
                etherquic::discover::serve_with(nic.as_deref(), &card, &*extras, &discovery_flag)
            {
                eprintln!("discovery: {err}");
            }
            if let Some(handle) = udp {
                let _ = handle.join();
            }
        })
        .ok();
    #[cfg(all(feature = "ui", target_os = "android"))]
    {
        let _ = (net, cert, key, fps, up, peers, stop);
        write_status(&status, "Android cannot host a desktop.");
        discovery_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = discovery {
            let _ = handle.join();
        }
        return;
    }
    #[cfg(all(feature = "ui", not(target_os = "android")))]
    {
        let slot = Arc::clone(&status);
        let report: etherquic_play::Status = Arc::new(move |text: &str| write_status(&slot, text));
        let session = etherquic_play::HostSession {
            local_mac: HOST_MAC,
            service_id: PLAY_SERVICE_ID,
            max_width: etherquic_play::DEFAULT_MAX_WIDTH,
            fps: fps.max(1),
            cert,
            key,
            audio_device: host_audio_device(),
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            etherquic_play::host_desktop(
                net,
                session,
                Arc::clone(&stop),
                Arc::clone(&up),
                Arc::clone(&peers),
                report,
            )
        }));
        if let Err(err) = session_error(outcome) {
            write_status(&status, format!("Host failed: {err}"));
        }
        discovery_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = discovery {
            let _ = handle.join();
        }
        return;
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = fps;
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                write_status(&status, format!("Host failed: {e}"));
                discovery_stop.store(true, Ordering::SeqCst);
                if let Some(handle) = discovery {
                    let _ = handle.join();
                }
                return;
            }
        };
        runtime.block_on(async move {
        let endpoint = match l2_server_endpoint(net, HOST_MAC, PLAY_SERVICE_ID, &cert, &key) {
            Ok(endpoint) => endpoint,
            Err(e) => {
                write_status(&status, format!("Host failed: {e}"));
                return;
            }
        };
        up.store(true, Ordering::SeqCst);
        write_status(
            &status,
            format!(
                "Waiting for a player. This PC is {} on service {PLAY_SERVICE_ID}.",
                crate::format_mac(HOST_MAC)
            ),
        );
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(200), endpoint.accept()).await {
                Ok(Some(incoming)) => {
                    let accepted = tokio::select! {
                        result = incoming => result,
                        _ = until_stop(&stop) => break,
                    };
                    match accepted {
                        Ok(conn) => {
                            let remote = conn.remote_address();
                            peers.fetch_add(1, Ordering::SeqCst);
                            write_status(&status, format!("Player connected ({remote})."));
                            tokio::select! {
                                _ = conn.closed() => {}
                                _ = until_stop(&stop) => {}
                            }
                            drop(conn);
                            peers.fetch_sub(1, Ordering::SeqCst);
                            if stop.load(Ordering::SeqCst) {
                                break;
                            }
                            write_status(
                                &status,
                                format!(
                                    "Waiting for a player. This PC is {} on service {PLAY_SERVICE_ID}.",
                                    crate::format_mac(HOST_MAC)
                                ),
                            );
                        }
                        Err(e) => write_status(&status, format!("Handshake failed: {e}")),
                    }
                }
                Ok(None) => {
                    write_status(&status, "Host closed.");
                    break;
                }
                Err(_) => {}
            }
        }
        up.store(false, Ordering::SeqCst);
        endpoint.close(0u32.into(), b"stop");
        let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
        });
        discovery_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = discovery {
            let _ = handle.join();
        }
    }
}

/// The codec order and pins a player offers. Without the window there is no
/// media session, so the headless build carries nothing.
#[cfg(feature = "ui")]
pub type CodecOffer = etherquic_play::CodecChoice;
#[cfg(not(feature = "ui"))]
#[derive(Clone, Debug, Default)]
pub struct CodecOffer;

#[cfg(feature = "ui")]
static SESSIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

#[cfg(feature = "ui")]
fn session_key(peer: [u8; 6], udp: Option<SocketAddr>) -> String {
    match udp {
        Some(addr) => addr.ip().to_string(),
        None => crate::format_mac(peer),
    }
}

#[cfg(feature = "ui")]
/// Keep the codecs a "Viewing …" status names, for the Settings summary.
fn remember_session(peer: [u8; 6], udp: Option<SocketAddr>, text: &str) {
    let Some(pair) = crate::prefs::negotiated_from_status(text) else {
        return;
    };
    let key = session_key(peer, udp);
    let mut list = SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
    list.retain(|(k, _)| *k != key);
    list.push((key.clone(), pair));
    // The MAC also finds it when the host was dialled by IP.
    if udp.is_some() && peer != [0; 6] {
        let mac = crate::format_mac(peer);
        let pair = list.last().map(|(_, p)| p.clone()).unwrap_or_default();
        list.retain(|(k, _)| *k != mac);
        list.push((mac, pair));
    }
}

#[cfg(feature = "ui")]
/// The codecs of the last session with this host in this run, if any.
pub fn last_session(peer: [u8; 6], ip: Option<std::net::IpAddr>) -> Option<String> {
    let list = SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
    let keys = [
        ip.map(|ip| ip.to_string()),
        (peer != [0; 6]).then(|| crate::format_mac(peer)),
    ];
    keys.iter().flatten().find_map(|key| {
        list.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p.clone())
    })
}

/// What an open view reads every frame. Settings change it while the view runs.
#[cfg(feature = "ui")]
pub fn live_view() -> etherquic_play::LiveView {
    static VIEW: std::sync::OnceLock<etherquic_play::LiveView> = std::sync::OnceLock::new();
    Arc::clone(VIEW.get_or_init(|| etherquic_play::live(etherquic_play::ViewOptions::default())))
}

#[cfg(feature = "ui")]
static HOST_AUDIO: Mutex<Option<String>> = Mutex::new(None);

/// Desktop audio source the next shared session opens. `None` picks one.
#[cfg(feature = "ui")]
pub fn set_host_audio_device(name: Option<String>) {
    match HOST_AUDIO.lock() {
        Ok(mut guard) => *guard = name,
        Err(poisoned) => *poisoned.into_inner() = name,
    }
}

#[cfg(feature = "ui")]
fn host_audio_device() -> Option<String> {
    match HOST_AUDIO.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(feature = "ui")]
static PLAYER_INPUT: Mutex<Option<tokio::sync::mpsc::Sender<Vec<etherquic_input::Event>>>> =
    Mutex::new(None);

#[cfg(feature = "ui")]
fn attach_player_input(tx: tokio::sync::mpsc::Sender<Vec<etherquic_input::Event>>) {
    match PLAYER_INPUT.lock() {
        Ok(mut guard) => *guard = Some(tx),
        Err(poisoned) => *poisoned.into_inner() = Some(tx),
    }
}

#[cfg(feature = "ui")]
fn detach_player_input() {
    match PLAYER_INPUT.lock() {
        Ok(mut guard) => *guard = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

/// Queue EQIN key events on the open Android player session.
#[cfg(feature = "ui")]
pub fn send_player_keys(presses: &[(bool, u32)]) -> bool {
    if presses.is_empty() {
        return true;
    }
    let events: Vec<etherquic_input::Event> = presses
        .iter()
        .map(|&(down, code)| etherquic_input::Event {
            time_us: 0,
            body: etherquic_input::Body::Key {
                down,
                source: etherquic_input::KeySource::Keysym,
                code,
            },
        })
        .collect();
    send_player_events(events)
}

/// Queue EQIN events (keys, pad, mouse) on the open Android player session.
#[cfg(feature = "ui")]
pub fn send_player_events(events: Vec<etherquic_input::Event>) -> bool {
    if events.is_empty() {
        return true;
    }
    let tx = match PLAYER_INPUT.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let Some(tx) = tx else {
        return false;
    };
    tx.try_send(events).is_ok()
}

/// Show or hide the phone IME. No-op off Android.
#[cfg(feature = "ui")]
pub fn set_soft_keyboard(show: bool) {
    #[cfg(target_os = "android")]
    gpui::set_android_ime(show);
    let _ = show;
}

/// Real pointer down/move/up instead of pan-as-scroll. Needed for analog sticks.
#[cfg(feature = "ui")]
pub fn set_touch_pointer(on: bool) {
    #[cfg(target_os = "android")]
    gpui::set_android_touch_pointer(on);
    let _ = on;
}

fn client_thread(
    net: Option<Arc<L2Net>>,
    nic: Option<String>,
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    trace_input: bool,
    codecs: CodecOffer,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    picture: Arc<AtomicU64>,
) {
    #[cfg(feature = "ui")]
    {
        #[cfg(target_os = "macos")]
        {
            // The picture window needs the main thread, which GPUI holds.
            drop(net);
            drop(picture);
            child_view(nic, peer, udp, trace_input, codecs, stop, status);
        }
        #[cfg(target_os = "android")]
        {
            let _ = (net, nic, picture, trace_input);
            android_join(peer, udp, codecs, stop, status);
        }
        #[cfg(not(any(target_os = "macos", target_os = "android")))]
        {
            let _ = nic;
            open_view(net, peer, udp, trace_input, codecs, stop, status, picture);
        }
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = (trace_input, nic, codecs, picture);
        client_quic(net, peer, udp, stop, status);
    }
}

/// macOS: the picture window runs in a child `etherquic-play connect`.
#[cfg(all(feature = "ui", target_os = "macos"))]
fn child_view(
    nic: Option<String>,
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    trace_input: bool,
    codecs: CodecOffer,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
) {
    let view = match live_view().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let connect = crate::child_view::ChildConnect {
        udp,
        nic,
        peer_mac: peer,
        local_mac: CLIENT_MAC,
        service_id: PLAY_SERVICE_ID,
        trace_input,
        view,
        codecs,
    };
    let last = Arc::new(Mutex::new(String::new()));
    let slot = Arc::clone(&status);
    let last_slot = Arc::clone(&last);
    let report = move |text: &str| {
        write_status(&slot, text);
        remember_session(peer, udp, text);
        match last_slot.lock() {
            Ok(mut guard) => *guard = text.to_string(),
            Err(poisoned) => *poisoned.into_inner() = text.to_string(),
        }
    };
    if let Err(err) = crate::child_view::run(&connect, &stop, &report) {
        write_status(&status, format!("Connect failed: {err}"));
        return;
    }
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let text = match last.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if !status_stands(&text) {
        write_status(&status, "Disconnected. The view closed.");
    }
}

#[cfg(all(feature = "ui", target_os = "android"))]
fn android_join(
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    codecs: CodecOffer,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
) {
    let Some(addr) = udp else {
        write_status(
            &status,
            "Connect needs a UDP address. Android has no Ethernet.",
        );
        return;
    };
    write_status(&status, format!("Dialing {addr}."));
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            write_status(&status, format!("Connect failed: {err}"));
            return;
        }
    };
    let offer = codecs.offer();
    let (_live_tx, live_rx) = tokio::sync::watch::channel(etherquic_play::LiveOffer {
        look: offer.look,
        dither: offer.dither.unwrap_or_default(),
        pixel_size: offer.pixel_size.unwrap_or(2),
        dither_strength: offer.dither_strength.unwrap_or(60),
        quality: offer.quality,
        dirty_rects: offer.want_dirty_rects.unwrap_or(false),
        yuv: offer.yuv.unwrap_or_default(),
    });
    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<etherquic_play::AudioChunk>(8);
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(32);
    attach_player_input(input_tx);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let frames: etherquic_play::etherquic_media::Sink<etherquic_play::etherquic_media::Frame> =
        std::sync::Arc::new(|_| true);
    let media = etherquic_play::PlayerMedia {
        offer,
        frames,
        events: event_tx,
        live: live_rx,
    };
    let stop_flag = Arc::clone(&stop);
    let outcome = runtime.block_on(async move {
        tokio::spawn(async move { while audio_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
        etherquic_play::join_udp(addr, media, audio_tx, input_rx, stop_flag).await
    });
    detach_player_input();
    match outcome {
        Ok(()) => {
            if !stop.load(Ordering::SeqCst) {
                write_status(&status, "The view closed.");
            }
        }
        Err(err) => write_status(
            &status,
            etherquic_play::session_end_status(&err.to_string()),
        ),
    }
    let _ = peer;
}

#[cfg(all(feature = "ui", not(any(target_os = "macos", target_os = "android"))))]
fn open_view(
    net: Option<Arc<L2Net>>,
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    trace_input: bool,
    codecs: CodecOffer,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    picture: Arc<AtomicU64>,
) {
    let slot = Arc::clone(&status);
    let last = Arc::new(Mutex::new(String::new()));
    let last_slot = Arc::clone(&last);
    let report: etherquic_play::Status = Arc::new(move |text: &str| {
        write_status(&slot, text);
        remember_session(peer, udp, text);
        match last_slot.lock() {
            Ok(mut guard) => *guard = text.to_string(),
            Err(poisoned) => *poisoned.into_inner() = text.to_string(),
        }
    });
    let session = etherquic_play::ConnectSession {
        local_mac: CLIENT_MAC,
        peer_mac: peer,
        service_id: PLAY_SERVICE_ID,
        trace_input,
        udp,
        view: live_view(),
        codecs,
        picture,
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        etherquic_play::connect_view(net, session, Arc::clone(&stop), report)
    }));
    if let Err(err) = session_error(outcome) {
        write_status(&status, format!("Connect failed: {err}"));
        return;
    }
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let text = match last.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if !status_stands(&text) {
        write_status(&status, "The view closed.");
    }
}

/// A failure, a timeout, or the host closing the session should stay on screen.
#[cfg(feature = "ui")]
fn status_stands(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("fail")
        || text.contains("timed")
        || text.contains("closed")
        || text.contains("ended")
        || text.contains("dropped")
}

#[cfg(feature = "ui")]
fn session_error(
    outcome: Result<
        Result<(), Box<dyn std::error::Error + Send + Sync>>,
        Box<dyn std::any::Any + Send>,
    >,
) -> Result<(), String> {
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("the desktop session closed unexpectedly".into()),
    }
}

#[cfg(not(feature = "ui"))]
fn client_quic(
    net: Option<Arc<L2Net>>,
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            write_status(&status, format!("Connect failed: {e}"));
            return;
        }
    };
    runtime.block_on(async move {
        let dial = udp.unwrap_or_else(|| peer_to_socket_addr(peer, PLAY_SERVICE_ID));
        let endpoint = if udp.is_some() {
            let bind = if dial.is_ipv4() {
                SocketAddr::from(([0, 0, 0, 0], 0))
            } else {
                SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
            };
            match etherquic::udp_client_endpoint_on(bind) {
                Ok(endpoint) => endpoint,
                Err(e) => {
                    write_status(&status, format!("Connect failed: {e}"));
                    return;
                }
            }
        } else {
            let Some(net) = net else {
                write_status(&status, "Connect failed: no adapter");
                return;
            };
            match l2_client_endpoint(net, CLIENT_MAC, PLAY_SERVICE_ID) {
                Ok(endpoint) => endpoint,
                Err(e) => {
                    write_status(&status, format!("Connect failed: {e}"));
                    return;
                }
            }
        };
        let addr = dial;
        match endpoint.connect(addr, "localhost") {
            Ok(connecting) => {
                let connected = tokio::select! {
                    result = tokio::time::timeout(Duration::from_secs(10), connecting) => {
                        Dial::Finished(result)
                    }
                    _ = until_stop(&stop) => Dial::Stopped,
                };
                match connected {
                    Dial::Stopped => {}
                    Dial::Finished(Ok(Ok(conn))) => {
                        write_status(
                            &status,
                            format!(
                                "Playing on {}. This window stays off the taskbar.",
                                crate::format_mac(peer)
                            ),
                        );
                        tokio::select! {
                            reason = conn.closed() => {
                                write_status(
                                    &status,
                                    etherquic_play::session_end_status(&reason.to_string()),
                                );
                            }
                            _ = until_stop(&stop) => {}
                        }
                        drop(conn);
                    }
                    Dial::Finished(Ok(Err(e))) => {
                        write_status(&status, format!("Connect failed: {e}"));
                    }
                    Dial::Finished(Err(_)) => write_status(&status, "Connect timed out."),
                }
            }
            Err(e) => write_status(&status, format!("Connect failed: {e}")),
        }
        endpoint.close(0u32.into(), b"stop");
        let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
    });
}

#[cfg(not(feature = "ui"))]
enum Dial<T> {
    Stopped,
    Finished(T),
}

#[cfg(not(feature = "ui"))]
async fn until_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn cert_paths() -> Result<(PathBuf, PathBuf), String> {
    let dir = std::env::var("ETHERQUIC_CERTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("certs")
        });
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    if !cert.is_file() || !key.is_file() {
        return Err(format!(
            "missing cert.pem or key.pem in {} (set ETHERQUIC_CERTS)",
            dir.display()
        ));
    }
    Ok((cert, key))
}

fn write_status(slot: &Mutex<String>, text: impl Into<String>) {
    let text = text.into();
    eprintln!("{text}");
    match slot.lock() {
        Ok(mut guard) => *guard = text,
        Err(poisoned) => *poisoned.into_inner() = text,
    }
}

fn read_status(slot: &Mutex<String>) -> String {
    match slot.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "ui")]
    #[test]
    fn a_dropped_connection_keeps_its_status() {
        assert!(status_stands("The connection dropped."));
        assert!(status_stands("The host closed the session."));
        assert!(!status_stands("Viewing aa:bb."));
    }

    #[test]
    fn idle_link_has_no_taskbar() {
        let link = Link::idle();
        assert_eq!(link.role(), Role::Idle);
        assert!(!link.shows_taskbar());
        assert_eq!(link.peers(), 0);
        assert!(link.status().contains("Idle"));
    }

    #[cfg(feature = "ui")]
    #[test]
    fn player_keys_reach_the_join_channel() {
        assert!(!send_player_keys(&[(true, 0xff1b)]));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        attach_player_input(tx);
        assert!(send_player_keys(&[(true, 0xff1b), (false, 0xff1b)]));
        let events = rx.try_recv().expect("queued extra key");
        assert_eq!(events.len(), 2);
        match &events[0].body {
            etherquic_input::Body::Key {
                down,
                code,
                source,
            } => {
                assert!(*down);
                assert_eq!(*code, 0xff1b);
                assert_eq!(*source, etherquic_input::KeySource::Keysym);
            }
            other => panic!("{other:?}"),
        }
        detach_player_input();
        assert!(!send_player_keys(&[(true, 0xff1b)]));
    }

    /// Two player sessions, the same acquire and release the window uses.
    /// Closing one must return while the other is still inside `hold_picture`
    /// and the picture-window thread is still drawing it.
    #[cfg(feature = "ui")]
    #[test]
    fn closing_one_session_does_not_join_the_other_picture() {
        use std::collections::HashMap;
        use std::time::{Duration, Instant};

        fn seat(stop: &Arc<AtomicBool>) -> etherquic_play::PictureSeat {
            etherquic_play::PictureSeat {
                id: 0,
                frames: Arc::new(Mutex::new(None)),
                input: Arc::new(etherquic_play::Gate::new()),
                stop: Arc::clone(stop),
                trace: false,
                view: etherquic_play::live(etherquic_play::ViewOptions::default()),
                stamps: Arc::new(Mutex::new(HashMap::new())),
                title: Arc::new(Mutex::new(String::new())),
                cursor: etherquic::cursor::share(),
            }
        }

        fn watching(stop: Arc<AtomicBool>, picture_id: u64) -> Link {
            let flag = Arc::clone(&stop);
            let picture = Arc::new(AtomicU64::new(picture_id));
            let thread = std::thread::Builder::new()
                .name("etherquic-connect".into())
                .spawn(move || {
                    etherquic_play::hold_picture(picture_id, &flag);
                })
                .expect("session thread");
            Link {
                role: Role::Client,
                stop_flag: stop,
                up: Arc::new(AtomicBool::new(true)),
                peers: Arc::new(AtomicUsize::new(1)),
                status: Arc::new(Mutex::new("Viewing".into())),
                thread: Some(thread),
                picture,
            }
        }

        let stop_a = Arc::new(AtomicBool::new(false));
        let stop_b = Arc::new(AtomicBool::new(false));
        let first = etherquic_play::open_picture("one", seat(&stop_a));
        let second = etherquic_play::open_picture("two", seat(&stop_b));
        assert!(etherquic_play::switch_picture(first));
        assert_eq!(etherquic_play::shown_picture(), Some(first));
        assert!(etherquic_play::switch_picture(second));
        assert_eq!(etherquic_play::shown_picture(), Some(second));

        let window_stop = Arc::clone(&stop_b);
        let window = std::thread::spawn(move || {
            while etherquic_play::shown_picture() == Some(second) {
                if window_stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let mut gone = watching(Arc::clone(&stop_a), first);
        let mut stay = watching(Arc::clone(&stop_b), second);
        std::thread::sleep(Duration::from_millis(50));
        assert!(gone.running());
        assert!(stay.running());
        assert!(!window.is_finished());

        let started = Instant::now();
        gone.release();
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "release joined a waiter"
        );
        assert!(
            etherquic_play::switch_picture(stay.picture_id()),
            "the session on screen could not be shown"
        );
        assert_eq!(etherquic_play::shown_picture(), Some(second));
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !window.is_finished(),
            "closing the other picture joined the window"
        );
        assert!(stay.running(), "the session on screen was stopped");
        assert!(
            !etherquic_play::shown_picture().is_some_and(|id| id == first),
            "the closed picture is still the one on screen"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while gone.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !gone.running(),
            "released session did not leave hold_picture"
        );
        gone.reap();

        stay.release();
        let _ = window.join();
        let deadline = Instant::now() + Duration::from_secs(2);
        while stay.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        stay.reap();
        let _ = etherquic_play::close_picture(first);
        let _ = etherquic_play::close_picture(second);
    }
}
