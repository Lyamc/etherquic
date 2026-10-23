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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PCAP_MAGIC: u32 = 0xa1b2c3d4;
const LINKTYPE_ETHERNET: u32 = 1;
/// Wireshark `WTAP_ENCAP_USER0`. The Lua dissector binds to this.
const LINKTYPE_USER0: u32 = 147;
/// Wireshark `WTAP_ENCAP_USER1`. `rtp.pcap` uses this.
const LINKTYPE_USER1: u32 = 148;
const SNAPLEN: u32 = 16 * 1024 * 1024;
const ETHER_MIN: usize = 60;
const ETHER_QUEUE: usize = 1024;
const APP_QUEUE: usize = 256;
const EXTRA_QUEUE: usize = 256;

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
rtp.pcap (link type 148) is opened only while the session carrier is UDP/RTP or ST 2110. Each packet is a direction byte then the RTP packet. wireshark/rtp.lua reads it.
avtp.pcap is Ethernet and is opened only while the session carrier is AVB. wireshark/avtp.lua reads EtherType 0x22F0.
";

static ENABLED: AtomicBool = AtomicBool::new(false);
static SHARE: Mutex<Option<Arc<Share>>> = Mutex::new(None);
static JOIN: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
/// Carrier id the open capture should record. Zero until a session sets one.
static CARRIER: AtomicU8 = AtomicU8::new(0);

struct Share {
    ether_tx: SyncSender<Stamped>,
    app_tx: SyncSender<Stamped>,
    rtp_tx: SyncSender<Stamped>,
    avtp_tx: SyncSender<Stamped>,
    cmd_tx: SyncSender<Cmd>,
    dropped_ether: Arc<AtomicU64>,
    dropped_app: Arc<AtomicU64>,
    dir: PathBuf,
}

