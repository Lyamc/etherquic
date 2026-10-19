//! One player at a time on the configured MAC and service id.
//!
//! The NIC opens on this thread. The Quinn endpoint is created inside the
//! Tokio runtime, same as the host console. Certificates come from
//! `ETHERQUIC_CERTS` or the directory of this exe. A service's working
//! directory is System32, so nothing here uses `CARGO_MANIFEST_DIR`.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use etherquic::{
    l2_client_endpoint, l2_server_endpoint, peer_to_socket_addr, udp_accept, udp_listeners, L2Net,
    ETHERTYPE_QUIC,
};
use quinn::Incoming;

use crate::config::{self, HostConfig, HostPaths};
use crate::confirm::{self, Decision};
use crate::identity;

const MTU: usize = 1500;

pub enum Note {
    Ready,
    Stopping,
}

#[derive(Clone)]
pub struct Log {
    path: PathBuf,
}

impl Log {
    pub fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn line(&self, message: &str) {
        let line = format!("{} {message}\n", utc_now());
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
        eprint!("{line}");
    }
}

pub fn serve(stop: &AtomicBool, log: &Log, mut note: impl FnMut(Note)) -> Result<(), String> {
    let paths = HostPaths::from_env();
    let existing = fs::read_to_string(&paths.config).ok();
    let cfg = config::resolve(existing.as_deref(), &identity::machine_id())?;
    if existing
        .as_deref()
        .is_none_or(|text| !config::already_written(text, &cfg))
    {
        write_text(&paths.config, &config::render(&cfg))?;
    }
    write_text(
        &paths.address,
        &format!("{}\n", config::format_mac(cfg.mac)),
    )?;
    let nic_label = if cfg.nic.is_empty() {
        "default adapter".to_string()
    } else {
        cfg.nic.clone()
    };
    let access = if cfg.unattended {
        "unattended, no confirmation"
    } else {
        "confirmation required (etherquic-host --approve)"
    };
    log.line(&format!(
        "address {} service {} nic {nic_label}. {access}",
        config::format_mac(cfg.mac),
        cfg.service
    ));
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    let (cert, key) = cert_paths()?;
    let nic = if cfg.nic.is_empty() {
        None
    } else {
        Some(cfg.nic.as_str())
    };
    let net = L2Net::open(nic, MTU, ETHERTYPE_QUIC).map_err(|err| err.to_string())?;
    let discovery_stop = Arc::new(AtomicBool::new(false));
    let picture: crate::extras::Picture = Arc::new(std::sync::Mutex::new(None));
    let discovery = spawn_discovery(
        &cfg,
        &paths.log,
        Arc::clone(&discovery_stop),
        net.l2_backend(),
        Arc::clone(&picture),
    );
    if stop.load(Ordering::SeqCst) {
        discovery_stop.store(true, Ordering::SeqCst);
        for handle in discovery {
            let _ = handle.join();
        }
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    let control = paths.control_dir();
    let result = runtime.block_on(accept_loop(
        net, cfg, cert, key, control, stop, log, &mut note, picture,
    ));
    discovery_stop.store(true, Ordering::SeqCst);
    for handle in discovery {
        let _ = handle.join();
    }
    result
}

fn spawn_discovery(
    cfg: &HostConfig,
    log_path: &Path,
    stop: Arc<AtomicBool>,
    l2_backend: u8,
    picture: crate::extras::Picture,
) -> Vec<thread::JoinHandle<()>> {
    let nic = cfg.nic.clone();
    let probe = etherquic::probe_backends();
    let backend = etherquic::selected_backend(&probe);
    let (video, audio) = etherquic::offer_from(
        probe,
        backend,
        etherquic::nvidia_present(),
        etherquic::gpu_compute_present(),
    );
    let note = format!(
        "{} media backends libopus={} ffmpeg={} gstreamer={} using {}\n",
        utc_now(),
        probe.libopus,
        probe.ffmpeg,
        probe.gstreamer,
        backend.name(),
    );
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = file.write_all(note.as_bytes());
    }
    eprint!("{note}");
    let codecs = etherquic_media::caps::summary(etherquic_media::caps::probe());
    let codecs = format!("{} {codecs}\n", utc_now());
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = file.write_all(codecs.as_bytes());
    }
    eprint!("{codecs}");
    let card = etherquic::discover::HostCard {
        name: crate::identity::computer_name(),
        l2_mac: cfg.mac,
        l2_service: cfg.service,
        // The port the UDP listeners will take: another program can hold
        // the service number on IPv4 (RoSE does on 4433).
        l3_port: etherquic::udp_port_for(cfg.service),
        video: Some(video),
        audio: Some(audio),
        l2_backend,
    };
    let extras: Arc<crate::extras::HostExtras> = Arc::new(crate::extras::HostExtras::start(
        picture,
        cfg.mac,
        Arc::clone(&stop),
    ));
    let card = Arc::new(card);
    let mut handles = Vec::new();
    for udp in [false, true] {
        let nic = nic.clone();
        let card = Arc::clone(&card);
        let extras = Arc::clone(&extras);
        let stop = Arc::clone(&stop);
        let log_path = log_path.to_path_buf();
        let name = if udp {
            "etherquic-discover-udp"
        } else {
            "etherquic-discover"
        };
        match thread::Builder::new().name(name.into()).spawn(move || {
            let nic = if nic.is_empty() { None } else { Some(nic) };
            let served = if udp {
                etherquic::discover::serve_udp(nic.as_deref(), &card, &*extras, &stop)
            } else {
                etherquic::discover::serve_with(nic.as_deref(), &card, &*extras, &stop)
            };
            if let Err(err) = served {
                let kind = if udp { "udp discovery" } else { "discovery" };
                let line = format!("{} {kind}: {err}\n", utc_now());
                if let Ok(mut file) = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    let _ = file.write_all(line.as_bytes());
                }
                eprint!("{line}");
            }
        }) {
            Ok(handle) => handles.push(handle),
            Err(err) => eprintln!("discovery thread: {err}"),
        }
    }
    handles
}

