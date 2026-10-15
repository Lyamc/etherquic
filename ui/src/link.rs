//! One L2 play session, either hosting or connecting.
//!
//! The NIC is opened on the caller thread so a missing adapter or Npcap
//! shows up before a worker starts. The Quinn endpoint is created inside
//! that worker's Tokio runtime.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        }
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
    ) -> Result<(), String> {
        self.reap();
        if self.thread.is_some() {
            return Err("stop the current session first".into());
        }
        let (net, opening) = if let Some(addr) = udp {
            (None, format!("Opening the view to {addr}…"))
        } else {
            let label = nic.clone().unwrap_or_else(|| "default adapter".into());
            let net = L2Net::open(nic.as_deref(), MTU, ETHERTYPE_QUIC).map_err(|e| e.to_string())?;
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
        let stop = Arc::clone(&self.stop_flag);
        let status = Arc::clone(&self.status);
        match std::thread::Builder::new()
            .name("etherquic-connect".into())
            .spawn(move || client_thread(net, nic, peer, udp, trace_input, stop, status))
        {
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
    #[cfg(feature = "ui")]
    {
        etherquic_play::hostinfo::host_extras(stop)
    }
    #[cfg(not(feature = "ui"))]
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
    #[cfg(feature = "ui")]
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

fn client_thread(
    net: Option<Arc<L2Net>>,
    nic: Option<String>,
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    trace_input: bool,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
) {
    #[cfg(feature = "ui")]
    {
        #[cfg(target_os = "macos")]
        {
            // The picture window needs the main thread, which GPUI holds.
            drop(net);
            child_view(nic, peer, udp, trace_input, stop, status);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = nic;
            open_view(net, peer, udp, trace_input, stop, status);
        }
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = (trace_input, nic);
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
    };
    let last = Arc::new(Mutex::new(String::new()));
    let slot = Arc::clone(&status);
    let last_slot = Arc::clone(&last);
    let report = move |text: &str| {
        write_status(&slot, text);
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

#[cfg(all(feature = "ui", not(target_os = "macos")))]
fn open_view(
    net: Option<Arc<L2Net>>,
    peer: [u8; 6],
    udp: Option<SocketAddr>,
    trace_input: bool,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
) {
    let slot = Arc::clone(&status);
    let last = Arc::new(Mutex::new(String::new()));
    let last_slot = Arc::clone(&last);
    let report: etherquic_play::Status = Arc::new(move |text: &str| {
        write_status(&slot, text);
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
}
