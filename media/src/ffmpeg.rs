//! H.264, H.265 and AV1 through an `ffmpeg` child process.
//!
//! The encoder reads raw RGB frames on stdin and writes an Annex-B (or OBU)
//! elementary stream on stdout with every packet flushed; the host forwards
//! those bytes as `EVI2` data records. The player's decoder is the mirror:
//! the elementary stream in, raw RGB frames of the configured size out. Both
//! run with B-frames off and a single decode thread, so a picture is not
//! held back for reordering or frame threads.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::negotiate::{Video, VideoCodec};
use crate::{Frame, Sink};

/// One encoder ffmpeg can run for a [`Video`] row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderSpec {
    pub video: Video,
    pub name: &'static str,
}

/// Encoders tried for each row, best first.
pub fn candidates(video: Video) -> &'static [&'static str] {
    match video {
        Video::H264Hw => {
            if cfg!(target_os = "macos") {
                &["h264_videotoolbox"]
            } else {
                &["h264_nvenc", "h264_amf", "h264_qsv"]
            }
        }
        Video::H265Hw => {
            if cfg!(target_os = "macos") {
                &["hevc_videotoolbox"]
            } else {
                &["hevc_nvenc", "hevc_amf", "hevc_qsv"]
            }
        }
        Video::Av1Hw => &["av1_nvenc", "av1_amf", "av1_qsv"],
        Video::H264Sw => &["libx264"],
        Video::Pyrowave | Video::Mjpeg => &[],
    }
}

fn output_format(codec: VideoCodec) -> &'static str {
    // NUT frames each packet, so the decoder's demuxer hands a picture on as
    // soon as it arrives. A raw elementary stream waits for the next picture's
    // start code first, one frame of extra delay.
    let _ = codec;
    "nut"
}

fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn runs(path: &PathBuf) -> bool {
    let mut command = Command::new(path);
    command.arg("-version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut command);
    command.status().map(|status| status.success()).unwrap_or(false)
}

/// The ffmpeg this process uses: `ETHERQUIC_FFMPEG`, beside the program,
/// Homebrew on macOS (a Finder-started app has no Homebrew on `PATH`), then
/// `PATH` widened with the signed-in user's path and WinGet on Windows.
pub fn binary() -> Option<PathBuf> {
    static BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            etherquic::widen_process_path();
            let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
            let mut list = Vec::new();
            if let Some(path) = std::env::var_os("ETHERQUIC_FFMPEG") {
                list.push(PathBuf::from(path));
            }
            if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
                list.push(dir.join(exe));
            }
            if cfg!(target_os = "macos") {
                list.push(PathBuf::from("/opt/homebrew/bin/ffmpeg"));
                list.push(PathBuf::from("/usr/local/bin/ffmpeg"));
            }
            list.push(PathBuf::from(exe));
            list.into_iter().find(|path| (!path.is_absolute() || path.is_file()) && runs(path))
        })
        .clone()
}

fn command(binary: &PathBuf) -> Command {
    let mut command = Command::new(binary);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    hide_window(&mut command);
    command
}

fn listing(flag: &str) -> String {
    let Some(binary) = binary() else { return String::new() };
    let mut command = Command::new(&binary);
    command.args(["-hide_banner", flag]).stdin(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut command);
    command
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

fn listed(text: &str, name: &str) -> bool {
    text.lines().any(|line| line.split_whitespace().nth(1) == Some(name))
}

/// Encoder arguments after the input. `kbps` is the target rate.
fn encoder_args(name: &str, fps: u32, kbps: u32) -> Vec<String> {
    let fps = fps.max(1);
    let rate = format!("{kbps}k");
    // One frame of buffer: the rate controller cannot borrow from later frames.
    let buffer = format!("{}k", (kbps / fps).max(64) * 2);
    let gop = (fps * 10).to_string();
    let mut args: Vec<String> = vec!["-c:v".into(), name.into(), "-bf".into(), "0".into(), "-g".into(), gop];
    let more: Vec<&str> = match name {
        "libx264" => vec![
            "-preset", "ultrafast", "-tune", "zerolatency", "-x264-params", "repeat-headers=1:sliced-threads=1",
        ],
        n if n.ends_with("_videotoolbox") => vec!["-realtime", "1", "-prio_speed", "1"],
        n if n.ends_with("_nvenc") => vec!["-preset", "p1", "-tune", "ull", "-zerolatency", "1", "-delay", "0", "-rc", "cbr"],
        n if n.ends_with("_amf") => vec!["-usage", "ultralowlatency", "-rc", "cbr"],
        n if n.ends_with("_qsv") => vec!["-preset", "veryfast", "-look_ahead", "0", "-async_depth", "1"],
        _ => vec![],
    };
    args.extend(more.into_iter().map(String::from));
    args.extend(["-b:v".into(), rate.clone(), "-maxrate".into(), rate, "-bufsize".into(), buffer]);
    args
}

fn codec_of(name: &str) -> VideoCodec {
    if name.contains("265") || name.contains("hevc") {
        VideoCodec::H265
    } else if name.contains("av1") {
        VideoCodec::Av1
    } else {
        VideoCodec::H264
    }
}

/// A short real encode, so an encoder that is listed but cannot open (no GPU,
/// no driver, wrong session) is never offered.
pub fn probe_encoder(name: &str) -> Result<(), String> {
    let binary = binary().ok_or("no ffmpeg")?;
    let mut cmd = command(&binary);
    cmd.args(["-f", "lavfi", "-i", "testsrc=size=320x180:rate=30", "-frames:v", "8", "-pix_fmt", "yuv420p"]);
    cmd.args(encoder_args(name, 30, 2000));
    cmd.args(["-f", "null", "-"]);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            if status.success() {
                return Ok(());
            }
            let mut text = String::new();
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_string(&mut text);
            }
            return Err(format!("{name}: {}", text.lines().last().unwrap_or("failed")));
        }
        if started.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{name}: test encode timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Encoders that pass [`probe_encoder`], one per row at most.
