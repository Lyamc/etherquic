//! H.264, H.265, AV1, OpenAPV, and JPEG XS through an `ffmpeg` child process.
//!
//! The encoder reads raw RGB frames on stdin and writes a framed bitstream on
//! stdout with every packet flushed; the host forwards those bytes as `EVI2`
//! data records. H.264/H.265/AV1 use NUT. OpenAPV uses the raw `apv` mux
//! (NUT has no APV tag). JPEG XS uses NUT with codec tag `jxs` (ffmpeg has
//! no `jpegxs` muxer, and a raw `image2pipe` stream does not frame pictures
//! for the live decoder). The player's decoder is the mirror: the
//! elementary stream in, raw RGB frames of the configured size out. Temporal
//! rows run with B-frames off and a single decode thread, so a picture is
//! not held back for reordering or frame threads.

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
        Video::OpenApv => &["liboapv"],
        Video::JpegXs => &["libsvtjpegxs"],
        Video::Pyrowave | Video::PyrowaveCpu | Video::Mjpeg | Video::Mj2k => &[],
    }
}

fn encoder_format(codec: VideoCodec) -> &'static str {
    // NUT frames each packet, so the decoder's demuxer hands a picture on as
    // soon as it arrives. A raw elementary stream waits for the next picture's
    // start code first, one frame of extra delay. APV has a raw muxer. JPEG
    // XS has no NUT tag of its own; encode sets `-tag:v jxs`.
    match codec {
        VideoCodec::OpenApv => "apv",
        _ => "nut",
    }
}

/// One raw picture on stdout. rawvideo is frame-threaded, so more than one
/// thread holds the picture until the next packet or the stream closes.
fn rawvideo_pipe(width: u32, height: u32) -> Vec<String> {
    vec![
        "-threads".into(),
        "1".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-s".into(),
        format!("{width}x{height}"),
        "pipe:1".into(),
    ]
}

fn decoder_format(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::OpenApv => "apv",
        _ => "nut",
    }
}

fn encoder_pix_fmt(name: &str) -> &'static str {
    match name {
        "liboapv" => "yuv422p10le",
        n if n.contains("jpegxs") => "yuv422p",
        _ => "yuv420p",
    }
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
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut command);
    command
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The ffmpeg this process uses: `ETHERQUIC_FFMPEG`, beside the program,
/// Homebrew on macOS (a Finder-started app has no Homebrew on `PATH`), then
/// `PATH` widened with the signed-in user's path and WinGet on Windows.
/// On Android the binary is packed as `libffmpeg.so` in `nativeLibraryDir`.
pub fn binary() -> Option<PathBuf> {
    static BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            etherquic::widen_process_path();
            let exe = if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            };
            let mut list = Vec::new();
            if let Some(path) = std::env::var_os("ETHERQUIC_FFMPEG") {
                list.push(PathBuf::from(path));
            }
            for dir in etherquic::native_library_dirs() {
                list.push(dir.join("libffmpeg.so"));
                list.push(dir.join(exe));
            }
            if let Some(dir) = std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.to_path_buf()))
            {
                list.push(dir.join("libffmpeg.so"));
                list.push(dir.join(exe));
            }
            if cfg!(target_os = "macos") {
                list.push(PathBuf::from("/opt/homebrew/bin/ffmpeg"));
                list.push(PathBuf::from("/usr/local/bin/ffmpeg"));
            }
            list.push(PathBuf::from(exe));
            list.into_iter()
                .find(|path| (!path.is_absolute() || path.is_file()) && runs(path))
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
    let Some(binary) = binary() else {
        return String::new();
    };
    let mut command = Command::new(&binary);
    command
        .args(["-hide_banner", flag])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut command);
    command
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

fn listed(text: &str, name: &str) -> bool {
    text.lines()
        .any(|line| line.split_whitespace().nth(1) == Some(name))
}

/// Frames in a long GOP when the player asked for dirty-rects. Hardware
/// encoders often clamp this; libx264 also gets `keyint=infinite`.
pub const LONG_GOP: u32 = 25_000;

