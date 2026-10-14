//! Optional capture of the frames this process sends and receives.
//!
//! Off until [`capture_start`] or `ETHERQUIC_CAPTURE`. The L2 thread only
//! loads an atomic while it is off, and it never waits on disk while it is on.
//!
//! `ether.pcap` is classic pcap, link type Ethernet. Each packet is one frame
//! handed to or read from the `0x88B6` socket: a QUIC datagram or an EQDS
//! discovery message. Frames shorter than 60 bytes are padded with zeros, the
//! same way a NIC pads them. `payload_len` still marks the real end.
//!
//! `app.pcap` is classic pcap, link type 147 (`USER0`). Each packet is the
//! plaintext record from a desktop session, after QUIC has decrypted it:
//! direction, three zero bytes, a 4-byte stream magic, then the payload.
//! An empty payload is the stream opening. The `u32le` length prefix is not
//! stored; the pcap length is the record length.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PCAP_MAGIC: u32 = 0xa1b2c3d4;
const LINKTYPE_ETHERNET: u32 = 1;
/// Wireshark `WTAP_ENCAP_USER0`. The Lua dissector binds to this.
const LINKTYPE_USER0: u32 = 147;
const SNAPLEN: u32 = 16 * 1024 * 1024;
const ETHER_MIN: usize = 60;
const ETHER_QUEUE: usize = 1024;
const APP_QUEUE: usize = 256;

/// This process handed the bytes to the socket or the QUIC stream.
pub const DIR_SEND: u8 = 1;
/// This process read the bytes from the socket or the QUIC stream.
pub const DIR_RECV: u8 = 2;

const NOTE: &str = "\
Open ether.pcap and app.pcap with the EtherQuic Lua dissector:

  wireshark -X lua_script:wireshark/etherquic.lua ether.pcap

ether.pcap is Ethernet. EtherType 0x88B6 carries a service header, then a QUIC datagram or an EQDS discovery message.
app.pcap uses Wireshark user DLT 147. Each record is the plaintext video (EVID), audio (EAUD), input (EINP), or confirmation (EACK) from this process.
Direction in app.pcap is send (1) or recv (2) for this process.
The dissector ships in the etherquic source tree at wireshark/etherquic.lua.
";

static ENABLED: AtomicBool = AtomicBool::new(false);
static SHARE: Mutex<Option<Arc<Share>>> = Mutex::new(None);
static JOIN: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

struct Share {
    ether_tx: SyncSender<Stamped>,
    app_tx: SyncSender<Stamped>,
    dropped_ether: Arc<AtomicU64>,
    dropped_app: Arc<AtomicU64>,
    dir: PathBuf,
}

struct Stamped {
    sec: u32,
    usec: u32,
    bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct CaptureInfo {
    pub dir: PathBuf,
    pub dropped_ether: u64,
    pub dropped_app: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("capture is already running")]
    AlreadyRunning,
    #[error("capture is not running")]
    NotRunning,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// True while a capture directory is open.
pub fn capture_active() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Directory of the running capture.
pub fn capture_dir() -> Option<PathBuf> {
    lock(&SHARE).as_ref().map(|share| share.dir.clone())
}

/// Start when `ETHERQUIC_CAPTURE` is a non-empty path. The path is the
/// directory itself, not a parent of a new timestamp folder.
pub fn capture_from_env() -> Result<Option<PathBuf>, CaptureError> {
    let Some(path) = std::env::var_os("ETHERQUIC_CAPTURE") else {
        return Ok(None);
    };
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(capture_start(Some(PathBuf::from(path)))?))
}

/// `None` uses `$XDG_STATE_HOME/etherquic/capture/<utc>/`, then
/// `~/.local/state`, then `%LOCALAPPDATA%`.
pub fn capture_start(dir: Option<PathBuf>) -> Result<PathBuf, CaptureError> {
    let mut share_guard = lock(&SHARE);
    if ENABLED.load(Ordering::Relaxed) || share_guard.is_some() {
        return Err(CaptureError::AlreadyRunning);
    }
    let dir = dir.unwrap_or_else(default_capture_dir);
    fs::create_dir_all(&dir)?;
    let mut ether = BufWriter::new(File::create(dir.join("ether.pcap"))?);
    let mut app = BufWriter::new(File::create(dir.join("app.pcap"))?);
    write_global(&mut ether, LINKTYPE_ETHERNET)?;
    write_global(&mut app, LINKTYPE_USER0)?;
    ether.flush()?;
    app.flush()?;
    fs::write(dir.join("wireshark.txt"), NOTE)?;
    let (ether_tx, ether_rx) = sync_channel(ETHER_QUEUE);
    let (app_tx, app_rx) = sync_channel(APP_QUEUE);
    let dropped_ether = Arc::new(AtomicU64::new(0));
    let dropped_app = Arc::new(AtomicU64::new(0));
    let handle = thread::Builder::new()
        .name("etherquic-capture".into())
        .spawn(move || writer(ether_rx, app_rx, ether, app))?;
    *lock(&JOIN) = Some(handle);
    *share_guard = Some(Arc::new(Share {
        ether_tx,
        app_tx,
        dropped_ether,
        dropped_app,
        dir: dir.clone(),
    }));
    ENABLED.store(true, Ordering::Relaxed);
    Ok(dir)
}

