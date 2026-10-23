//! Host side of a session: read the player's offer, pick codecs, encode the
//! newest picture whenever the stream can take one, and switch codecs when an
//! encoder fails or the player sends a new offer.
//!
//! Capture writes into a [`FrameSlot`], which keeps only the newest raw
//! frame. The encoder pulls from it at its own pace, and QUIC flow control
//! pushes back on the encoder, so a slow link drops raw frames before they are
//! encoded. That keeps inter-frame codecs (H.264, H.265, AV1) valid: every
//! coded frame that leaves the encoder reaches the player.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::{mpsc, watch};

use crate::audio::AudioEncoder;
use crate::dirty;
use crate::negotiate::{
    choose_audio, choose_video, session_fps_wish, Audio, FpsWish, HostCaps, Offer, StreamConfig,
    Video, AUDIO2_MAGIC, CLOCK_AUDIO, CLOCK_VIDEO, OFFER_MAGIC, RECORD_CLOCK, RECORD_CURSOR,
    RECORD_DATA, RECORD_PATCH, VIDEO2_MAGIC,
};
use crate::video::{EncodeSetup, VideoEncoder};
use crate::{wire, yuv, Frame, Log, Pcm};

static DIRTY_RECTS: AtomicBool = AtomicBool::new(false);
static CAPTURE_FPS: AtomicU32 = AtomicU32::new(0);

/// Capture rate the desk or the host service set while hosting. `0` keeps the
/// rate the capture loop was started with.
pub fn set_capture_fps(fps: u32) {
    CAPTURE_FPS.store(fps, Ordering::Relaxed);
}

pub fn capture_fps_override() -> Option<u32> {
    let fps = CAPTURE_FPS.load(Ordering::Relaxed);
    (fps > 0).then_some(fps)
}

/// When this computer hosts: skip unchanged pictures, and send MJPEG/MJ2K
/// dirty tiles, for a player that did not send `dirty-rects` in its offer.
pub fn set_dirty_rects(on: bool) {
    DIRTY_RECTS.store(on, Ordering::Relaxed);
}

pub fn dirty_rects() -> bool {
    DIRTY_RECTS.load(Ordering::Relaxed)
}

/// Frame pacing for a capture loop that is already running. A desk or service
/// override replaces the rate the loop started with. `0` clears the override.
pub fn capture_period(started_fps: u32) -> Duration {
    let fps = capture_fps_override().unwrap_or(started_fps).max(1);
    Duration::from_millis((1000 / u64::from(fps)).max(1))
}

fn style_from_offer(offer: Option<&Offer>) -> etherquic::look::Style {
    let Some(offer) = offer else {
        return etherquic::look::Style::default();
    };
    etherquic::look::Style {
        look: offer.look,
        dither: offer.dither.unwrap_or_default(),
        pixel_size: offer
            .pixel_size
            .unwrap_or_else(|| offer.look.default_pixel_size()),
        strength: offer.dither_strength.unwrap_or(60),
    }
}

/// The player's offer wins when it sent `dirty-rects`. An older player leaves
/// the line off, and this host's Settings toggle still applies.
pub fn offer_wants_dirty_rects(offer: Option<&Offer>) -> bool {
    match offer.and_then(|offer| offer.want_dirty_rects) {
        Some(want) => want,
        None => dirty_rects(),
    }
}

pub const LEGACY_VIDEO_MAGIC: &[u8; 4] = b"EVID";
pub const LEGACY_AUDIO_MAGIC: &[u8; 4] = b"EAUD";

#[derive(Default)]
struct SlotState {
    frame: Option<Arc<Frame>>,
    seq: u64,
    /// When [`FrameSlot::put`] stored the frame now in `frame`.
    captured: Option<Instant>,
    closed: bool,
}

/// The newest captured frame. Older ones are replaced, never queued.
#[derive(Clone, Default)]
pub struct FrameSlot {
    inner: Arc<(Mutex<SlotState>, Condvar)>,
}

pub enum Wait {
    /// Sequence, frame, and the instant [`FrameSlot::put`] stored it.
    Frame(u64, Arc<Frame>, Instant),
    Timeout,
    Closed,
}

impl FrameSlot {
    pub fn new() -> FrameSlot {
        FrameSlot::default()
    }

    pub fn put(&self, frame: Frame) {
        let (lock, cond) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.frame = Some(Arc::new(frame));
            state.captured = Some(Instant::now());
            state.seq += 1;
            cond.notify_all();
        }
    }

    pub fn close(&self) {
        let (lock, cond) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.closed = true;
            cond.notify_all();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.0.lock().map(|s| s.closed).unwrap_or(true)
    }

    /// The newest frame, if capture has stored one.
    pub fn latest(&self) -> Option<Arc<Frame>> {
        let (lock, _) = &*self.inner;
        lock.lock().ok().and_then(|state| state.frame.clone())
    }

    /// The newest frame after `seen`, waiting up to `timeout`.
    pub fn wait(&self, seen: u64, timeout: Duration) -> Wait {
        let (lock, cond) = &*self.inner;
        let Ok(mut state) = lock.lock() else {
            return Wait::Closed;
        };
        let deadline = Instant::now() + timeout;
        loop {
            if state.closed {
                return Wait::Closed;
            }
            if state.seq > seen {
                if let Some(frame) = &state.frame {
                    let captured = state.captured.unwrap_or_else(Instant::now);
                    return Wait::Frame(state.seq, frame.clone(), captured);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Wait::Timeout;
            }
            match cond.wait_timeout(state, left) {
                Ok((next, _)) => state = next,
                Err(_) => return Wait::Closed,
            }
        }
    }
}

/// Shrink a wide capture so one carrier record finishes inside the player wait.
pub fn fit_width(frame: &Frame, max_width: u32) -> Frame {
    let max_width = max_width.max(2);
    if frame.width <= max_width || frame.width < 2 || frame.height < 2 {
        return frame.clone();
    }
    let mut width = frame.width.min(max_width);
    width -= width % 2;
    let mut height =
        ((u64::from(frame.height) * u64::from(width)) / u64::from(frame.width)).max(2) as u32;
    height -= height % 2;
    if height == 0 {
        height = 2;
    }
    let mut rgb = vec![0u8; (width as usize) * (height as usize) * 3];
    for y in 0..height {
        let sy = (u64::from(y) * u64::from(frame.height)) / u64::from(height);
        for x in 0..width {
            let sx = (u64::from(x) * u64::from(frame.width)) / u64::from(width);
            let from = ((sy * u64::from(frame.width) + sx) * 3) as usize;
            let to = ((u64::from(y) * u64::from(width) + u64::from(x)) * 3) as usize;
            if from + 3 <= frame.rgb.len() && to + 3 <= rgb.len() {
                rgb[to..to + 3].copy_from_slice(&frame.rgb[from..from + 3]);
            }
        }
    }
    Frame { width, height, rgb }
}

/// JPEG-encode one frame and send it with raw PCM on carriers 3–5.
///
/// No Quinn connection. The audio body is even little-endian samples, with no
/// record prefix, so ST 2110's sample rebuild matches the bytes UDP and AVB return.
pub fn serve_carrier(id: u8, frame: &Frame, pcm: &Pcm) -> Result<(Vec<u8>, Vec<u8>), String> {
    let picture = crate::jpeg::encode(frame, crate::jpeg::QUALITY)?;
    let mut audio = Vec::with_capacity(pcm.samples.len() * 2);
    for sample in &pcm.samples {
        audio.extend_from_slice(&sample.to_le_bytes());
    }
    if picture.is_empty() || audio.is_empty() {
        return Err("carrier picture or audio is empty".into());
    }
    if !etherquic::carrier::host_push(id, &picture, &audio) {
        return Err("carrier did not send".into());
    }
    Ok((picture, audio))
}

/// How long a queued keyframe may wait for a natural I-frame before the host
/// forces one on every copy of the stream.
pub const KEYFRAME_WAIT: Duration = Duration::from_millis(4000);