/// Connect to a host and hold the session until `stop` or the host closes it.
pub fn connect(
    stop: &AtomicBool,
    log: &Log,
    peer: [u8; 6],
    nic_override: Option<&str>,
    service_override: Option<u16>,
) -> Result<(), String> {
    let paths = HostPaths::from_env();
    let existing = fs::read_to_string(&paths.config).ok();
    let cfg = config::resolve(existing.as_deref(), &identity::machine_id())?;
    let local = config::client_mac(&identity::machine_id());
    let service = service_override.unwrap_or(cfg.service);
    let nic_owned = nic_override.map(str::to_string).or_else(|| {
        if cfg.nic.is_empty() {
            None
        } else {
            Some(cfg.nic)
        }
    });
    log.line(&format!(
        "client {} connecting to {} service {service}",
        config::format_mac(local),
        config::format_mac(peer)
    ));
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    let net =
        L2Net::open(nic_owned.as_deref(), MTU, ETHERTYPE_QUIC).map_err(|err| err.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime.block_on(client_loop(net, local, peer, service, stop, log))
}

/// Write `control/approve` or `control/deny` for the host service to see.
pub fn signal(approve: bool) -> Result<(), String> {
    let dir = HostPaths::from_env().control_dir();
    fs::create_dir_all(&dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
    let approve_path = dir.join("approve");
    let deny_path = dir.join("deny");
    let (write_path, clear_path) = if approve {
        (&approve_path, &deny_path)
    } else {
        (&deny_path, &approve_path)
    };
    fs::write(write_path, "1\n").map_err(|err| format!("write {}: {err}", write_path.display()))?;
    match fs::remove_file(clear_path) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(format!("clear {}: {err}", clear_path.display())),
    }
    if approve {
        println!("approved the next player");
    } else {
        println!("denied the waiting player");
    }
    Ok(())
}

async fn accept_loop(
    net: std::sync::Arc<L2Net>,
    cfg: HostConfig,
    cert: PathBuf,
    key: PathBuf,
    control: PathBuf,
    stop: &AtomicBool,
    log: &Log,
    note: &mut impl FnMut(Note),
    picture: crate::extras::Picture,
) -> Result<(), String> {
    let endpoint = l2_server_endpoint(net, cfg.mac, cfg.service, &cert, &key)
        .map_err(|err| err.to_string())?;
    let access = if cfg.unattended {
        "Unattended: a player connects with no confirmation."
    } else {
        "Confirmation required. Run etherquic-host --approve."
    };
    let udp = udp_listeners(cfg.service, &cert, &key);
    log.line(&format!(
        "listening. This PC is {} on service {}, UDP port {}. {} {access}",
        config::format_mac(cfg.mac),
        cfg.service,
        udp.port,
        udp.note
    ));
    note(Note::Ready);
    #[cfg(windows)]
    let desk = crate::bridge::Desk::spawn(log, picture);
    #[cfg(not(windows))]
    drop(picture);
    let mut udp_v4 = udp.v4;
    let mut udp_v6 = udp.v6;
    let mut closed_by_endpoint = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let arrival = tokio::select! {
            incoming = endpoint.accept() => Arrival::L2(incoming),
            incoming = udp_accept(udp_v4.as_ref()) => Arrival::Udp4(incoming),
            incoming = udp_accept(udp_v6.as_ref()) => Arrival::Udp6(incoming),
            _ = tokio::time::sleep(Duration::from_millis(200)) => Arrival::Tick,
        };
        let incoming = match arrival {
            Arrival::Tick => continue,
            Arrival::L2(None) => {
                log.line("host endpoint closed");
                closed_by_endpoint = true;
                break;
            }
            Arrival::Udp4(None) => {
                log.line("udp ipv4 endpoint closed");
                udp_v4 = None;
                continue;
            }
            Arrival::Udp6(None) => {
                log.line("udp ipv6 endpoint closed");
                udp_v6 = None;
                continue;
            }
            Arrival::L2(Some(incoming))
            | Arrival::Udp4(Some(incoming))
            | Arrival::Udp6(Some(incoming)) => incoming,
        };
        let remote = incoming.remote_address().to_string();
        match wait_for_player(cfg.unattended, &control, stop, log, &remote).await {
            Verdict::Stop => {
                incoming.refuse();
                break;
            }
            Verdict::Refuse => {
                log.line(&format!("refused {remote}"));
                incoming.refuse();
            }
            Verdict::Accept => {
                if cfg.unattended {
                    log.line(&format!("unattended accept {remote}"));
                } else {
                    log.line(&format!("approved {remote}"));
                }
                // `Incoming`'s await accepts the handshake. Do that only after the gate.
                let accepted = tokio::select! {
                    result = incoming => result,
                    _ = until_stop(stop) => break,
                };
                match accepted {
                    Ok(conn) => {
                        log.line(&format!("player connected ({remote})"));
                        #[cfg(windows)]
                        {
                            log.line("serving picture, audio, and input");
                            tokio::select! {
                                result = desk.serve_player(conn) => {
                                    if let Err(err) = result {
                                        log.line(&format!("play session: {err}"));
                                    }
                                }
                                _ = until_stop(stop) => {}
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            tokio::select! {
                                _ = conn.closed() => {}
                                _ = until_stop(stop) => {}
                            }
                            drop(conn);
                        }
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        log.line("player disconnected");
                    }
                    Err(err) => log.line(&format!("handshake failed: {err}")),
                }
            }
        }
    }
    #[cfg(windows)]
    drop(desk);
    note(Note::Stopping);
    endpoint.close(0u32.into(), b"stop");
    if let Some(endpoint) = udp_v4.as_ref() {
        endpoint.close(0u32.into(), b"stop");
    }
    if let Some(endpoint) = udp_v6.as_ref() {
        endpoint.close(0u32.into(), b"stop");
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
    if closed_by_endpoint && !stop.load(Ordering::SeqCst) {
        return Err("host endpoint closed".into());
    }
    log.line("host stopped");
    Ok(())
}

enum Arrival {
    L2(Option<Incoming>),
    Udp4(Option<Incoming>),
    Udp6(Option<Incoming>),
    Tick,
}

enum Verdict {
    Accept,
    Refuse,
    Stop,
}

async fn wait_for_player(
    unattended: bool,
    control: &Path,
    stop: &AtomicBool,
    log: &Log,
    remote: &str,
) -> Verdict {
    if unattended {
        return Verdict::Accept;
    }
    let approve = control.join("approve");
    let deny = control.join("deny");
    let pending = control.join("pending.txt");
    let verdict = match decision_now(false, control) {
        Decision::Accept => Verdict::Accept,
        Decision::Refuse => Verdict::Refuse,
        Decision::Wait => {
            let _ = fs::create_dir_all(control);
            let _ = fs::write(&pending, format!("waiting {remote}\n"));
            log.line(&format!(
                "player {remote} is waiting. Run etherquic-host --approve or etherquic-host --deny"
            ));
            let deadline = Instant::now() + confirm::CONFIRM_TIMEOUT;
            loop {
                if stop.load(Ordering::SeqCst) {
                    break Verdict::Stop;
                }
                match decision_now(false, control) {
                    Decision::Accept => break Verdict::Accept,
                    Decision::Refuse => break Verdict::Refuse,
                    Decision::Wait if Instant::now() >= deadline => {
                        log.line(&format!("player {remote} was not confirmed"));
                        break Verdict::Refuse;
                    }
                    Decision::Wait => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            }
        }
    };
    let _ = fs::remove_file(&approve);
    let _ = fs::remove_file(&deny);
    let _ = fs::remove_file(&pending);
    verdict
}

fn decision_now(unattended: bool, control: &Path) -> Decision {
    confirm::decision(
        unattended,
        control.join("approve").is_file(),
        control.join("deny").is_file(),
    )
}

async fn client_loop(
    net: std::sync::Arc<L2Net>,
    local: [u8; 6],
    peer: [u8; 6],
    service: u16,
    stop: &AtomicBool,
    log: &Log,
) -> Result<(), String> {
    let endpoint = l2_client_endpoint(net, local, service).map_err(|err| err.to_string())?;
    let addr = peer_to_socket_addr(peer, service);
    let result = match endpoint.connect(addr, "localhost") {
        Ok(connecting) => {
            let connected = tokio::select! {
                // Longer than the host confirmation wait, so --approve can land first.
                result = tokio::time::timeout(Duration::from_secs(70), connecting) => {
                    Dial::Finished(result)
                }
                _ = until_stop(stop) => Dial::Stopped,
            };
            match connected {
                Dial::Stopped => Ok(()),
                Dial::Finished(Ok(Ok(conn))) => {
                    log.line(&format!(
                        "connected to {} as {}",
                        config::format_mac(peer),
                        config::format_mac(local)
                    ));
                    tokio::select! {
                        _ = conn.closed() => {
                            log.line("the host closed the session");
                        }
                        _ = until_stop(stop) => {}
                    }
                    drop(conn);
                    Ok(())
                }
                Dial::Finished(Ok(Err(err))) => Err(format!("connect failed: {err}")),
                Dial::Finished(Err(_)) => Err("connect timed out".into()),
            }
        }
        Err(err) => Err(format!("connect failed: {err}")),
    };
    endpoint.close(0u32.into(), b"stop");
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
    result
}

enum Dial<T> {
    Stopped,
    Finished(T),
}

async fn until_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn cert_paths() -> Result<(PathBuf, PathBuf), String> {
    let dir = if let Some(dir) = std::env::var_os("ETHERQUIC_CERTS") {
        PathBuf::from(dir)
    } else {
        std::env::current_exe()
            .map_err(|err| err.to_string())?
            .parent()
            .ok_or_else(|| "etherquic-host has no directory".to_string())?
            .to_path_buf()
    };
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    if cert.is_file() && key.is_file() {
        return Ok((cert, key));
    }
    let resources = dir.join("..").join("Resources");
    let cert = resources.join("cert.pem");
    let key = resources.join("key.pem");
    if cert.is_file() && key.is_file() {
        return Ok((cert, key));
    }
    Err(format!(
        "missing cert.pem or key.pem in {} (set ETHERQUIC_CERTS, or install them next to etherquic-host.exe)",
        dir.display()
    ))
}

fn write_text(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    fs::write(path, body).map_err(|err| format!("write {}: {err}", path.display()))
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_unix(secs)
}

fn format_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}Z")
}

/// Days since 1970-01-01 to a civil date. Howard Hinnant's `civil_from_days`.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::format_unix;

    #[test]
    fn unix_epoch_is_1970() {
        assert_eq!(format_unix(0), "1970-01-01 00:00:00Z");
    }
}