pub fn working_encoders() -> Vec<EncoderSpec> {
    if binary().is_none() {
        return Vec::new();
    }
    let list = listing("-encoders");
    let rows = [Video::Av1Hw, Video::H265Hw, Video::H264Hw, Video::H264Sw];
    let handles: Vec<_> = rows
        .into_iter()
        .map(|video| {
            let names: Vec<&'static str> =
                candidates(video).iter().copied().filter(|name| listed(&list, name)).collect();
            std::thread::spawn(move || {
                names
                    .into_iter()
                    .find(|name| probe_encoder(name).is_ok())
                    .map(|name| EncoderSpec { video, name })
            })
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
}

/// Bitstreams this ffmpeg decodes, and the decoder name for each.
pub fn working_decoders() -> Vec<(VideoCodec, &'static str)> {
    if binary().is_none() {
        return Vec::new();
    }
    let list = listing("-decoders");
    let mut out = Vec::new();
    if listed(&list, "h264") {
        out.push((VideoCodec::H264, "h264"));
    }
    if listed(&list, "hevc") {
        out.push((VideoCodec::H265, "hevc"));
    }
    // ffmpeg's own "av1" decoder needs a hardware accelerator; dav1d does not.
    if listed(&list, "libdav1d") {
        out.push((VideoCodec::Av1, "libdav1d"));
    }
    out
}

fn decoder_name(codec: VideoCodec) -> Option<&'static str> {
    static DECODERS: OnceLock<Vec<(VideoCodec, &'static str)>> = OnceLock::new();
    DECODERS
        .get_or_init(working_decoders)
        .iter()
        .find(|(c, _)| *c == codec)
        .map(|(_, name)| *name)
}

/// Default target for a codec, scaled from a 1280x720 30 fps baseline.
pub fn default_kbps(codec: VideoCodec, width: u32, height: u32, fps: u32) -> u32 {
    let base = match codec {
        VideoCodec::H264 => 12_000u64,
        VideoCodec::H265 => 8_000,
        VideoCodec::Av1 => 6_000,
        _ => 12_000,
    };
    let scale = (width as u64 * height as u64 * fps.max(1) as u64).max(1);
    ((base * scale) / (1280 * 720 * 30)).clamp(1_000, 80_000) as u32
}

type Failure = Arc<Mutex<Option<String>>>;

fn stderr_tail(child: &mut Child, failure: Failure) -> Option<JoinHandle<()>> {
    let mut stderr = child.stderr.take()?;
    Some(std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        if let Some(line) = text.lines().rev().find(|l| !l.trim().is_empty()) {
            if let Ok(mut slot) = failure.lock() {
                slot.get_or_insert_with(|| line.to_string());
            }
        }
    }))
}

fn failed(failure: &Failure) -> Option<String> {
    failure.lock().ok().and_then(|slot| slot.clone())
}

/// One running ffmpeg encode.
pub struct Encoder {
    name: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    threads: Vec<JoinHandle<()>>,
    failure: Failure,
    width: u32,
    height: u32,
    pub kbps: u32,
}