/// Players ask for a keyframe; each encoder waits [`KEYFRAME_WAIT`] for a
/// natural I-frame, then reopens so every subscriber gets the same IDR.
#[derive(Clone, Default)]
pub struct KeyframeBus {
    gen: Arc<AtomicU64>,
}

impl KeyframeBus {
    pub fn request(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
    }

    fn poll(&self, seen: &mut u64) -> bool {
        let gen = self.gen.load(Ordering::SeqCst);
        if gen > *seen {
            *seen = gen;
            true
        } else {
            false
        }
    }
}

struct Member {
    id: u64,
    conn: Connection,
    exclusive: bool,
}

struct RoomInner {
    exclusive: AtomicBool,
    next_id: AtomicU64,
    members: Mutex<Vec<Member>>,
}

/// Who is in this host: exclusive lock, and the connections to kick.
#[derive(Clone, Default)]
pub struct Room {
    inner: Arc<RoomInner>,
}

impl Default for RoomInner {
    fn default() -> Self {
        Self {
            exclusive: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            members: Mutex::new(Vec::new()),
        }
    }
}

/// Result of offering a seat in a [`Room`].
pub enum Admit {
    Ok(u64),
    Busy,
}

impl Room {
    pub fn locked(&self) -> bool {
        self.inner.exclusive.load(Ordering::SeqCst)
    }

    /// Record this connection. An exclusive offer kicks everyone else.
    pub fn admit(&self, conn: &Connection, exclusive: bool) -> Admit {
        let Ok(mut members) = self.inner.members.lock() else {
            return Admit::Busy;
        };
        if self.inner.exclusive.load(Ordering::SeqCst) {
            return Admit::Busy;
        }
        if exclusive {
            for member in members.drain(..) {
                member.conn.close(1u32.into(), b"exclusive");
            }
            self.inner.exclusive.store(true, Ordering::SeqCst);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        members.push(Member {
            id,
            conn: conn.clone(),
            exclusive,
        });
        Admit::Ok(id)
    }

    pub fn leave(&self, id: u64) {
        let Ok(mut members) = self.inner.members.lock() else {
            return;
        };
        if let Some(index) = members.iter().position(|member| member.id == id) {
            let exclusive = members[index].exclusive;
            members.remove(index);
            if exclusive {
                self.inner.exclusive.store(false, Ordering::SeqCst);
            }
        }
    }
}

/// Shared encode + exclusive lock for every player on this host.
#[derive(Clone, Default)]
pub struct SharedEncode {
    pub keyframes: KeyframeBus,
    pub hub: Option<EncodeHub>,
    pub room: Option<Room>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum EncodeKey {
    Legacy,
    V2(Vec<u8>),
}

impl EncodeKey {
    /// Fields [`Offer::same_codecs`] treats as encoder-preserving are stripped
    /// so look, view-only, exclusive, and a keyframe pulse share one pump.
    fn from_offer(offer: Option<&Offer>) -> Self {
        match offer {
            None => Self::Legacy,
            Some(offer) => {
                let mut offer = offer.clone();
                offer.look = etherquic::look::Look::Off;
                offer.dither = None;
                offer.pixel_size = None;
                offer.dither_strength = None;
                offer.view_only = false;
                offer.exclusive = false;
                offer.want_keyframe = false;
                Self::V2(offer.encode())
            }
        }
    }
}

struct Sub {
    id: u64,
    tx: mpsc::Sender<VideoOut>,
    wait_idr: bool,
}

#[derive(Clone)]
struct FanOut {
    subscribers: Arc<Mutex<Vec<Sub>>>,
    last_config: Arc<Mutex<Option<Vec<u8>>>>,
    keyframes: KeyframeBus,
    intra: Arc<AtomicBool>,
}

impl FanOut {
    fn set_intra(&self, intra: bool) {
        self.intra.store(intra, Ordering::Relaxed);
    }

    fn is_closed(&self) -> bool {
        let Ok(mut subs) = self.subscribers.lock() else {
            return true;
        };
        // A dropped player leaves its sender here until the next packet.
        // Capture never goes idle, so the encode thread has to notice the
        // closed sender itself or it keeps a dead ffmpeg forever.
        subs.retain(|sub| !sub.tx.is_closed());
        subs.is_empty()
    }

