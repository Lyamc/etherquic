//! Picture overlay for session delay, and an optional log of those samples.
//!
//! Input is the time from a local key or click until the host reports that it
//! applied the event. Picture is capture on the host, plus half the current
//! round trip, plus the time until this window presents that frame. Audio is
//! the same shape until the samples are next in line for the speaker. The
//! fourth trace is UDP sent and received, in megabytes per second.
//!
//! A host from before the clock record leaves picture and audio blank. The
//! log lives in the temp directory and drops files older than a day.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RING: usize = 180;
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);
const SWEEP: Duration = Duration::from_secs(60 * 60);

static GRAPH: AtomicBool = AtomicBool::new(false);
static LOG: AtomicBool = AtomicBool::new(false);
static RTT_US: AtomicU64 = AtomicU64::new(0);

struct Latest {
    input: Option<f32>,
    picture: Option<f32>,
    audio: Option<f32>,
    mbytes: f32,
    armed_picture: Option<(Duration, Instant)>,
}

struct Store {
    points: VecDeque<LatestSample>,
    latest: Latest,
    file: Option<File>,
    last_log: Option<Instant>,
    last_sweep: Option<Instant>,
}

struct LatestSample {
    input: Option<f32>,
    picture: Option<f32>,
    audio: Option<f32>,
    mbytes: f32,
}

fn store() -> &'static Mutex<Store> {
    static STORE: Mutex<Store> = Mutex::new(Store {
        points: VecDeque::new(),
        latest: Latest {
            input: None,
            picture: None,
            audio: None,
            mbytes: 0.0,
            armed_picture: None,
        },
        file: None,
        last_log: None,
        last_sweep: None,
    });
    &STORE
}

fn lock() -> std::sync::MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn set_graph(on: bool) {
    GRAPH.store(on, Ordering::Relaxed);
}

pub fn graph_enabled() -> bool {
    GRAPH.load(Ordering::Relaxed)
}

pub fn set_log(on: bool) {
    LOG.store(on, Ordering::Relaxed);
    let mut slot = lock();
    if on {
        open_log(&mut slot);
    } else {
        slot.file = None;
    }
    sweep(&mut slot);
}

pub fn log_enabled() -> bool {
    LOG.load(Ordering::Relaxed)
}

/// True when a local input event should carry a stamp the host can confirm.
pub fn wants_stamps() -> bool {
    graph_enabled() || log_enabled()
}

pub fn set_rtt(rtt: Duration) {
    let us = u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX);
    RTT_US.store(us, Ordering::Relaxed);
}

fn half_rtt_ms() -> f32 {
    RTT_US.load(Ordering::Relaxed) as f32 / 2000.0
}

/// Start of a view. Clears the previous session's traces.
pub fn begin_session() {
    etherquic_media::timing::clear();
    let mut slot = lock();
    slot.points.clear();
    slot.latest = Latest {
        input: None,
        picture: None,
        audio: None,
        mbytes: 0.0,
        armed_picture: None,
    };
    slot.file = None;
    slot.last_log = None;
    if log_enabled() {
        open_log(&mut slot);
    }
    // A view that is not recording still drops logs older than a day.
    sweep(&mut slot);
}

pub fn note_input(ms: f32) {
    if !wants_stamps() {
        return;
    }
    let mut slot = lock();
    slot.latest.input = Some(ms.max(0.0));
    push_point(&mut slot);
}

/// Remember the clock that belongs to the picture just decoded.
pub fn arm_picture(host: Duration, recv: Instant) {
    if !wants_stamps() {
        return;
    }
    lock().latest.armed_picture = Some((host, recv));
}

/// The window presented the picture [`arm_picture`] stored.
pub fn presented_picture() {
    if !wants_stamps() {
        return;
    }
    let mut slot = lock();
    let Some((host, recv)) = slot.latest.armed_picture.take() else {
        return;
    };
    let waited = Instant::now().saturating_duration_since(recv);
    slot.latest.picture = Some(span_ms(host, waited, half_rtt_ms()));
    push_point(&mut slot);
}

/// `buffered` is how long the samples already queued will occupy the speaker.
pub fn hear_audio(buffered: Duration) {
    if !wants_stamps() {
        return;
    }
    let Some(stamp) = etherquic_media::timing::take_audio() else {
        return;
    };
    let waited = Instant::now().saturating_duration_since(stamp.recv);
    let mut slot = lock();
    slot.latest.audio = Some(span_ms(
        stamp.host,
        waited.saturating_add(buffered),
        half_rtt_ms(),
    ));
    push_point(&mut slot);
}