/// Flush both files and join the writer.
pub fn capture_stop() -> Result<CaptureInfo, CaptureError> {
    ENABLED.store(false, Ordering::Relaxed);
    let share = lock(&SHARE).take().ok_or(CaptureError::NotRunning)?;
    let dropped_ether = Arc::clone(&share.dropped_ether);
    let dropped_app = Arc::clone(&share.dropped_app);
    let dir = share.dir.clone();
    drop(share);
    let join = lock(&JOIN).take();
    if let Some(handle) = join {
        let _ = handle.join();
    }
    Ok(CaptureInfo {
        dir,
        dropped_ether: dropped_ether.load(Ordering::Relaxed),
        dropped_app: dropped_app.load(Ordering::Relaxed),
    })
}

/// Record one Ethernet frame. Returns immediately when capture is off.
///
/// `dir` is [`DIR_SEND`] or [`DIR_RECV`]. A classic Ethernet pcap has no
/// direction field, so both are written to `ether.pcap`.
pub fn note_ether(dir: u8, frame: &[u8]) {
    let _ = dir;
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(share) = lock(&SHARE).clone() else {
        return;
    };
    let (sec, usec) = now_us();
    let packet = Stamped {
        sec,
        usec,
        bytes: pad_ether(frame),
    };
    if share.ether_tx.try_send(packet).is_err() {
        share.dropped_ether.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one plaintext application record. `payload` is empty for a stream open.
pub fn note_app(dir: u8, magic: &[u8; 4], payload: &[u8]) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(share) = lock(&SHARE).clone() else {
        return;
    };
    let (sec, usec) = now_us();
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.push(dir);
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(magic);
    bytes.extend_from_slice(payload);
    let packet = Stamped { sec, usec, bytes };
    if share.app_tx.try_send(packet).is_err() {
        share.dropped_app.fetch_add(1, Ordering::Relaxed);
    }
}

fn pad_ether(frame: &[u8]) -> Vec<u8> {
    if frame.len() >= ETHER_MIN {
        return frame.to_vec();
    }
    let mut out = Vec::with_capacity(ETHER_MIN);
    out.extend_from_slice(frame);
    out.resize(ETHER_MIN, 0);
    out
}

fn writer(
    ether_rx: Receiver<Stamped>,
    app_rx: Receiver<Stamped>,
    mut ether: BufWriter<File>,
    mut app: BufWriter<File>,
) {
    let mut ether_open = true;
    let mut app_open = true;
    while ether_open || app_open {
        if ether_open {
            match ether_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(packet) => {
                    let _ = write_packet(&mut ether, &packet);
                    while let Ok(more) = ether_rx.try_recv() {
                        let _ = write_packet(&mut ether, &more);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => ether_open = false,
            }
        }
        if app_open {
            loop {
                match app_rx.try_recv() {
                    Ok(packet) => {
                        let _ = write_packet(&mut app, &packet);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        app_open = false;
                        break;
                    }
                }
            }
        }
        if !ether_open && app_open {
            match app_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(packet) => {
                    let _ = write_packet(&mut app, &packet);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => app_open = false,
            }
        }
    }
    let _ = ether.flush();
    let _ = app.flush();
}

fn write_global(out: &mut impl Write, network: u32) -> std::io::Result<()> {
    out.write_all(&PCAP_MAGIC.to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&4u16.to_le_bytes())?;
    out.write_all(&0i32.to_le_bytes())?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(&SNAPLEN.to_le_bytes())?;
    out.write_all(&network.to_le_bytes())?;
    Ok(())
}

fn write_packet(out: &mut impl Write, packet: &Stamped) -> std::io::Result<()> {
    let orig = packet.bytes.len() as u32;
    let incl = packet.bytes.len().min(SNAPLEN as usize);
    out.write_all(&packet.sec.to_le_bytes())?;
    out.write_all(&packet.usec.to_le_bytes())?;
    out.write_all(&(incl as u32).to_le_bytes())?;
    out.write_all(&orig.to_le_bytes())?;
    out.write_all(&packet.bytes[..incl])?;
    out.flush()?;
    Ok(())
}

fn now_us() -> (u32, u32) {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (dur.as_secs() as u32, dur.subsec_micros())
}

fn default_capture_dir() -> PathBuf {
    state_root()
        .join("etherquic")
        .join("capture")
        .join(stamp_utc())
}

fn state_root() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local").join("state");
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local);
        }
    }
    std::env::temp_dir()
}