    fn send(&self, out: VideoOut) -> bool {
        let must = matches!(&out, VideoOut::Config(_));
        if let VideoOut::Config(bytes) = &out {
            if let Ok(mut slot) = self.last_config.lock() {
                *slot = Some(bytes.clone());
            }
        }
        let intra = self.intra.load(Ordering::Relaxed);
        let snapshot: Vec<(u64, mpsc::Sender<VideoOut>, bool)> = {
            let Ok(subs) = self.subscribers.lock() else {
                return false;
            };
            if subs.is_empty() {
                return false;
            }
            subs.iter()
                .map(|sub| (sub.id, sub.tx.clone(), sub.wait_idr))
                .collect()
        };
        let mut lagged = false;
        let mut dead = Vec::new();
        let mut unblocked = Vec::new();
        for (id, tx, wait_idr) in snapshot {
            if wait_idr && !must && !intra {
                continue;
            }
            let alive = if must {
                tx.blocking_send(out.clone()).is_ok()
            } else {
                match tx.try_send(out.clone()) {
                    Ok(()) => true,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        lagged = true;
                        true
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => false,
                }
            };
            if !alive {
                dead.push(id);
            } else if must {
                unblocked.push(id);
            }
        }
        if lagged {
            self.keyframes.request();
        }
        let Ok(mut subs) = self.subscribers.lock() else {
            return false;
        };
        subs.retain(|sub| !dead.contains(&sub.id));
        if must {
            for sub in subs.iter_mut() {
                if unblocked.contains(&sub.id) {
                    sub.wait_idr = false;
                }
            }
        }
        !subs.is_empty()
    }
}

struct PumpSlot {
    subscribers: Arc<Mutex<Vec<Sub>>>,
    last_config: Arc<Mutex<Option<Vec<u8>>>>,
    next_id: Arc<AtomicU64>,
    offer_tx: watch::Sender<Option<Offer>>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// Drops every subscriber sender when the encode thread ends, including panic.
struct PumpExit {
    subscribers: Arc<Mutex<Vec<Sub>>>,
}

impl Drop for PumpExit {
    fn drop(&mut self) {
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.clear();
        }
    }
}

/// One encoder per matching offer. Late joiners share it and ask for a keyframe.
#[derive(Clone)]
pub struct EncodeHub {
    frames: FrameSlot,
    keyframes: KeyframeBus,
    fps: FpsWish,
    display_hz: Option<u32>,
    caps: crate::negotiate::HostCaps,
    log: Log,
    cap_kbps: Arc<AtomicU64>,
    inner: Arc<Mutex<HashMap<EncodeKey, PumpSlot>>>,
}

impl EncodeHub {
    pub fn new(frames: FrameSlot, keyframes: KeyframeBus, options: &HostOptions) -> Self {
        Self {
            frames,
            keyframes,
            fps: options.fps,
            display_hz: options.display_hz,
            caps: options.caps.clone(),
            log: options.log.clone(),
            cap_kbps: options.cap_kbps.clone(),
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn pump_options(&self) -> HostOptions {
        HostOptions {
            fps: self.fps,
            display_hz: self.display_hz,
            caps: self.caps.clone(),
            log: self.log.clone(),
            cap_kbps: self.cap_kbps.clone(),
            carrier_tap: None,
        }
    }

    fn subscribe(&self, offer: Option<Offer>) -> mpsc::Receiver<VideoOut> {
        let key = EncodeKey::from_offer(offer.as_ref());
        let (tx, rx) = mpsc::channel(8);
        let Ok(mut inner) = self.inner.lock() else {
            return rx;
        };
        self.reap(&mut inner);
        if let Some(slot) = inner.get_mut(&key) {
            let _ = slot.offer_tx.send(offer);
            if let Ok(config) = slot.last_config.lock() {
                if let Some(bytes) = config.clone() {
                    let _ = tx.try_send(VideoOut::Config(bytes));
                }
            }
            let id = slot.next_id.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut subs) = slot.subscribers.lock() {
                subs.push(Sub {
                    id,
                    tx,
                    wait_idr: true,
                });
            }
            self.keyframes.request();
            return rx;
        }
        let subscribers = Arc::new(Mutex::new(vec![Sub {
            id: 1,
            tx,
            wait_idr: false,
        }]));
        let last_config = Arc::new(Mutex::new(None));
        let intra = Arc::new(AtomicBool::new(true));
        let fan = FanOut {
            subscribers: subscribers.clone(),
            last_config: last_config.clone(),
            keyframes: self.keyframes.clone(),
            intra: intra.clone(),
        };
        let frames = self.frames.clone();
        let options = self.pump_options();
        let keyframes = self.keyframes.clone();
        let (offer_tx, offer_rx) = watch::channel(offer);
        let sent = Arc::new(AtomicU64::new(0));
        let exit = PumpExit {
            subscribers: subscribers.clone(),
        };
        let join = std::thread::Builder::new()
            .name("etherquic-video-encode".into())
            .spawn(move || {
                let _exit = exit;
                video_pump(frames, offer_rx, options, fan, sent, keyframes);
            })
            .ok();
        inner.insert(
            key,
            PumpSlot {
                subscribers,
                last_config,
                next_id: Arc::new(AtomicU64::new(2)),
                offer_tx,
                join,
            },
        );
        rx
    }

    fn update_offer(&self, offer: Option<Offer>) {
        let key = EncodeKey::from_offer(offer.as_ref());
        let Ok(inner) = self.inner.lock() else {
            return;
        };
        if let Some(slot) = inner.get(&key) {
            let _ = slot.offer_tx.send(offer);
        }
    }

    fn reap(&self, inner: &mut HashMap<EncodeKey, PumpSlot>) {
        inner.retain(|_, slot| match &slot.join {
            Some(handle) if handle.is_finished() => false,
            None => false,
            Some(_) => true,
        });
    }
}

/// How this session carries media.
pub enum Negotiated {
    /// The peer did not agree on `etherquic/2`, or sent no offer: MJPEG and PCM.
    Legacy,
    V2 {
        offer: Offer,
        offers: RecvStream,
    },
}

impl Negotiated {
    pub fn is_v2(&self) -> bool {
        matches!(self, Negotiated::V2 { .. })
    }
}

/// With `etherquic/2` the player's first stream is its offer. Anything else
/// (or nothing within five seconds) is treated as a player that cannot
/// negotiate. A non-offer stream is handed back so the caller can still use it.
pub async fn accept_offer(
    conn: &Connection,
) -> Result<(Negotiated, Option<(RecvStream, [u8; 4])>), String> {
    if !etherquic::negotiates(conn) {
        return Ok((Negotiated::Legacy, None));
    }
    let accepted = tokio::time::timeout(Duration::from_secs(5), conn.accept_uni()).await;
    let mut recv = match accepted {
        Ok(Ok(recv)) => recv,
        Ok(Err(error)) => return Err(error.to_string()),
        Err(_) => return Ok((Negotiated::Legacy, None)),
    };
    let Some(magic) = wire::read_magic(&mut recv).await? else {
        return Ok((Negotiated::Legacy, None));
    };
    if &magic != OFFER_MAGIC {
        return Ok((Negotiated::Legacy, Some((recv, magic))));
    }
    let record = wire::read_record(&mut recv, OFFER_MAGIC)
        .await?
        .ok_or("offer stream ended")?;
    let offer = Offer::decode(&record)?;
    Ok((
        Negotiated::V2 {
            offer,
            offers: recv,
        },
        None,
    ))
}

/// The two host-to-player media streams, plus the connection mode 2 uses
/// for picture and audio datagrams.
pub struct Streams {
    video: SendStream,
    audio: SendStream,
    conn: Connection,
    v2: bool,
}

pub async fn open_streams(conn: &Connection, v2: bool) -> Result<Streams, String> {
    let (vm, am) = if v2 {
        (VIDEO2_MAGIC, AUDIO2_MAGIC)
    } else {
        (LEGACY_VIDEO_MAGIC, LEGACY_AUDIO_MAGIC)
    };
    let mut video = conn.open_uni().await.map_err(|e| e.to_string())?;
    video
        .set_priority(etherquic::PRIORITY_VIDEO)
        .map_err(|e| e.to_string())?;
    wire::write_magic(&mut video, vm).await?;
    let mut audio = conn.open_uni().await.map_err(|e| e.to_string())?;
    audio
        .set_priority(etherquic::PRIORITY_AUDIO)
        .map_err(|e| e.to_string())?;
    wire::write_magic(&mut audio, am).await?;
    Ok(Streams {
        video,
        audio,
        conn: conn.clone(),
        v2,
    })
}

/// One media lane. Config and cursor stay on `send`. When the agreed carrier
/// is QUIC datagrams, picture and audio records leave as datagrams after that
/// config record has been written.
struct LaneSend {
    send: SendStream,
    conn: Connection,
    v2: bool,
    lane: u8,
    datagrams: bool,
    next_id: u16,
    seam: etherquic::carrier::Seam,
    cap_kbps: Arc<AtomicU64>,
    rate: etherquic::carrier::RateCap,
    loss_at: Instant,
    sent_base: u64,
    lost_base: u64,
    tap: Option<mpsc::UnboundedSender<Vec<u8>>>,
    capture_origin: Option<Instant>,
}

fn capture_at(lane: &mut LaneSend) -> Duration {
    match lane.capture_origin {
        Some(origin) => origin.elapsed(),
        None => {
            lane.capture_origin = Some(Instant::now());
            Duration::ZERO
        }
    }
}

/// Host settings for one session.
#[derive(Clone)]
pub struct HostOptions {
    /// Encode rate when the player sends Auto or no fps line.
    pub fps: FpsWish,
    /// Host display refresh. Used for Match, and as capture's natural ceiling.
    pub display_hz: Option<u32>,
    pub caps: HostCaps,
    pub log: Log,
    /// Encoder cap in kbit/s after a full second above 1% loss. Zero leaves the offer rate.
    pub cap_kbps: Arc<AtomicU64>,
    /// RTP, AVTP, and ST 2110 packets the session emitted. Empty for QUIC streams.
    pub carrier_tap: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

/// Offer bitrate, cut by the carrier cap once loss has reduced it.
pub fn encoder_kbps(offer_kbps: u32, cap_kbps: u64) -> u32 {
    if cap_kbps == 0 {
        return offer_kbps;
    }
    let cap = u32::try_from(cap_kbps).unwrap_or(u32::MAX);
    if offer_kbps == 0 {
        cap
    } else {
        offer_kbps.min(cap)
    }
}

#[derive(Clone)]
enum VideoOut {
    Config(Vec<u8>),
    /// Coded picture. `Some` asks for a clock record on the first chunk of this frame.
    Data(Vec<u8>, Option<Instant>),
    Patch(Vec<u8>, Option<Instant>),
}

/// Aborts session tasks if this future is dropped.
///
/// `serve_shared` stops waiting when the QUIC connection closes. A dropped
/// `JoinHandle` would leave the UDP, AVB, and ST 2110 sends running.
struct AbortOnDrop(Vec<tokio::task::AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for handle in &self.0 {
            handle.abort();
        }
    }
}

/// Send pictures and audio until capture stops (the slot closes and the
/// audio sender drops) or the player goes away.
pub async fn run(
    streams: Streams,
    negotiated: Negotiated,
    frames: FrameSlot,
    audio: mpsc::Receiver<Pcm>,
    options: HostOptions,
    cursor: watch::Receiver<Option<Vec<u8>>>,
) -> Result<(), String> {
    run_shared(
        streams,
        negotiated,
        frames,
        audio,
        options,
        cursor,
        SharedEncode::default(),
    )
    .await
}

/// [`run`] with a room-wide encoder hub, keyframe bus, and exclusive lock.
pub async fn run_shared(
    streams: Streams,
    negotiated: Negotiated,
    frames: FrameSlot,
    audio: mpsc::Receiver<Pcm>,
    options: HostOptions,
    mut cursor: watch::Receiver<Option<Vec<u8>>>,
    shared: SharedEncode,
) -> Result<(), String> {
    let Streams {
        video: video_send,
        audio: audio_send,
        conn,
        v2,
    } = streams;
    let agreed = match &negotiated {
        Negotiated::V2 { offer, .. } => {
            offer.carrier.unwrap_or(etherquic::carrier::QUIC_STREAMS)
        }
        Negotiated::Legacy => etherquic::carrier::QUIC_STREAMS,
    };
    let mut video_lane = LaneSend {
        send: video_send,
        conn: conn.clone(),
        v2,
        lane: etherquic::carrier::Lane::Picture as u8,
        datagrams: agreed == etherquic::carrier::QUIC_DATAGRAMS,
        next_id: 1,
        seam: etherquic::carrier::Seam::open(agreed),
        cap_kbps: options.cap_kbps.clone(),
        rate: etherquic::carrier::RateCap::new(100_000_000),
        loss_at: Instant::now(),
        sent_base: 0,
        lost_base: 0,
        tap: options.carrier_tap.clone(),
        capture_origin: None,
    };
    let audio_lane = LaneSend {
        send: audio_send,
        conn,
        v2,
        lane: etherquic::carrier::Lane::Audio as u8,
        datagrams: agreed == etherquic::carrier::QUIC_DATAGRAMS,
        next_id: 1,
        seam: etherquic::carrier::Seam::open(agreed),
        cap_kbps: options.cap_kbps.clone(),
        rate: etherquic::carrier::RateCap::new(100_000_000),
        loss_at: Instant::now(),
        sent_base: 0,
        lost_base: 0,
        tap: options.carrier_tap.clone(),
        capture_origin: None,
    };
    let keyframes = shared.keyframes.clone();
    let owned_hub = shared
        .hub
        .is_none()
        .then(|| EncodeHub::new(frames.clone(), keyframes.clone(), &options));
    let hub = shared.hub.clone().or(owned_hub).expect("encode hub");
    let _ = frames;
    let (offer_tx, offer_rx) = watch::channel::<Option<Offer>>(None);
    let audio_offer = offer_rx.clone();
    let mut offer_rx = offer_rx;
    let mut offer_task = None;
    if let Negotiated::V2 { offer, mut offers } = negotiated {
        if offer.want_keyframe {
            keyframes.request();
        }
        let _ = offer_tx.send(Some(offer));
        let log = options.log.clone();
        let keyframes = keyframes.clone();
        offer_task = Some(tokio::spawn(async move {
            loop {
                match wire::read_record(&mut offers, OFFER_MAGIC).await {
                    Ok(Some(record)) => match Offer::decode(&record) {
                        Ok(next) => {
                            log(&format!(
                                "player sent a new offer: video {:?} failed {:?} look {}",
                                tokens(&next.video),
                                tokens(&next.failed),
                                next.look.token()
                            ));
                            if next.want_keyframe {
                                keyframes.request();
                            }
                            let _ = offer_tx.send(Some(next));
                        }
                        Err(error) => log(&format!("bad offer: {error}")),
                    },
                    Ok(None) | Err(_) => break,
                }
            }
        }));
    }

    let (out_tx, mut out_rx) = mpsc::channel::<VideoOut>(8);
    let mut sub_rx = hub.subscribe(offer_rx.borrow_and_update().clone());
    let mut encode_key = EncodeKey::from_offer(offer_rx.borrow().as_ref());
    let forward = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                out = sub_rx.recv() => {
                    let Some(out) = out else {
                        break;
                    };
                    if out_tx.send(out).await.is_err() {
                        break;
                    }
                }
                changed = offer_rx.changed() => {
                    if changed.is_err() {
                        continue;
                    }
                    let next = offer_rx.borrow_and_update().clone();
                    let next_key = EncodeKey::from_offer(next.as_ref());
                    if next_key != encode_key {
                        sub_rx = hub.subscribe(next);
                        encode_key = next_key;
                    } else {
                        hub.update_offer(next);
                    }
                }
            }
        }
    });

    let video_task = tokio::spawn(async move {
        let mut cursor_open = true;
        loop {
            tokio::select! {
                biased;
                // UDP, AVB, and ST 2110 do not write the picture on this QUIC
                // stream, so a player that left never errors `recv` below.
                // Prefer the closed connection over a queued frame or the
                // send keeps going after "player disconnected".
                _ = video_lane.conn.closed() => return Ok(()),
                // An encoded picture goes out before a cursor sample. The watch
                // keeps only the latest cursor, so the pointer still follows
                // this frame instead of sitting in front of it.
                out = out_rx.recv() => {
                    let Some(out) = out else { break };
                    write_out(&mut video_lane, out).await?;
                    if cursor_open {
                        cursor_open = write_pending_cursor(&mut cursor, &mut video_lane).await?;
                    }
                }
                changed = cursor.changed(), if cursor_open => {
                    match changed {
                        Ok(()) => {
                            cursor_open = send_cursor(&mut cursor, &mut video_lane).await?;
                        }
                        Err(_) => cursor_open = false,
                    }
                }
            }
        }
        finish(&mut video_lane.send).await
    });

    let audio_task = tokio::spawn(audio_loop(
        audio_lane,
        audio,
        audio_offer,
        options.clone(),
    ));

    // `serve_shared` drops this future when the connection closes. Dropping a
    // JoinHandle detaches the task, so abort here or a carrier send outlives
    // the player and the next handshake times out behind the leftover picture.
    let mut abort = Vec::new();
    if let Some(task) = &offer_task {
        abort.push(task.abort_handle());
    }
    abort.push(forward.abort_handle());
    abort.push(video_task.abort_handle());
    abort.push(audio_task.abort_handle());
    let _abort = AbortOnDrop(abort);

    let (video_join, audio_join) = tokio::join!(video_task, audio_task);
    forward.abort();
    if let Some(task) = offer_task {
        task.abort();
    }
    let video_result = video_join.map_err(|e| e.to_string()).and_then(|r| r);
    let audio_result = audio_join.map_err(|e| e.to_string()).and_then(|r| r);
    video_result.and(audio_result)
}