/// Encoder arguments after the input. `kbps` is the target rate.
/// `long_gop` is the dirty-rects temporal path: infinite/large GOP, B-frames
/// still off, intra-refresh when that encoder documents it.
pub fn encoder_args(name: &str, fps: u32, kbps: u32, long_gop: bool) -> Vec<String> {
    let fps = fps.max(1);
    let rate = format!("{kbps}k");
    // One frame of buffer: the rate controller cannot borrow from later frames.
    let buffer = format!("{}k", (kbps / fps).max(64) * 2);
    let gop = if long_gop {
        LONG_GOP.to_string()
    } else {
        (fps * 10).to_string()
    };
    let mut args: Vec<String> = vec![
        "-c:v".into(),
        name.into(),
        "-bf".into(),
        "0".into(),
        "-g".into(),
        gop,
    ];
    let x264 = if long_gop {
        "repeat-headers=1:sliced-threads=1:keyint=infinite:intra-refresh=1"
    } else {
        "repeat-headers=1:sliced-threads=1"
    };
    let more: Vec<&str> = match name {
        "libx264" => vec![
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-x264-params",
            x264,
        ],
        n if n.ends_with("_videotoolbox") => vec!["-realtime", "1", "-prio_speed", "1"],
        n if n.ends_with("_nvenc") => {
            let mut nvenc = vec![
                "-preset",
                "p1",
                "-tune",
                "ull",
                "-zerolatency",
                "1",
                "-delay",
                "0",
                "-rc",
                "cbr",
            ];
            if long_gop {
                nvenc.extend(["-intra-refresh", "1"]);
            }
            nvenc
        }
        n if n.ends_with("_amf") => vec!["-usage", "ultralowlatency", "-rc", "cbr"],
        n if n.ends_with("_qsv") => vec![
            "-preset",
            "veryfast",
            "-look_ahead",
            "0",
            "-async_depth",
            "1",
        ],
        "liboapv" => vec!["-preset", "fastest"],
        n if n.contains("jpegxs") => vec!["-tag:v", "jxs"],
        _ => vec![],
    };
    args.extend(more.into_iter().map(String::from));
    args.extend([
        "-b:v".into(),
        rate.clone(),
        "-maxrate".into(),
        rate,
        "-bufsize".into(),
        buffer,
    ]);
    // Frame threads keep the newest picture until the next one. A still
    // desktop never sends that next picture.
    if one_thread(name) {
        args.extend(["-threads".into(), "1".into()]);
    }
    args
}

/// Hardware encoders do not use ffmpeg's frame-thread pool.
fn one_thread(name: &str) -> bool {
    !name.contains("videotoolbox")
        && !name.ends_with("_nvenc")
        && !name.ends_with("_amf")
        && !name.ends_with("_qsv")
        && !name.ends_with("_vaapi")
        && !name.ends_with("_vulkan")
        && !name.ends_with("_mf")
        && !name.ends_with("_d3d12va")
}

fn codec_of(name: &str) -> VideoCodec {
    if name.contains("265") || name.contains("hevc") {
        VideoCodec::H265
    } else if name.contains("av1") {
        VideoCodec::Av1
    } else if name.contains("oapv") || name == "apv" || name.starts_with("apv_") {
        VideoCodec::OpenApv
    } else if name.contains("jpegxs") || name.contains("jxs") {
        VideoCodec::JpegXs
    } else {
        VideoCodec::H264
    }
}