impl Encoder {
    pub fn new(spec: &EncoderSpec, width: u32, height: u32, fps: u32, kbps: u32, sink: Sink<Vec<u8>>) -> Result<Encoder, String> {
        let binary = binary().ok_or("no ffmpeg")?;
        let codec = codec_of(spec.name);
        let mut cmd = command(&binary);
        cmd.args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{width}x{height}")]);
        cmd.args(["-r", &fps.max(1).to_string(), "-i", "pipe:0", "-an", "-pix_fmt", "yuv420p"]);
        cmd.args(encoder_args(spec.name, fps, kbps));
        cmd.args(["-flush_packets", "1", "-f", output_format(codec), "pipe:1"]);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|error| format!("{}: {error}", spec.name))?;
        let failure: Failure = Arc::new(Mutex::new(None));
        let mut threads = Vec::new();
        if let Some(t) = stderr_tail(&mut child, failure.clone()) {
            threads.push(t);
        }
        let mut stdout = child.stdout.take().ok_or("no encoder stdout")?;
        let reader_failure = failure.clone();
        threads.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if !sink(buf[..n].to_vec()) {
                            return;
                        }
                    }
                }
            }
            // Give stderr a moment to land before naming the exit.
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(mut slot) = reader_failure.lock() {
                slot.get_or_insert_with(|| "encoder exited".to_string());
            }
        }));
        let stdin = child.stdin.take();
        Ok(Encoder { name: spec.name, child, stdin, threads, failure, width, height, kbps })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn push(&mut self, frame: &Frame) -> Result<(), String> {
        if let Some(reason) = failed(&self.failure) {
            return Err(format!("{}: {reason}", self.name));
        }
        frame.check()?;
        let stdin = self.stdin.as_mut().ok_or("encoder input closed")?;
        stdin
            .write_all(&frame.rgb)
            .and_then(|_| stdin.flush())
            .map_err(|error| {
                let reason = failed(&self.failure).unwrap_or_else(|| error.to_string());
                format!("{}: {reason}", self.name)
            })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// One running ffmpeg decode of an elementary stream.
pub struct Decoder {
    child: Child,
    stdin: Option<ChildStdin>,
    threads: Vec<JoinHandle<()>>,
    failure: Failure,
}

impl Decoder {
    pub fn supports(codec: VideoCodec) -> bool {
        decoder_name(codec).is_some()
    }

    pub fn new(codec: VideoCodec, width: u32, height: u32, sink: Sink<Frame>) -> Result<Decoder, String> {
        let binary = binary().ok_or("no ffmpeg")?;
        let name = decoder_name(codec).ok_or("ffmpeg has no decoder for this codec")?;
        if width == 0 || height == 0 {
            return Err("decoder needs the picture size".into());
        }
        let mut cmd = command(&binary);
        // `-fflags nobuffer` makes the h264 parser drop every picture here, so it stays off.
        cmd.args(["-probesize", "32", "-analyzeduration", "0", "-flags", "low_delay"]);
        cmd.args(["-threads", "1"]);
        if cfg!(target_os = "macos") && codec != VideoCodec::Av1 {
            cmd.args(["-hwaccel", "videotoolbox"]);
        }
        cmd.args(["-c:v", name, "-f", output_format(codec), "-i", "pipe:0"]);
        cmd.args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{width}x{height}"), "pipe:1"]);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|error| format!("ffmpeg decoder: {error}"))?;
        let failure: Failure = Arc::new(Mutex::new(None));
        let mut threads = Vec::new();
        if let Some(t) = stderr_tail(&mut child, failure.clone()) {
            threads.push(t);
        }
        let mut stdout = child.stdout.take().ok_or("no decoder stdout")?;
        let reader_failure = failure.clone();
        let size = width as usize * height as usize * 3;
        threads.push(std::thread::spawn(move || {
            loop {
                let mut rgb = vec![0u8; size];
                if stdout.read_exact(&mut rgb).is_err() {
                    break;
                }
                if !sink(Frame { width, height, rgb }) {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(mut slot) = reader_failure.lock() {
                slot.get_or_insert_with(|| "decoder exited".to_string());
            }
        }));
        let stdin = child.stdin.take();
        Ok(Decoder { child, stdin, threads, failure })
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some(reason) = failed(&self.failure) {
            return Err(format!("ffmpeg decoder: {reason}"));
        }
        let stdin = self.stdin.as_mut().ok_or("decoder input closed")?;
        stdin.write_all(bytes).and_then(|_| stdin.flush()).map_err(|error| {
            let reason = failed(&self.failure).unwrap_or_else(|| error.to_string());
            format!("ffmpeg decoder: {reason}")
        })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