async fn write_out(lane: &mut LaneSend, out: VideoOut) -> Result<(), String> {
    match out {
        VideoOut::Config(bytes) if lane.v2 => {
            if let Ok(config) = StreamConfig::decode(&bytes) {
                let id = config.carrier.unwrap_or(etherquic::carrier::QUIC_STREAMS);
                lane.datagrams = id == etherquic::carrier::QUIC_DATAGRAMS;
                lane.seam = etherquic::carrier::Seam::open(id);
                etherquic::set_capture_carrier(id);
                etherquic::carrier::apply_agreed_carrier(&lane.conn, id);
            }
            // The config record is on the reliable stream before any picture.
            wire::write_record(&mut lane.send, VIDEO2_MAGIC, &bytes).await?;
            if lane.seam.id() >= etherquic::carrier::UDP_RTP {
                let capture = capture_at(lane);
                let _ = lane
                    .seam
                    .send(etherquic::carrier::Lane::Picture, &bytes, None, capture);
                flush_tap(lane);
            }
            Ok(())
        }
        VideoOut::Config(_) => Ok(()),
        VideoOut::Data(bytes, captured) if lane.v2 && lane.seam.id() >= etherquic::carrier::UDP_RTP => {
            send_clock(lane, VIDEO2_MAGIC, CLOCK_VIDEO, etherquic::carrier::Lane::Picture, captured).await?;
            send_seam(lane, RECORD_DATA, &bytes)
        }
        VideoOut::Data(bytes, captured) if lane.v2 && lane.datagrams => {
            send_clock(lane, VIDEO2_MAGIC, CLOCK_VIDEO, etherquic::carrier::Lane::Picture, captured).await?;
            send_datagram_record(lane, RECORD_DATA, &bytes).await
        }
        VideoOut::Data(bytes, captured) if lane.v2 => {
            send_clock(lane, VIDEO2_MAGIC, CLOCK_VIDEO, etherquic::carrier::Lane::Picture, captured).await?;
            wire::write_typed(&mut lane.send, VIDEO2_MAGIC, RECORD_DATA, &bytes).await
        }
        VideoOut::Data(bytes, _) => wire::write_record(&mut lane.send, LEGACY_VIDEO_MAGIC, &bytes).await,
        VideoOut::Patch(bytes, captured) if lane.v2 && lane.seam.id() >= etherquic::carrier::UDP_RTP => {
            send_clock(lane, VIDEO2_MAGIC, CLOCK_VIDEO, etherquic::carrier::Lane::Picture, captured).await?;
            send_seam(lane, RECORD_PATCH, &bytes)
        }
        VideoOut::Patch(bytes, captured) if lane.v2 && lane.datagrams => {
            send_clock(lane, VIDEO2_MAGIC, CLOCK_VIDEO, etherquic::carrier::Lane::Picture, captured).await?;
            send_datagram_record(lane, RECORD_PATCH, &bytes).await
        }
        VideoOut::Patch(bytes, captured) if lane.v2 => {
            send_clock(lane, VIDEO2_MAGIC, CLOCK_VIDEO, etherquic::carrier::Lane::Picture, captured).await?;
            wire::write_typed(&mut lane.send, VIDEO2_MAGIC, RECORD_PATCH, &bytes).await
        }
        VideoOut::Patch(_, _) => Ok(()),
    }
}