/// A short real encode, so an encoder that is listed but cannot open (no GPU,
/// no driver, wrong session) is never offered.
pub fn probe_encoder(name: &str) -> Result<(), String> {
    let binary = binary().ok_or("no ffmpeg")?;
    let mut cmd = command(&binary);
    cmd.args([
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=320x180:rate=30",
        "-frames:v",
        "8",
        "-pix_fmt",
        encoder_pix_fmt(name),
    ]);
    cmd.args(encoder_args(name, 30, 2000, false));
    cmd.args(["-f", "null", "-"]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
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
            return Err(format!(
                "{name}: {}",
                text.lines().last().unwrap_or("failed")
            ));
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
    let rows = [
        Video::Av1Hw,
        Video::H265Hw,
        Video::H264Hw,
        Video::H264Sw,
        Video::JpegXs,
        Video::OpenApv,
    ];
    let handles: Vec<_> = rows
        .into_iter()
        .map(|video| {
            let names: Vec<&'static str> = candidates(video)
                .iter()
                .copied()
                .filter(|name| listed(&list, name))
                .collect();
            std::thread::spawn(move || {
                names
                    .into_iter()
                    .find(|name| probe_encoder(name).is_ok())
                    .map(|name| EncoderSpec { video, name })
            })
        })
        .collect();
    handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect()
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
    if listed(&list, "apv") {
        out.push((VideoCodec::OpenApv, "apv"));
    } else if listed(&list, "liboapv") {
        out.push((VideoCodec::OpenApv, "liboapv"));
    }
    if listed(&list, "libsvtjpegxs") {
        out.push((VideoCodec::JpegXs, "libsvtjpegxs"));
    } else if listed(&list, "jpegxs") {
        out.push((VideoCodec::JpegXs, "jpegxs"));
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
        VideoCodec::OpenApv => 16_000,
        // JPEG XS is bits-per-pixel; ~3 bpp at 1280x720 30 fps is 80 Mbps.
        VideoCodec::JpegXs => 80_000,
        _ => 12_000,
    };
    let scale = (width as u64 * height as u64 * fps.max(1) as u64).max(1);
    let cap = match codec {
        VideoCodec::JpegXs => 400_000,
        _ => 80_000,
    };
    ((base * scale) / (1280 * 720 * 30)).clamp(1_000, cap) as u32
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
    pub fn new(
        spec: &EncoderSpec,
        width: u32,
        height: u32,
        fps: u32,
        kbps: u32,
        long_gop: bool,
        sink: Sink<Vec<u8>>,
    ) -> Result<Encoder, String> {
        let binary = binary().ok_or("no ffmpeg")?;
        let codec = codec_of(spec.name);
        let mut cmd = command(&binary);
        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &format!("{width}x{height}"),
        ]);
        cmd.args([
            "-r",
            &fps.max(1).to_string(),
            "-i",
            "pipe:0",
            "-an",
            "-pix_fmt",
            encoder_pix_fmt(spec.name),
        ]);
        cmd.args(encoder_args(spec.name, fps, kbps, long_gop));
        cmd.args(["-flush_packets", "1", "-f", encoder_format(codec), "pipe:1"]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|error| format!("{}: {error}", spec.name))?;
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
        Ok(Encoder {
            name: spec.name,
            child,
            stdin,
            threads,
            failure,
            width,
            height,
            kbps,
        })
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

    pub fn new(
        codec: VideoCodec,
        width: u32,
        height: u32,
        sink: Sink<Frame>,
    ) -> Result<Decoder, String> {
        let binary = binary().ok_or("no ffmpeg")?;
        let name = decoder_name(codec).ok_or("ffmpeg has no decoder for this codec")?;
        if width == 0 || height == 0 {
            return Err("decoder needs the picture size".into());
        }
        let mut cmd = command(&binary);
        // `-fflags nobuffer` makes the h264 parser drop every picture here, so it stays off.
        cmd.args([
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-flags",
            "low_delay",
        ]);
        cmd.args(["-threads", "1"]);
        if cfg!(target_os = "macos") && matches!(codec, VideoCodec::H264 | VideoCodec::H265) {
            cmd.args(["-hwaccel", "videotoolbox"]);
        }
        cmd.args(["-c:v", name, "-f", decoder_format(codec), "-i", "pipe:0"]);
        cmd.args(rawvideo_pipe(width, height));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|error| format!("ffmpeg decoder: {error}"))?;
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
        Ok(Decoder {
            child,
            stdin,
            threads,
            failure,
        })
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some(reason) = failed(&self.failure) {
            return Err(format!("ffmpeg decoder: {reason}"));
        }
        let stdin = self.stdin.as_mut().ok_or("decoder input closed")?;
        stdin
            .write_all(bytes)
            .and_then(|_| stdin.flush())
            .map_err(|error| {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn libx264_long_gop_keeps_no_bframes_and_infinite_keyint() {
        let short = encoder_args("libx264", 30, 2000, false);
        let text = joined(&short);
        assert!(text.contains("-bf 0"), "{text}");
        assert!(text.contains("-g 300"), "{text}");
        assert!(text.contains("repeat-headers=1:sliced-threads=1"), "{text}");
        assert!(
            !text.contains("keyint=infinite"),
            "short GOP should not request infinite keyint: {text}"
        );

        let long = encoder_args("libx264", 30, 2000, true);
        let text = joined(&long);
        assert!(text.contains("-bf 0"), "{text}");
        assert!(text.contains(&format!("-g {LONG_GOP}")), "{text}");
        assert!(text.contains("keyint=infinite"), "{text}");
        assert!(text.contains("intra-refresh=1"), "{text}");
        assert!(text.contains("-tune zerolatency"), "{text}");
    }

    #[test]
    fn nvenc_long_gop_keeps_delay_zero_and_adds_intra_refresh() {
        let long = encoder_args("h264_nvenc", 60, 4000, true);
        let text = joined(&long);
        assert!(text.contains("-bf 0"), "{text}");
        assert!(text.contains("-delay 0"), "{text}");
        assert!(text.contains(&format!("-g {LONG_GOP}")), "{text}");
        assert!(text.contains("-intra-refresh 1"), "{text}");
        let short = encoder_args("h264_nvenc", 60, 4000, false);
        assert!(joined(&short).contains("-g 600"), "{}", joined(&short));
        assert!(
            !joined(&short).contains("-intra-refresh"),
            "{}",
            joined(&short)
        );
        assert!(
            !joined(&short).contains("-threads"),
            "{}",
            joined(&short)
        );
    }

    #[test]
    fn openapv_and_jpegxs_map_to_raw_mux_and_intra_pix_fmt() {
        assert_eq!(codec_of("liboapv"), VideoCodec::OpenApv);
        assert_eq!(codec_of("apv"), VideoCodec::OpenApv);
        assert_eq!(codec_of("libsvtjpegxs"), VideoCodec::JpegXs);
        assert_eq!(encoder_format(VideoCodec::OpenApv), "apv");
        assert_eq!(decoder_format(VideoCodec::OpenApv), "apv");
        assert_eq!(encoder_format(VideoCodec::JpegXs), "nut");
        assert_eq!(decoder_format(VideoCodec::JpegXs), "nut");
        let jxs = joined(&encoder_args("libsvtjpegxs", 30, 2000, false));
        assert!(jxs.contains("-tag:v jxs"), "{jxs}");
        assert!(jxs.contains("-threads 1"), "{jxs}");
        assert_eq!(encoder_format(VideoCodec::H264), "nut");
        assert_eq!(encoder_pix_fmt("liboapv"), "yuv422p10le");
        assert_eq!(encoder_pix_fmt("libsvtjpegxs"), "yuv422p");
        assert_eq!(encoder_pix_fmt("libx264"), "yuv420p");
        let args = joined(&encoder_args("liboapv", 30, 2000, false));
        assert!(args.contains("-preset fastest"), "{args}");
        assert!(args.contains("-bf 0"), "{args}");
        assert!(args.contains("-b:v 2000k"), "{args}");
        assert!(args.contains("-threads 1"), "{args}");
        let x264 = joined(&encoder_args("libx264", 30, 2000, false));
        assert!(x264.contains("-threads 1"), "{x264}");
        let toolbox = joined(&encoder_args("h264_videotoolbox", 30, 2000, false));
        assert!(
            !toolbox.contains("-threads"),
            "hardware encode keeps its own threading: {toolbox}"
        );
        assert_eq!(candidates(Video::OpenApv), &["liboapv"]);
        assert_eq!(candidates(Video::JpegXs), &["libsvtjpegxs"]);
        let raw = joined(&rawvideo_pipe(1280, 720));
        assert!(raw.contains("-threads 1 -f rawvideo"), "{raw}");
        assert!(raw.contains("1280x720"), "{raw}");
    }

    #[test]
    fn liboapv_encodes_and_decodes_when_ffmpeg_has_it() {
        let Some(_) = binary() else {
            return;
        };
        let list = listing("-encoders");
        if !listed(&list, "liboapv") {
            return;
        }
        probe_encoder("liboapv").expect("liboapv test encode");
        assert!(
            working_decoders()
                .iter()
                .any(|(codec, _)| *codec == VideoCodec::OpenApv),
            "ffmpeg lists liboapv but no APV decoder"
        );
    }

    #[test]
    fn libsvtjpegxs_encodes_and_decodes_when_ffmpeg_has_it() {
        let Some(_) = binary() else {
            return;
        };
        let list = listing("-encoders");
        if !listed(&list, "libsvtjpegxs") {
            return;
        }
        probe_encoder("libsvtjpegxs").expect("libsvtjpegxs test encode");
        assert!(
            working_decoders()
                .iter()
                .any(|(codec, _)| *codec == VideoCodec::JpegXs),
            "ffmpeg lists libsvtjpegxs but no JPEG XS decoder"
        );
    }
}