pub fn note_throughput(total_bytes: u64, previous: u64, elapsed: Duration) {
    if !wants_stamps() || elapsed.is_zero() || total_bytes < previous {
        return;
    }
    let mbytes = (total_bytes - previous) as f32 / elapsed.as_secs_f32() / 1.0e6;
    let mut slot = lock();
    slot.latest.mbytes = mbytes;
    push_point(&mut slot);
}

fn span_ms(host: Duration, after_recv: Duration, half_rtt_ms: f32) -> f32 {
    host.as_secs_f32() * 1000.0 + after_recv.as_secs_f32() * 1000.0 + half_rtt_ms
}

fn push_point(slot: &mut Store) {
    slot.points.push_back(LatestSample {
        input: slot.latest.input,
        picture: slot.latest.picture,
        audio: slot.latest.audio,
        mbytes: slot.latest.mbytes,
    });
    while slot.points.len() > RING {
        slot.points.pop_front();
    }
    write_row(slot);
}

fn write_row(slot: &mut Store) {
    if !log_enabled() {
        return;
    }
    if slot
        .last_log
        .is_some_and(|at| at.elapsed() < Duration::from_millis(100))
    {
        return;
    }
    if slot.file.is_none() {
        open_log(slot);
    }
    let Some(file) = slot.file.as_mut() else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let row = format!(
        "{now},{},{},{},{:.3}\n",
        field(slot.latest.input),
        field(slot.latest.picture),
        field(slot.latest.audio),
        slot.latest.mbytes,
    );
    let _ = file.write_all(row.as_bytes());
    slot.last_log = Some(Instant::now());
    if slot
        .last_sweep
        .is_none_or(|at| at.elapsed() >= SWEEP)
    {
        sweep(slot);
    }
}

fn field(value: Option<f32>) -> String {
    match value {
        Some(ms) => format!("{ms:.2}"),
        None => String::new(),
    }
}

pub fn log_dir() -> PathBuf {
    std::env::temp_dir().join("etherquic-latency")
}

fn open_log(slot: &mut Store) {
    let dir = log_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("latency-{now}.csv"));
    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(_) => return,
    };
    let _ = writeln!(
        file,
        "unix_ms,input_ms,picture_ms,audio_ms,megabytes_s"
    );
    slot.file = Some(file);
    slot.last_log = None;
}

fn sweep(slot: &mut Store) {
    slot.last_sweep = Some(Instant::now());
    sweep_dir(&log_dir(), SystemTime::now(), KEEP);
}

/// Delete logs in `dir` whose modified time is older than `keep`.
pub fn sweep_dir(dir: &Path, now: SystemTime, keep: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age > keep {
            let _ = fs::remove_file(path);
        }
    }
}

/// Draw the overlay into an XRGB buffer. Does nothing while the graph is off.
pub fn paint(buffer: &mut [u32], width: u32, height: u32) {
    if !graph_enabled() || width < 80 || height < 40 {
        return;
    }
    let slot = lock();
    let panel_w = 248.min(width.saturating_sub(16)) as i32;
    let panel_h = 78i32;
    let x0 = 8i32;
    let y0 = 8i32;
    fill_rect(buffer, width, height, x0, y0, panel_w, panel_h, 0x101418);
    let rows = [
        ("In", slot.latest.input, 0x00F0C0_40, &slot.points, Series::Input),
        ("Pic", slot.latest.picture, 0x0040E0_80, &slot.points, Series::Picture),
        ("Aud", slot.latest.audio, 0x0060A0_FF, &slot.points, Series::Audio),
        ("MB/s", Some(slot.latest.mbytes), 0x00E8E8_E8, &slot.points, Series::Net),
    ];
    for (index, (label, value, color, points, series)) in rows.iter().enumerate() {
        let y = y0 + 4 + index as i32 * 18;
        text(buffer, width, height, x0 + 4, y + 4, label, *color);
        text(
            buffer,
            width,
            height,
            x0 + 40,
            y + 4,
            &format_value(*value, *series),
            *color,
        );
        spark(buffer, width, height, x0 + 96, y + 2, panel_w - 100, 14, points, *series, *color);
    }
}

#[derive(Clone, Copy)]
enum Series {
    Input,
    Picture,
    Audio,
    Net,
}