enum Cmd {
    Open { id: u8, done: SyncSender<()> },
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
    let (rtp_tx, rtp_rx) = sync_channel(EXTRA_QUEUE);
    let (avtp_tx, avtp_rx) = sync_channel(EXTRA_QUEUE);
    let (cmd_tx, cmd_rx) = sync_channel(16);
    let dropped_ether = Arc::new(AtomicU64::new(0));
    let dropped_app = Arc::new(AtomicU64::new(0));
    let writer_dir = dir.clone();
    let handle = thread::Builder::new()
        .name("etherquic-capture".into())
        .spawn(move || writer(ether_rx, app_rx, rtp_rx, avtp_rx, cmd_rx, writer_dir, ether, app))?;
    *lock(&JOIN) = Some(handle);
    *share_guard = Some(Arc::new(Share {
        ether_tx,
        app_tx,
        rtp_tx,
        avtp_tx,
        cmd_tx,
        dropped_ether,
        dropped_app,
        dir: dir.clone(),
    }));
    drop(share_guard);
    ENABLED.store(true, Ordering::Relaxed);
    let id = CARRIER.load(Ordering::Relaxed);
    if matches!(id, 3 | 4 | 5) {
        set_capture_carrier(id);
    }
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

/// Remember which carrier this process is using. While capture is on, open
/// `rtp.pcap` for UDP/RTP (3) and ST 2110 (5), or `avtp.pcap` for AVB (4).
/// A carrier that is off does not get a file.
pub fn set_capture_carrier(id: u8) {
    CARRIER.store(id, Ordering::Relaxed);
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let cmd_tx = {
        let guard = lock(&SHARE);
        let Some(share) = guard.as_ref() else {
            return;
        };
        share.cmd_tx.clone()
    };
    let (done_tx, done_rx) = sync_channel(1);
    if cmd_tx
        .try_send(Cmd::Open {
            id,
            done: done_tx,
        })
        .is_err()
    {
        return;
    }
    let _ = done_rx.recv_timeout(Duration::from_secs(2));
}

/// One RTP packet. Written only while the capture carrier is 3 or 5.
/// The stored packet is the direction byte followed by the RTP bytes.
pub fn note_rtp(dir: u8, packet: &[u8]) {
    let id = CARRIER.load(Ordering::Relaxed);
    if id != 3 && id != 5 {
        return;
    }
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(share) = lock(&SHARE).clone() else {
        return;
    };
    let (sec, usec) = now_us();
    let mut bytes = Vec::with_capacity(1 + packet.len());
    bytes.push(dir);
    bytes.extend_from_slice(packet);
    if share
        .rtp_tx
        .try_send(Stamped { sec, usec, bytes })
        .is_err()
    {
        share.dropped_ether.fetch_add(1, Ordering::Relaxed);
    }
}

/// One AVTP Ethernet frame. Written only while the capture carrier is 4.
pub fn note_avtp(dir: u8, frame: &[u8]) {
    let _ = dir;
    if CARRIER.load(Ordering::Relaxed) != 4 {
        return;
    }
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
    if share.avtp_tx.try_send(packet).is_err() {
        share.dropped_ether.fetch_add(1, Ordering::Relaxed);
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
    rtp_rx: Receiver<Stamped>,
    avtp_rx: Receiver<Stamped>,
    cmd_rx: Receiver<Cmd>,
    dir: PathBuf,
    mut ether: BufWriter<File>,
    mut app: BufWriter<File>,
) {
    let mut rtp: Option<BufWriter<File>> = None;
    let mut avtp: Option<BufWriter<File>> = None;
    let mut rtp_hold: Vec<Stamped> = Vec::new();
    let mut avtp_hold: Vec<Stamped> = Vec::new();
    let mut ether_up = true;
    let mut app_up = true;
    let mut rtp_up = true;
    let mut avtp_up = true;
    let mut cmd_up = true;
    while ether_up || app_up || rtp_up || avtp_up || cmd_up {
        let mut idle = true;
        if cmd_up {
            loop {
                match cmd_rx.try_recv() {
                    Ok(cmd) => {
                        idle = false;
                        apply_cmd(&dir, cmd, &mut rtp, &mut avtp, &mut rtp_hold, &mut avtp_hold);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        cmd_up = false;
                        break;
                    }
                }
            }
        }
        if take_some(&ether_rx, &mut ether_up, |packet| {
            let _ = write_packet(&mut ether, &packet);
        }) {
            idle = false;
        }
        if take_some(&app_rx, &mut app_up, |packet| {
            let _ = write_packet(&mut app, &packet);
        }) {
            idle = false;
        }
        if take_some(&rtp_rx, &mut rtp_up, |packet| {
            push_extra(&mut rtp, &mut rtp_hold, packet);
        }) {
            idle = false;
        }
        if take_some(&avtp_rx, &mut avtp_up, |packet| {
            push_extra(&mut avtp, &mut avtp_hold, packet);
        }) {
            idle = false;
        }
        if idle {
            block_one(
                &ether_rx,
                &app_rx,
                &rtp_rx,
                &avtp_rx,
                &cmd_rx,
                &dir,
                &mut ether,
                &mut app,
                &mut rtp,
                &mut avtp,
                &mut rtp_hold,
                &mut avtp_hold,
                &mut ether_up,
                &mut app_up,
                &mut rtp_up,
                &mut avtp_up,
                &mut cmd_up,
            );
        }
    }
    let _ = ether.flush();
    let _ = app.flush();
    if let Some(file) = rtp.as_mut() {
        let _ = file.flush();
    }
    if let Some(file) = avtp.as_mut() {
        let _ = file.flush();
    }
}

fn take_some(rx: &Receiver<Stamped>, up: &mut bool, mut write: impl FnMut(Stamped)) -> bool {
    if !*up {
        return false;
    }
    match rx.try_recv() {
        Ok(packet) => {
            write(packet);
            true
        }
        Err(TryRecvError::Empty) => false,
        Err(TryRecvError::Disconnected) => {
            *up = false;
            false
        }
    }
}

fn push_extra(file: &mut Option<BufWriter<File>>, hold: &mut Vec<Stamped>, packet: Stamped) {
    if let Some(out) = file.as_mut() {
        let _ = write_packet(out, &packet);
    } else {
        hold.push(packet);
    }
}

fn apply_cmd(
    dir: &std::path::Path,
    cmd: Cmd,
    rtp: &mut Option<BufWriter<File>>,
    avtp: &mut Option<BufWriter<File>>,
    rtp_hold: &mut Vec<Stamped>,
    avtp_hold: &mut Vec<Stamped>,
) {
    let Cmd::Open { id, done } = cmd;
    match id {
        3 | 5 => open_extra(dir, "rtp.pcap", LINKTYPE_USER1, rtp, rtp_hold),
        4 => open_extra(dir, "avtp.pcap", LINKTYPE_ETHERNET, avtp, avtp_hold),
        _ => {}
    }
    let _ = done.send(());
}

fn open_extra(
    dir: &std::path::Path,
    name: &str,
    link: u32,
    slot: &mut Option<BufWriter<File>>,
    hold: &mut Vec<Stamped>,
) {
    if slot.is_none() {
        if let Ok(mut file) = File::create(dir.join(name)) {
            if write_global(&mut file, link).is_ok() && file.flush().is_ok() {
                *slot = Some(BufWriter::new(file));
            }
        }
    }
    if let Some(out) = slot.as_mut() {
        for packet in hold.drain(..) {
            let _ = write_packet(out, &packet);
        }
    }
}

fn block_one(
    ether_rx: &Receiver<Stamped>,
    app_rx: &Receiver<Stamped>,
    rtp_rx: &Receiver<Stamped>,
    avtp_rx: &Receiver<Stamped>,
    cmd_rx: &Receiver<Cmd>,
    dir: &std::path::Path,
    ether: &mut BufWriter<File>,
    app: &mut BufWriter<File>,
    rtp: &mut Option<BufWriter<File>>,
    avtp: &mut Option<BufWriter<File>>,
    rtp_hold: &mut Vec<Stamped>,
    avtp_hold: &mut Vec<Stamped>,
    ether_up: &mut bool,
    app_up: &mut bool,
    rtp_up: &mut bool,
    avtp_up: &mut bool,
    cmd_up: &mut bool,
) {
    let wait = Duration::from_millis(25);
    if *ether_up {
        match ether_rx.recv_timeout(wait) {
            Ok(packet) => {
                let _ = write_packet(ether, &packet);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => *ether_up = false,
        }
        return;
    }
    if *app_up {
        match app_rx.recv_timeout(wait) {
            Ok(packet) => {
                let _ = write_packet(app, &packet);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => *app_up = false,
        }
        return;
    }
    if *rtp_up {
        match rtp_rx.recv_timeout(wait) {
            Ok(packet) => push_extra(rtp, rtp_hold, packet),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => *rtp_up = false,
        }
        return;
    }
    if *avtp_up {
        match avtp_rx.recv_timeout(wait) {
            Ok(packet) => push_extra(avtp, avtp_hold, packet),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => *avtp_up = false,
        }
        return;
    }
    if *cmd_up {
        match cmd_rx.recv_timeout(wait) {
            Ok(cmd) => apply_cmd(dir, cmd, rtp, avtp, rtp_hold, avtp_hold),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => *cmd_up = false,
        }
    }
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
        let _globals = crate::carrier::test_globals();
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
    }

    #[test]
    fn pcap_round_trip_keeps_the_ether_frame_and_the_app_record() {
        let _globals = crate::carrier::test_globals();
        let _hold = gate().lock().unwrap_or_else(|poison| poison.into_inner());
        if capture_active() {
            let _ = capture_stop();
        }
        set_capture_carrier(1);
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

    #[test]
    fn rtp_and_avtp_open_only_for_the_active_carrier() {
        let _globals = crate::carrier::test_globals();
        let _hold = gate().lock().unwrap_or_else(|poison| poison.into_inner());
        if capture_active() {
            let _ = capture_stop();
        }
        set_capture_carrier(1);
        let dir = std::env::temp_dir().join(format!(
            "etherquic-capture-carrier-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _stop = StopGuard;
        capture_start(Some(dir.clone())).unwrap();
        let rtp = vec![0x80, 96, 0, 1, 0, 0, 0x10, 0, 0, 0, 0, 1, 0xAB];
        let avtp = ethernet_avtp();
        note_rtp(DIR_SEND, &rtp);
        note_avtp(DIR_SEND, &avtp);
        assert!(!dir.join("rtp.pcap").is_file(), "carrier 1 opened rtp.pcap");
        assert!(!dir.join("avtp.pcap").is_file(), "carrier 1 opened avtp.pcap");

        set_capture_carrier(3);
        assert!(dir.join("rtp.pcap").is_file());
        assert!(!dir.join("avtp.pcap").is_file(), "UDP/RTP opened avtp.pcap");
        note_rtp(DIR_SEND, &rtp);
        note_avtp(DIR_SEND, &avtp);

        set_capture_carrier(5);
        note_rtp(DIR_RECV, &rtp);
        assert!(!dir.join("avtp.pcap").is_file(), "ST 2110 opened avtp.pcap");

        set_capture_carrier(4);
        assert!(dir.join("avtp.pcap").is_file());
        note_avtp(DIR_SEND, &avtp);
        note_rtp(DIR_SEND, &rtp);

        set_capture_carrier(2);
        note_rtp(DIR_SEND, &rtp);
        note_avtp(DIR_SEND, &avtp);

        capture_stop().unwrap();
        let (network, packets) = read_pcap(&dir.join("rtp.pcap"));
        assert_eq!(network, LINKTYPE_USER1);
        assert_eq!(packets.len(), 2, "off-carrier RTP notes were stored");
        assert_eq!(packets[0][0], DIR_SEND);
        assert_eq!(&packets[0][1..], rtp.as_slice());
        assert_eq!(packets[1][0], DIR_RECV);
        assert_eq!(&packets[1][1..], rtp.as_slice());

        let (network, packets) = read_pcap(&dir.join("avtp.pcap"));
        assert_eq!(network, LINKTYPE_ETHERNET);
        assert_eq!(packets.len(), 1, "off-carrier AVTP notes were stored");
        assert_eq!(&packets[0][12..14], &0x22F0u16.to_be_bytes());
        assert_eq!(packets[0][14], 3);
        let _ = fs::remove_dir_all(&dir);
        set_capture_carrier(1);
    }

    #[test]
    fn the_seam_writes_rtp_or_avtp_only_for_its_carrier() {
        let _globals = crate::carrier::test_globals();
        let _hold = gate().lock().unwrap_or_else(|poison| poison.into_inner());
        if capture_active() {
            let _ = capture_stop();
        }
        set_capture_carrier(1);
        let dir = std::env::temp_dir().join(format!(
            "etherquic-capture-seam-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _stop = StopGuard;
        capture_start(Some(dir.clone())).unwrap();
        let mut rtp = crate::carrier::Seam::open(crate::carrier::UDP_RTP);
        rtp.send(crate::carrier::Lane::Picture, b"picture-bytes", None, std::time::Duration::from_millis(1));
        assert!(!dir.join("rtp.pcap").exists());
        set_capture_carrier(crate::carrier::UDP_RTP);
        rtp.send(crate::carrier::Lane::Picture, b"picture-bytes", None, std::time::Duration::from_millis(1));
        set_capture_carrier(crate::carrier::ST2110);
        let mut st = crate::carrier::Seam::open(crate::carrier::ST2110);
        st.send(crate::carrier::Lane::Picture, b"jpeg-xs-body-bytes", None, std::time::Duration::from_millis(1));
        assert!(!dir.join("avtp.pcap").exists());
        set_capture_carrier(crate::carrier::AVB);
        crate::carrier::note_avb_listener(true);
        let mut avb = crate::carrier::Seam::open(crate::carrier::AVB);
        avb.send(crate::carrier::Lane::Picture, b"avb-picture", None, std::time::Duration::from_millis(1));
        set_capture_carrier(crate::carrier::QUIC_STREAMS);
        avb.send(crate::carrier::Lane::Picture, b"not-captured", None, std::time::Duration::ZERO);
        crate::carrier::note_avb_listener(false);
        rtp.send(crate::carrier::Lane::Picture, b"not-captured", None, std::time::Duration::ZERO);
        capture_stop().unwrap();
        let (network, packets) = read_pcap(&dir.join("rtp.pcap"));
        assert_eq!(network, LINKTYPE_USER1);
        assert!(!packets.is_empty());
        assert!(packets.iter().all(|packet| packet[0] == DIR_SEND));
        let (network, packets) = read_pcap(&dir.join("avtp.pcap"));
        assert_eq!(network, LINKTYPE_ETHERNET);
        assert_eq!(packets.len(), 1);
        assert_eq!(&packets[0][12..14], &0x22F0u16.to_be_bytes());
        let _ = fs::remove_dir_all(&dir);
        set_capture_carrier(1);
    }

    fn ethernet_avtp() -> Vec<u8> {
        let mut frame = vec![0u8; 60];
        frame[12] = 0x22;
        frame[13] = 0xF0;
        frame[14] = 3;
        frame
    }

    struct StopGuard;

    impl Drop for StopGuard {
        fn drop(&mut self) {
            let _ = capture_stop();
            set_capture_carrier(1);
        }
    }
}
