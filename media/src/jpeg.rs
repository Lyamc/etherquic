//! MJPEG: every picture a JPEG. The legacy `EVID` stream and the floor of
//! every negotiation.
//!
//! Live frames go through ffmpeg's mjpeg encoder. The pure-Rust encoder
//! stays for tiles and for a machine with no ffmpeg; one core of it misses
//! 60 fps at 720p.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use jpeg_encoder::{ColorType, Encoder};
use zune_core::colorspace::ColorSpace;
use zune_jpeg::JpegDecoder;

use crate::{Frame, Sink};

pub const QUALITY: u8 = crate::negotiate::DEFAULT_QUALITY;

pub fn encode(frame: &Frame, quality: u8) -> Result<Vec<u8>, String> {
    frame.check()?;
    let width = u16::try_from(frame.width).map_err(|_| "picture too wide for JPEG")?;
    let height = u16::try_from(frame.height).map_err(|_| "picture too tall for JPEG")?;
    let mut out = Vec::new();
    Encoder::new(&mut out, quality.clamp(1, 100))
        .encode(&frame.rgb, width, height, ColorType::Rgb)
        .map_err(|error| error.to_string())?;
    Ok(out)
}

/// ffmpeg `-q:v` for a 1–100 quality. 2 is the finest mjpeg step, 31 the coarsest.
fn qscale(quality: u8) -> u8 {
    let quality = crate::negotiate::clamp_quality(quality);
    (2 + (u16::from(100 - quality) * 29) / 99) as u8
}

/// End offset of the first JPEG in `buf` (SOI through EOI), if both markers
/// are present. Stuffed `FF 00` bytes inside the scan are not markers.
pub(crate) fn jpeg_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == 0xFF && buf[i + 1] == 0xD8 {
            break;
        }
        i += 1;
    }
    if i + 1 >= buf.len() {
        return None;
    }
    let mut j = i + 2;
    while j + 1 < buf.len() {
        if buf[j] == 0xFF {
            match buf[j + 1] {
                0x00 => j += 2,
                0xD9 => return Some(j + 2),
                _ => j += 1,
            }
        } else {
            j += 1;
        }
    }
    None
}

/// One ffmpeg mjpeg process. Each `push` writes a picture; complete JPEGs
/// are handed to `sink` as they come off stdout.
pub struct Pipe {
    child: Child,
    stdin: Option<ChildStdin>,
    threads: Vec<JoinHandle<()>>,
    failure: Arc<Mutex<Option<String>>>,
    width: u32,
    height: u32,
}

impl Pipe {
    pub fn open(width: u32, height: u32, fps: u32, quality: u8, sink: Sink<Vec<u8>>) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("empty picture".into());
        }
        let binary = crate::ffmpeg::binary().ok_or("no ffmpeg")?;
        let scale = qscale(quality).to_string();
        let mut cmd = Command::new(&binary);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            &fps.max(1).to_string(),
            "-i",
            "pipe:0",
            "-an",
            "-c:v",
            "mjpeg",
            // Frame threads hold this JPEG until the next picture.
            "-threads",
            "1",
            "-q:v",
            &scale,
            "-flush_packets",
            "1",
            "-f",
            "image2pipe",
            "pipe:1",
        ]);
        hide_window(&mut cmd);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|error| format!("mjpeg: {error}"))?;
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let mut threads = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            let slot = failure.clone();
            threads.push(std::thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                if let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty()) {
                    if let Ok(mut held) = slot.lock() {
                        held.get_or_insert_with(|| line.to_string());
                    }
                }
            }));
        }
        let mut stdout = child.stdout.take().ok_or("mjpeg has no stdout")?;
        let slot = failure.clone();
        threads.push(std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 64 * 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some(end) = jpeg_end(&buf) {
                            let jpeg: Vec<u8> = buf.drain(..end).collect();
                            if !sink(jpeg) {
                                return;
                            }
                        }
                        if buf.len() > 8 * 1024 * 1024 {
                            if let Ok(mut held) = slot.lock() {
                                held.get_or_insert_with(|| "mjpeg frame was not framed".to_string());
                            }
                            return;
                        }
                    }
                }
            }
        }));
        let stdin = child.stdin.take();
        Ok(Self {
            child,
            stdin,
            threads,
            failure,
            width,
            height,
        })
    }

    pub fn push(&mut self, frame: &Frame) -> Result<(), String> {
        if let Some(reason) = self.failure.lock().ok().and_then(|slot| slot.clone()) {
            return Err(format!("mjpeg: {reason}"));
        }
        if frame.width != self.width || frame.height != self.height {
            return Err("mjpeg size changed".into());
        }
        frame.check()?;
        let stdin = self.stdin.as_mut().ok_or("mjpeg input closed")?;
        stdin.write_all(&frame.rgb).and_then(|_| stdin.flush()).map_err(|error| {
            let reason = self
                .failure
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| error.to_string());
            format!("mjpeg: {reason}")
        })
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
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

pub fn decode(bytes: &[u8]) -> Result<Frame, String> {
    let mut decoder = JpegDecoder::new(bytes);
    let pixels = decoder.decode().map_err(|error| error.to_string())?;
    let info = decoder.info().ok_or("jpeg has no header")?;
    let width = info.width as u32;
    let height = info.height as u32;
    let rgb = match decoder.get_output_colorspace() {
        Some(ColorSpace::RGB) => pixels,
        Some(ColorSpace::Luma) => pixels.into_iter().flat_map(|y| [y, y, y]).collect(),
        other => return Err(format!("unsupported jpeg colorspace {other:?}")),
    };
    let frame = Frame { width, height, rgb };
    frame.check()?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_end_splits_two_pictures() {
        let frame = Frame {
            width: 8,
            height: 8,
            rgb: vec![40; 8 * 8 * 3],
        };
        let first = encode(&frame, 40).expect("first");
        let second = encode(&frame, 80).expect("second");
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0, 1, 2]);
        buf.extend_from_slice(&first);
        buf.extend_from_slice(&second);
        let end = jpeg_end(&buf).expect("end");
        assert_eq!(&buf[..end][3..], first.as_slice());
        buf.drain(..end);
        assert_eq!(jpeg_end(&buf), Some(second.len()));
    }
}