fn format_value(value: Option<f32>, series: Series) -> String {
    let Some(value) = value else {
        return "—".into();
    };
    if matches!(series, Series::Net) {
        return format!("{value:.1}");
    }
    if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn sample_of(point: &LatestSample, series: Series) -> Option<f32> {
    match series {
        Series::Input => point.input,
        Series::Picture => point.picture,
        Series::Audio => point.audio,
        Series::Net => Some(point.mbytes),
    }
}

fn spark(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    points: &VecDeque<LatestSample>,
    series: Series,
    color: u32,
) {
    if w < 4 || h < 4 || points.is_empty() {
        return;
    }
    let peak = points
        .iter()
        .filter_map(|point| sample_of(point, series))
        .fold(1.0f32, f32::max)
        .max(1.0);
    let n = points.len();
    let bar_w = (w as usize / n).clamp(1, 3) as i32;
    let mut cursor = x;
    for point in points {
        let Some(value) = sample_of(point, series) else {
            cursor += bar_w;
            continue;
        };
        let bh = ((value / peak) * (h as f32)).round() as i32;
        let bh = bh.clamp(1, h);
        fill_rect(buffer, width, height, cursor, y + h - bh, bar_w.max(1), bh, color);
        cursor += bar_w;
        if cursor >= x + w {
            break;
        }
    }
}

fn fill_rect(buffer: &mut [u32], width: u32, height: u32, x: i32, y: i32, w: i32, h: i32, color: u32) {
    if w <= 0 || h <= 0 {
        return;
    }
    for row in y..y + h {
        if row < 0 || row as u32 >= height {
            continue;
        }
        for col in x..x + w {
            if col < 0 || col as u32 >= width {
                continue;
            }
            let index = row as u32 * width + col as u32;
            if let Some(pixel) = buffer.get_mut(index as usize) {
                *pixel = color;
            }
        }
    }
}

fn text(buffer: &mut [u32], width: u32, height: u32, x: i32, y: i32, text: &str, color: u32) {
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = glyph(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    fill_rect(buffer, width, height, cursor + col, y + row as i32, 1, 1, color);
                }
            }
        }
        cursor += 6;
    }
}

fn glyph(ch: char) -> [u8; 7] {
    match ch {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '.' => [0, 0, 0, 0, 0, 0b00100, 0b00100],
        '-' | '—' => [0, 0, 0, 0b01110, 0, 0, 0],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'c' => [0, 0, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110],
        'd' => [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111],
        'i' => [0b00100, 0, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'm' => [0, 0, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001],
        'n' => [0, 0, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001],
        's' => [0, 0, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        'u' => [0, 0, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        ' ' => [0; 7],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn clock_record_round_trips() {
        let payload = etherquic::negotiate::clock_payload(1, Duration::from_micros(1234));
        let mut record = vec![etherquic::negotiate::RECORD_CLOCK];
        record.extend(payload);
        let (stream, delay) = etherquic::negotiate::decode_clock(&record).expect("clock");
        assert_eq!(stream, 1);
        assert_eq!(delay, Duration::from_micros(1234));
    }

    #[test]
    fn span_adds_host_wait_and_half_the_round_trip() {
        let ms = span_ms(Duration::from_millis(4), Duration::from_millis(6), 10.0);
        assert!((ms - 20.0).abs() < 0.2, "{ms}");
    }

    #[test]
    fn sweep_drops_a_file_older_than_a_day() {
        let dir = std::env::temp_dir().join(format!(
            "etherquic-latency-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("latency-old.csv");
        let fresh = dir.join("latency-new.csv");
        fs::write(&old, b"x").unwrap();
        fs::write(&fresh, b"y").unwrap();
        let past = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
        filetime_set(&old, past);
        sweep_dir(&dir, SystemTime::now(), KEEP);
        assert!(!old.exists());
        assert!(fresh.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    fn filetime_set(path: &Path, when: SystemTime) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn paint_writes_pixels_when_the_graph_is_on() {
        GRAPH.store(true, Ordering::Relaxed);
        {
            let mut slot = lock();
            slot.latest.input = Some(12.0);
            slot.latest.picture = Some(28.0);
            slot.points.clear();
            slot.points.push_back(LatestSample {
                input: Some(12.0),
                picture: Some(28.0),
                audio: None,
                mbytes: 1.5,
            });
        }
        let mut buffer = vec![0u32; 200 * 100];
        paint(&mut buffer, 200, 100);
        assert!(buffer.iter().any(|pixel| *pixel != 0));
        GRAPH.store(false, Ordering::Relaxed);
    }
}