/// A clock record on the same path as the media that follows it.
/// `None` means this chunk is a continuation and already had a clock.
async fn send_clock(
    lane: &mut LaneSend,
    magic: &[u8; 4],
    stream: u8,
    which: etherquic::carrier::Lane,
    captured: Option<Instant>,
) -> Result<(), String> {
    let Some(captured) = captured else {
        return Ok(());
    };
    if !lane.v2 {
        return Ok(());
    }
    let payload = etherquic::negotiate::clock_payload(stream, captured.elapsed());
    if lane.seam.id() >= etherquic::carrier::UDP_RTP {
        let mut body = Vec::with_capacity(1 + payload.len());
        body.push(RECORD_CLOCK);
        body.extend_from_slice(&payload);
        let capture = capture_at(lane);
        let _ = lane.seam.send(which, &body, None, capture);
        flush_tap(lane);
        return Ok(());
    }
    if lane.datagrams {
        return send_datagram_record(lane, RECORD_CLOCK, &payload).await;
    }
    wire::write_typed(&mut lane.send, magic, RECORD_CLOCK, &payload).await
}

fn send_seam(lane: &mut LaneSend, kind: u8, payload: &[u8]) -> Result<(), String> {
    let mut body = Vec::with_capacity(1 + payload.len());
    body.push(kind);
    body.extend_from_slice(payload);
    let capture = capture_at(lane);
    let _ = lane
        .seam
        .send(etherquic::carrier::Lane::Picture, &body, None, capture);
    flush_tap(lane);
    sample_loss(lane);
    Ok(())
}

fn flush_tap(lane: &mut LaneSend) {
    let packets = lane.seam.take_packets();
    if let Some(tap) = &lane.tap {
        for packet in packets {
            let _ = tap.send(packet);
        }
    }
}

fn sample_loss(lane: &mut LaneSend) {
    if lane.seam.id() == etherquic::carrier::UDP_RTP {
        let now = Instant::now();
        let dt = now.saturating_duration_since(lane.loss_at);
        if dt.is_zero() {
            return;
        }
        lane.loss_at = now;
        let cap = lane.seam.drive_cap(&mut lane.rate, dt);
        if lane.rate.reduced() {
            lane.cap_kbps.store(cap / 1000, Ordering::Relaxed);
        } else {
            lane.cap_kbps.store(0, Ordering::Relaxed);
        }
        return;
    }
    if lane.datagrams {
        etherquic::carrier::observe_udp_rtt(lane.conn.rtt(), true);
        etherquic::carrier::apply_agreed_carrier(
            &lane.conn,
            etherquic::carrier::QUIC_DATAGRAMS,
        );
    }
    let stats = lane.conn.stats();
    let now = Instant::now();
    let dt = now.saturating_duration_since(lane.loss_at);
    if dt.is_zero() {
        return;
    }
    let sent = stats.path.sent_packets.saturating_sub(lane.sent_base);
    let lost = stats.path.lost_packets.saturating_sub(lane.lost_base);
    lane.sent_base = stats.path.sent_packets;
    lane.lost_base = stats.path.lost_packets;
    lane.loss_at = now;
    let cap = lane.rate.apply_loss(dt, sent, lost);
    if lane.rate.reduced() {
        lane.cap_kbps.store(cap / 1000, Ordering::Relaxed);
    } else {
        lane.cap_kbps.store(0, Ordering::Relaxed);
    }
}

/// Lane byte, then the shared slice header, then a record body the player
/// already understands (`D` or `R` plus the payload).
async fn send_datagram_record(lane: &mut LaneSend, kind: u8, payload: &[u8]) -> Result<(), String> {
    let mut body = Vec::with_capacity(1 + payload.len());
    body.push(kind);
    body.extend_from_slice(payload);
    let id = lane.next_id;
    lane.next_id = lane.next_id.wrapping_add(1);
    let budget = lane
        .conn
        .max_datagram_size()
        .unwrap_or(1200)
        .saturating_sub(16)
        .max(32);
    for slice in etherquic::carrier::split_record(id, &body, budget) {
        let mut packet = Vec::with_capacity(1 + slice.bytes.len() + 6);
        packet.push(lane.lane);
        packet.extend(slice.encode());
        // `send_datagram` drops the oldest queued datagram when the buffer
        // is full, which punches a hole in the picture still being sent.
        // Wait for room instead; the player's deadline still drops a late one.
        lane.conn
            .send_datagram_wait(packet.into())
            .await
            .map_err(|e| e.to_string())?;
    }
    sample_loss(lane);
    Ok(())
}

/// `false` when the cursor watch has closed.
async fn write_pending_cursor(
    cursor: &mut watch::Receiver<Option<Vec<u8>>>,
    lane: &mut LaneSend,
) -> Result<bool, String> {
    match cursor.has_changed() {
        Ok(false) => Ok(true),
        Err(_) => Ok(false),
        Ok(true) => send_cursor(cursor, lane).await,
    }
}

async fn send_cursor(
    cursor: &mut watch::Receiver<Option<Vec<u8>>>,
    lane: &mut LaneSend,
) -> Result<bool, String> {
    let bytes = cursor.borrow_and_update().clone();
    if lane.v2 {
        if let Some(bytes) = bytes {
            if lane.datagrams {
                send_datagram_record(lane, RECORD_CURSOR, &bytes).await?;
            } else {
                wire::write_typed(&mut lane.send, VIDEO2_MAGIC, RECORD_CURSOR, &bytes).await?;
            }
        }
    }
    Ok(true)
}

async fn finish(send: &mut SendStream) -> Result<(), String> {
    send.finish().map_err(|e| e.to_string())?;
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
}