fn stamp_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Howard Hinnant `civil_from_days`. `z` is days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, DEFAULT_ETHER_TYPE};
    use std::path::Path;
    use std::sync::OnceLock;

    fn gate() -> &'static Mutex<()> {
        static GATE: OnceLock<Mutex<()>> = OnceLock::new();
        GATE.get_or_init(|| Mutex::new(()))
    }

    fn read_pcap(path: &Path) -> (u32, Vec<Vec<u8>>) {
        let bytes = fs::read(path).unwrap();
        assert!(bytes.len() >= 24, "pcap header truncated");
        assert_eq!(&bytes[0..4], &PCAP_MAGIC.to_le_bytes());
        assert_eq!(u16::from_le_bytes(bytes[4..6].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[6..8].try_into().unwrap()), 4);
        let network = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut off = 24;
        let mut packets = Vec::new();
        while off + 16 <= bytes.len() {
            let incl = u32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap()) as usize;
            let orig = u32::from_le_bytes(bytes[off + 12..off + 16].try_into().unwrap()) as usize;
            off += 16;
            assert!(off + incl <= bytes.len(), "packet truncated");
            assert_eq!(incl, orig);
            packets.push(bytes[off..off + incl].to_vec());
            off += incl;
        }
        assert_eq!(off, bytes.len());
        (network, packets)
    }

    #[test]
    fn unix_epoch_and_a_known_day() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
    }

    #[test]
    fn pcap_round_trip_keeps_the_ether_frame_and_the_app_record() {
        let _hold = gate().lock().unwrap_or_else(|poison| poison.into_inner());
        if capture_active() {
            let _ = capture_stop();
        }
        let dir = std::env::temp_dir().join(format!(
            "etherquic-capture-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _stop = StopGuard;
        let started = capture_start(Some(dir.clone())).unwrap();
        assert_eq!(started, dir);

        let frame = Frame {
            dst: [0x02, 0, 0, 0, 0, 2],
            src: [0x02, 0, 0, 0, 0, 1],
            ether_type: DEFAULT_ETHER_TYPE,
            dst_service: 4433,
            src_service: 9,
            payload: vec![0xAA, 0xBB, 0xCC],
        };
        let encoded = frame.encode();
        assert!(encoded.len() < ETHER_MIN);
        note_ether(DIR_SEND, &encoded);

        let payload = [0xFF, 0xD8, 0x00];
        note_app(DIR_RECV, b"EVID", &payload);
        note_app(DIR_SEND, b"EAUD", &[]);

        let info = capture_stop().unwrap();
        assert_eq!(info.dir, dir);
        assert_eq!(info.dropped_ether, 0);
        assert_eq!(info.dropped_app, 0);
        assert!(dir.join("wireshark.txt").is_file());

        let (network, packets) = read_pcap(&dir.join("ether.pcap"));
        assert_eq!(network, LINKTYPE_ETHERNET);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].len(), ETHER_MIN);
        assert_eq!(Frame::decode(&packets[0]).unwrap(), frame);
        assert!(packets[0][encoded.len()..].iter().all(|byte| *byte == 0));

        let (network, packets) = read_pcap(&dir.join("app.pcap"));
        assert_eq!(network, LINKTYPE_USER0);
        assert_eq!(
            packets,
            vec![
                vec![DIR_RECV, 0, 0, 0, b'E', b'V', b'I', b'D', 0xFF, 0xD8, 0x00],
                vec![DIR_SEND, 0, 0, 0, b'E', b'A', b'U', b'D'],
            ]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    struct StopGuard;

    impl Drop for StopGuard {
        fn drop(&mut self) {
            let _ = capture_stop();
        }
    }
}