fn send_patches(
    current: &Active,
    frame: &Frame,
    rects: &[dirty::Rect],
    out: &FanOut,
    sent: &AtomicU64,
    captured: Instant,
) -> Result<bool, String> {
    for rect in rects {
        let (rect, tile) = dirty::crop(frame, *rect)?;
        let coded = current.encoder.encode_intra(&tile)?;
        let payload = dirty::encode_payload(rect, &coded);
        sent.fetch_add(payload.len() as u64, Ordering::Relaxed);
        if !out.send(VideoOut::Patch(payload, Some(captured))) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn tokens(rows: &[Video]) -> Vec<&'static str> {
    rows.iter().map(|row| row.token()).collect()
}

struct Active {
    encoder: VideoEncoder,
    video: Video,
    size: (u32, u32),
    requested_kbps: u32,
    /// Consecutive `push` failures. PyroWave reopens a few times before the
    /// host excludes it for the rest of the session.
    encode_fails: u32,
}

fn video_pump(
    frames: FrameSlot,
    mut offer_rx: watch::Receiver<Option<Offer>>,
    options: HostOptions,
    out: FanOut,
    sent: Arc<AtomicU64>,
    keyframes: KeyframeBus,
) {
    let log = options.log.clone();
    let mut offer = offer_rx.borrow_and_update().clone();
    let v2 = offer.is_some();
    let mut excluded: Vec<Video> = Vec::new();
    let mut active: Option<Active> = None;
    let mut reason = String::new();
    let mut first_config = true;
    let mut seen = 0u64;
    let mut frames_in = 0u64;
    let mut tick = Instant::now();
    let mut tick_bytes = 0u64;
    let mut previous: Option<(u32, u32, Vec<u8>)> = None;
    let mut last_encode: Option<Instant> = None;
    let mut keyframe_seen = 0u64;
    let mut keyframe_wait: Option<Instant> = None;
    let mut fresh_encoder = false;
    // The encode sink runs on this thread for intra codecs and on ffmpeg's
    // reader thread for the rest. The flag sends one clock for the first
    // chunk of the frame that was just pushed.
    let capture_stamp = Arc::new(Mutex::new(Instant::now()));
    let clock_arm = Arc::new(AtomicBool::new(false));
    loop {
        if offer_rx.has_changed().unwrap_or(false) {
            let next = offer_rx.borrow_and_update().clone();
            let keep_encoder = match (&offer, &next) {
                (Some(current), Some(incoming)) => current.same_codecs(incoming),
                (None, None) => true,
                _ => false,
            };
            offer = next;
            if !keep_encoder {
                active = None;
                previous = None;
                reason = "the player sent a new offer".into();
            }
        }
        if keyframes.poll(&mut keyframe_seen) && keyframe_wait.is_none() {
            keyframe_wait = Some(Instant::now());
        }
        if keyframe_wait.is_some_and(|at| at.elapsed() >= KEYFRAME_WAIT) {
            keyframe_wait = None;
            previous = None;
            if active
                .as_ref()
                .is_some_and(|current| !current.video.skip_bad_frame())
            {
                active = None;
                reason = "keyframe requested".into();
            }
        }
        if let Some(row) = active.as_ref() {
            let wish = session_fps_wish(offer.as_ref().map(|o| o.fps), options.fps);
            if let Some(period) = encode_period(wish, row.video.hardware(), options.display_hz) {
                if let Some(last) = last_encode {
                    let left = period.saturating_sub(last.elapsed());
                    if !left.is_zero() {
                        // Capture is paced to this same period. Discarding a
                        // frame that arrived a little early skips a whole extra
                        // capture and the session settles near 40 fps.
                        // last_encode is when the previous frame was taken, so
                        // this sleeps only the remainder after fit and encode.
                        std::thread::sleep(left);
                    }
                }
            }
        }
        let frame = match frames.wait(seen, Duration::from_millis(250)) {
            Wait::Frame(seq, frame, captured) => {
                seen = seq;
                if out.is_closed() {
                    return;
                }
                // Start the period here. Stamping after push returns adds a
                // full period on top of a codec that already finished.
                last_encode = Some(Instant::now());
                if let Ok(mut slot) = capture_stamp.lock() {
                    *slot = captured;
                }
                frame
            }
            Wait::Timeout if out.is_closed() => return,
            Wait::Timeout => continue,
            Wait::Closed => break,
        };
        let (max_w, max_h) = offer
            .as_ref()
            .map(|o| (o.max.width, o.max.height))
            .unwrap_or((0, 0));
        let mut frame = yuv::fit(&frame, max_w, max_h);
        etherquic::look::apply_style(
            &mut frame.rgb,
            frame.width,
            frame.height,
            style_from_offer(offer.as_ref()),
        );
        let offer_kbps = offer.as_ref().map(|o| o.bitrate_kbps).unwrap_or(0);
        let capped = encoder_kbps(offer_kbps, options.cap_kbps.load(Ordering::Relaxed));
        if active.as_ref().is_some_and(|row| row.requested_kbps != capped) {
            active = None;
            previous = None;
        }
        if active
            .as_ref()
            .map(|a| a.size != (frame.width, frame.height))
            .unwrap_or(true)
        {
            // The old encoder (and an ffmpeg reader thread) ends before the
            // next config record, so no stale bytes follow it. The next
            // picture is a full frame: the player clears its canvas on config.
            drop(active.take());
            previous = None;
            loop {
                let (video, encoder_name) = match &offer {
                    None => (Video::Mjpeg, "jpeg".to_string()),
                    Some(offer) => {
                        let pick = choose_video(
                            offer,
                            &options.caps,
                            &excluded,
                            Some((frame.width, frame.height)),
                        );
                        (pick.row, pick.encoder)
                    }
                };
                let asked_kbps = capped;
                let quality = offer.as_ref().map(|o| o.quality).unwrap_or(0);
                let yuv = offer
                    .as_ref()
                    .map(crate::negotiate::pick_pyrowave_yuv)
                    .unwrap_or(crate::negotiate::YuvFormat::YUV420P8);
                let want_dirty = offer_wants_dirty_rects(offer.as_ref());
                let knows_patches =
                    v2 && offer.as_ref().is_some_and(|offer| offer.knows_dirty_rects);
                let fps = encode_fps(
                    offer.as_ref(),
                    options.fps,
                    video.hardware(),
                    options.display_hz,
                );
                let setup = EncodeSetup {
                    video,
                    encoder: encoder_name.clone(),
                    width: frame.width,
                    height: frame.height,
                    fps,
                    kbps: asked_kbps,
                    quality,
                    yuv,
                    dirty_rects: want_dirty,
                };
                let sink_out = out.clone();
                let sink_sent = sent.clone();
                let sink_stamp = Arc::clone(&capture_stamp);
                let sink_arm = Arc::clone(&clock_arm);
                let sink: crate::Sink<Vec<u8>> = Arc::new(move |bytes: Vec<u8>| {
                    sink_sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    let captured = sink_stamp
                        .lock()
                        .map(|slot| *slot)
                        .unwrap_or_else(|poison| *poison.into_inner());
                    let clock = sink_arm.swap(false, Ordering::Relaxed).then_some(captured);
                    sink_out.send(VideoOut::Data(bytes, clock))
                });
                match VideoEncoder::open(&setup, sink) {
                    Ok((encoder, opened_kbps)) => {
                        let mut config = StreamConfig::video(
                            video,
                            &encoder_name,
                            frame.width,
                            frame.height,
                            fps,
                        );
                        config.bitrate_kbps = opened_kbps;
                        config.yuv = yuv.bitstream();
                        config.dirty_rects = video.dirty_rects_mode(want_dirty, knows_patches);
                        if v2 {
                            if let Some(offer) = offer.as_ref() {
                                let picture_ok = offer
                                    .video
                                    .iter()
                                    .any(|video| matches!(video, Video::JpegXs));
                                let opus = offer.audio.iter().any(|audio| matches!(audio, Audio::Opus));
                                let pcm = offer.audio.iter().any(|audio| matches!(audio, Audio::Pcm));
                                etherquic::carrier::prepare_announce(picture_ok, opus && !pcm);
                            }
                            let requested = offer.as_ref().and_then(|row| row.carrier);
                            match etherquic::carrier::agree(
                                true,
                                requested,
                                &etherquic::carrier::local_carriers(0),
                            ) {
                                Ok(id) => config.carrier = Some(id),
                                Err(error) => {
                                    log(&error);
                                    return;
                                }
                            }
                        }
                        config.reason = std::mem::take(&mut reason);
                        if first_config {
                            config.host = options.caps.clone();
                            first_config = false;
                        }
                        log(&format!(
                            "video {} ({}) {}x{} {} fps{}{}{}",
                            video.name(),
                            encoder_name,
                            frame.width,
                            frame.height,
                            fps,
                            if opened_kbps > 0 {
                                format!(" {opened_kbps} kbit/s")
                            } else {
                                String::new()
                            },
                            if config.reason.is_empty() {
                                String::new()
                            } else {
                                format!(", because {}", config.reason)
                            },
                            config
                                .carrier
                                .map(|id| format!(" carrier {id}"))
                                .unwrap_or_default()
                        ));
                        if v2 && !out.send(VideoOut::Config(config.encode())) {
                            return;
                        }
                        out.set_intra(video.skip_bad_frame());
                        fresh_encoder = true;
                        active = Some(Active {
                            encoder,
                            video,
                            size: (frame.width, frame.height),
                            requested_kbps: asked_kbps,
                            encode_fails: 0,
                        });
                        break;
                    }
                    Err(error) => {
                        log(&format!(
                            "{} encoder failed to start: {error}",
                            video.name()
                        ));
                        if video == Video::Mjpeg {
                            log("MJPEG cannot start; the picture stops.");
                            return;
                        }
                        excluded.push(video);
                        reason = format!("{} encoder failed: {error}", video.token());
                    }
                }
            }
        }
        let Some(current) = active.as_mut() else {
            continue;
        };
        let prev = previous
            .as_ref()
            .filter(|(w, h, _)| *w == frame.width && *h == frame.height)
            .map(|(_, _, rgb)| rgb.as_slice());
        let knows_patches = v2 && offer.as_ref().is_some_and(|offer| offer.knows_dirty_rects);
        match dirty::action(
            offer_wants_dirty_rects(offer.as_ref()),
            knows_patches,
            current.video,
            prev,
            &frame.rgb,
            frame.width,
            frame.height,
        ) {
            dirty::Action::Skip => {
                // The newest PyroWave picture is still on the GPU. A skip
                // never calls push, so map it now or it never goes out.
                if let Some(current) = active.as_mut() {
                    match current.encoder.flush() {
                        Ok(true) => {
                            current.encode_fails = 0;
                            frames_in += 1;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            log(&format!(
                                "{} encoder failed to finish the frame: {error}",
                                current.video.name()
                            ));
                        }
                    }
                }
                continue;
            }
            dirty::Action::Patches(rects) => {
                let captured = capture_stamp
                    .lock()
                    .map(|slot| *slot)
                    .unwrap_or_else(|poison| *poison.into_inner());
                match send_patches(current, &frame, &rects, &out, &sent, captured) {
                    Ok(true) => {
                        current.encode_fails = 0;
                        frames_in += 1;
                        previous = Some((frame.width, frame.height, frame.rgb.clone()));
                        continue;
                    }
                    Ok(false) => return,
                    Err(_) => {}
                }
            }
            dirty::Action::Full => {}
        }
        clock_arm.store(true, Ordering::Relaxed);
        match current.encoder.push(&frame) {
            Ok(true) => {
                current.encode_fails = 0;
                frames_in += 1;
                previous = Some((frame.width, frame.height, frame.rgb.clone()));
                if current.video.skip_bad_frame() || fresh_encoder {
                    keyframe_wait = None;
                }
                fresh_encoder = false;
            }
            Ok(false) => return,
            Err(error) => {
                if out.is_closed() {
                    return;
                }
                log(&format!(
                    "{} encoder failed mid-session: {error}",
                    current.video.name()
                ));
                if current.video == Video::Mjpeg {
                    // The pipe's error sticks after ffmpeg exits. Reopen it
                    // once the next picture arrives.
                    active = None;
                    previous = None;
                    continue;
                }
                current.encode_fails = current.encode_fails.saturating_add(1);
                // A short GPU hiccup should not dump PyroWave onto MJPEG.
                if current.video.skip_bad_frame() && current.encode_fails < 8 {
                    reason = format!(
                        "{} encoder failed (retry {}): {error}",
                        current.video.token(),
                        current.encode_fails
                    );
                    active = None;
                    previous = None;
                    continue;
                }
                excluded.push(current.video);
                reason = format!("{} encoder failed: {error}", current.video.token());
                active = None;
                previous = None;
            }
        }
        if tick.elapsed() >= Duration::from_secs(5) {
            let total = sent.load(Ordering::Relaxed);
            let secs = tick.elapsed().as_secs_f64();
            if let Some(current) = &active {
                log(&format!(
                    "sending {} {}x{} {:.1} fps {:.2} Mbit/s",
                    current.video.token(),
                    current.size.0,
                    current.size.1,
                    frames_in as f64 / secs,
                    (total - tick_bytes) as f64 * 8.0 / secs / 1e6
                ));
            }
            tick = Instant::now();
            tick_bytes = total;
            frames_in = 0;
        }
    }
}

fn encode_fps(
    offer: Option<&Offer>,
    host: FpsWish,
    hardware: bool,
    display_hz: Option<u32>,
) -> u32 {
    session_fps_wish(offer.map(|o| o.fps), host).resolve(hardware, display_hz)
}

fn encode_period(wish: FpsWish, hardware: bool, display_hz: Option<u32>) -> Option<Duration> {
    match wish {
        FpsWish::Match if display_hz.is_none() => None,
        wish => {
            let fps = wish.resolve(hardware, display_hz).max(1);
            // Milliseconds truncate 60 fps to 16 ms. That never meets the display.
            Some(Duration::from_nanos(1_000_000_000 / u64::from(fps)))
        }
    }
}

async fn audio_loop(
    mut lane: LaneSend,
    mut audio: mpsc::Receiver<Pcm>,
    mut offer_rx: watch::Receiver<Option<Offer>>,
    options: HostOptions,
) -> Result<(), String> {
    let log = options.log.clone();
    let mut offer = offer_rx.borrow_and_update().clone();
    let mut excluded: Vec<Audio> = Vec::new();
    let mut encoder: Option<(AudioEncoder, Audio)> = None;
    loop {
        let pcm = tokio::select! {
            biased;
            _ = lane.conn.closed() => return Ok(()),
            pcm = audio.recv() => pcm,
        };
        let Some(pcm) = pcm else {
            break;
        };
        let captured = Instant::now();
        if !lane.v2 {
            wire::write_record(&mut lane.send, LEGACY_AUDIO_MAGIC, &pcm.encode()).await?;
            continue;
        }
        if offer_rx.has_changed().unwrap_or(false) {
            offer = offer_rx.borrow_and_update().clone();
            encoder = None;
        }
        if encoder
            .as_ref()
            .map(|(enc, _)| enc.changed(&pcm))
            .unwrap_or(true)
        {
            encoder = None;
            let mut reason = String::new();
            while encoder.is_none() {
                let pick = match &offer {
                    Some(offer) => choose_audio(offer, &options.caps, &excluded),
                    None => break,
                };
                let max_channels = offer.as_ref().map(|o| o.audio_channels).unwrap_or(0);
                let stereo_kbps = offer.as_ref().map(|o| o.audio_bitrate_kbps).unwrap_or(0);
                match AudioEncoder::open(
                    pick.row,
                    pcm.sample_rate,
                    pcm.channels,
                    max_channels,
                    stereo_kbps,
                ) {
                    Ok((enc, rate, channels)) => {
                        let mut config =
                            StreamConfig::audio(pick.row, &pick.encoder, rate, channels);
                        config.reason = std::mem::take(&mut reason);
                        if pick.row == Audio::Opus {
                            config.bitrate_kbps =
                                crate::negotiate::scale_audio_kbps(stereo_kbps, channels);
                        }
                        if let Some(offer) = offer.as_ref() {
                            let picture_ok = offer
                                .video
                                .iter()
                                .any(|video| matches!(video, Video::JpegXs));
                            let opus = offer.audio.iter().any(|audio| matches!(audio, Audio::Opus));
                            let pcm = offer.audio.iter().any(|audio| matches!(audio, Audio::Pcm));
                            etherquic::carrier::prepare_announce(picture_ok, opus && !pcm);
                        }
                        let requested = offer.as_ref().and_then(|row| row.carrier);
                        match etherquic::carrier::agree(
                            true,
                            requested,
                            &etherquic::carrier::local_carriers(0),
                        ) {
                            Ok(id) => config.carrier = Some(id),
                            Err(error) => {
                                log(&error);
                                return Err(error);
                            }
                        }
                        lane.datagrams = config.carrier == Some(etherquic::carrier::QUIC_DATAGRAMS);
                        let id = config.carrier.unwrap_or(etherquic::carrier::QUIC_STREAMS);
                        lane.seam = etherquic::carrier::Seam::open(id);
                        etherquic::set_capture_carrier(id);
                        etherquic::carrier::apply_agreed_carrier(&lane.conn, id);
                        log(&format!(
                            "audio {} {} Hz {} ch (capture {} Hz {} ch)",
                            pick.row.name(),
                            rate,
                            channels,
                            pcm.sample_rate,
                            pcm.channels
                        ));
                        let encoded = config.encode();
                        wire::write_record(&mut lane.send, AUDIO2_MAGIC, &encoded).await?;
                        // ST 2110-30 carries samples only. The config already
                        // went on the stream above.
                        if lane.seam.id() >= etherquic::carrier::UDP_RTP
                            && lane.seam.id() != etherquic::carrier::ST2110
                        {
                            let capture = capture_at(&mut lane);
                            let _ = lane.seam.send(
                                etherquic::carrier::Lane::Audio,
                                &encoded,
                                None,
                                capture,
                            );
                            flush_tap(&mut lane);
                        }
                        encoder = Some((enc, pick.row));
                    }
                    Err(error) => {
                        log(&format!(
                            "{} audio encoder failed: {error}",
                            pick.row.name()
                        ));
                        if pick.row == Audio::Pcm {
                            return Err(error);
                        }
                        excluded.push(pick.row);
                        reason = format!("{} encoder failed: {error}", pick.row.token());
                    }
                }
            }
        }
        let Some((enc, row)) = encoder.as_mut() else {
            continue;
        };
        match enc.push(&pcm) {
            Ok(packets) => {
                let mut stamped = false;
                for packet in packets {
                    if !stamped {
                        send_clock(
                            &mut lane,
                            AUDIO2_MAGIC,
                            CLOCK_AUDIO,
                            etherquic::carrier::Lane::Audio,
                            Some(captured),
                        )
                        .await?;
                        stamped = true;
                    }
                    if lane.seam.id() >= etherquic::carrier::UDP_RTP {
                        let mut body = Vec::with_capacity(1 + packet.len());
                        body.push(RECORD_DATA);
                        body.extend_from_slice(&packet);
                        let capture = capture_at(&mut lane);
                        let _ = lane.seam.send(
                            etherquic::carrier::Lane::Audio,
                            &body,
                            None,
                            capture,
                        );
                        flush_tap(&mut lane);
                        sample_loss(&mut lane);
                    } else if lane.datagrams {
                        send_datagram_record(&mut lane, RECORD_DATA, &packet).await?;
                    } else {
                        wire::write_typed(&mut lane.send, AUDIO2_MAGIC, RECORD_DATA, &packet)
                            .await?;
                    }
                }
            }
            Err(error) => {
                log(&format!(
                    "{} audio encoder failed mid-session: {error}",
                    row.name()
                ));
                excluded.push(*row);
                encoder = None;
            }
        }
    }
    finish(&mut lane.send).await
}

#[cfg(test)]
mod tests {
    use super::{EncodeHub, FrameSlot, HostOptions, KeyframeBus, Room};
    use crate::negotiate::{FpsWish, HostCaps};
    use crate::Frame;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn capture_fps_changes_the_period_while_the_loop_is_up() {
        super::set_capture_fps(0);
        assert_eq!(
            super::capture_period(60),
            Duration::from_millis(1000 / 60)
        );
        super::set_capture_fps(30);
        assert_eq!(super::capture_period(60), Duration::from_millis(1000 / 30));
        super::set_dirty_rects(true);
        assert!(super::dirty_rects());
        super::set_dirty_rects(false);
        assert!(!super::dirty_rects());
        super::set_capture_fps(0);
    }

    #[test]
    fn a_full_second_of_loss_cuts_the_encoder_cap() {
        let mut cap = etherquic::carrier::RateCap::new(100_000_000);
        assert_eq!(cap.apply_loss(Duration::from_millis(1), 100, 100), 100_000_000);
        let bits = cap.apply_loss(Duration::from_secs(1), 100, 2);
        assert_eq!(bits, 80_000_000);
        assert_eq!(super::encoder_kbps(0, bits / 1000), 80_000);
        assert_eq!(super::encoder_kbps(10_000, 0), 10_000);
    }

    #[test]
    fn keyframe_bus_wakes_every_subscriber() {
        let bus = KeyframeBus::default();
        let mut a = 0u64;
        let mut b = 0u64;
        assert!(!bus.poll(&mut a));
        bus.request();
        assert!(bus.poll(&mut a));
        assert!(bus.poll(&mut b));
        assert!(!bus.poll(&mut a));
        bus.request();
        assert!(bus.poll(&mut a));
        assert!(bus.poll(&mut b));
    }

    #[test]
    fn exclusive_room_refuses_a_second_seat() {
        let room = Room::default();
        assert!(!room.locked());
        room.inner.exclusive.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(room.locked());
        room.inner.exclusive.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!room.locked());
    }

    fn tiny_frame() -> Frame {
        Frame {
            width: 4,
            height: 4,
            rgb: vec![180u8; 4 * 4 * 3],
        }
    }

    fn test_hub(frames: FrameSlot) -> EncodeHub {
        EncodeHub::new(
            frames,
            KeyframeBus::default(),
            &HostOptions {
                fps: FpsWish::Fixed(30),
                display_hz: None,
                caps: HostCaps::baseline(),
                log: Arc::new(|_: &str| {}),
                cap_kbps: Arc::new(AtomicU64::new(0)),
                carrier_tap: None,
            },
        )
    }

    fn wait_closed(rx: &mut tokio::sync::mpsc::Receiver<super::VideoOut>, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            match rx.try_recv() {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        panic!("encode hub kept the subscriber open after capture closed");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    #[test]
    fn encode_hub_closes_subscribers_when_capture_closes() {
        let frames = FrameSlot::new();
        let hub = test_hub(frames.clone());
        let mut rx = hub.subscribe(None);
        frames.put(tiny_frame());
        let deadline = Instant::now() + Duration::from_secs(2);
        while rx.try_recv().is_err() {
            if Instant::now() > deadline {
                panic!("encode hub sent no picture");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        frames.close();
        wait_closed(&mut rx, 2);
    }

    #[test]
    fn encode_hub_fans_out_one_encode_to_two_subscribers() {
        let frames = FrameSlot::new();
        let hub = test_hub(frames.clone());
        let mut a = hub.subscribe(None);
        frames.put(tiny_frame());
        let deadline = Instant::now() + Duration::from_secs(2);
        while a.try_recv().is_err() {
            if Instant::now() > deadline {
                panic!("first subscriber got no picture");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut b = hub.subscribe(None);
        hub.keyframes.request();
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut got_b = false;
        let mut n = 1u8;
        while Instant::now() < deadline {
            n = n.wrapping_add(1);
            frames.put(Frame {
                width: 4,
                height: 4,
                rgb: vec![n; 4 * 4 * 3],
            });
            if b.try_recv().is_ok() {
                got_b = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(got_b, "late subscriber got no picture after a keyframe request");
        frames.close();
        wait_closed(&mut a, 2);
        wait_closed(&mut b, 2);
    }
}
