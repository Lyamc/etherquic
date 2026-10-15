//! Encoders and decoders for a negotiated etherquic session.
//!
//! Every codec here is real end to end: PyroWave through `libpyrowave-shared`
//! (CPU buffers in and out, Vulkan compute inside), Opus through libopus,
//! H.264, H.265 and AV1 through an `ffmpeg` child process, and MJPEG and PCM
//! in Rust. [`caps`] runs a test encode or decode before a codec is offered,
//! so nothing is advertised that cannot run on this computer.
//!
//! FFI to the two C libraries is the only `unsafe` in etherquic; it sits in
//! [`pyrowave`] and [`opus`], loaded at run time so a missing library only
//! removes that codec.

pub mod audio;
pub mod caps;
pub mod ffmpeg;
pub mod host;
pub mod jpeg;
pub mod opus;
pub mod player;
pub mod pyrowave;
pub mod video;
pub mod wire;
pub mod yuv;

pub use etherquic::negotiate;

use std::sync::Arc;

/// Packed 8-bit RGB, `width * height * 3` bytes. Both sides are even.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Frame {
    pub fn check(&self) -> Result<(), String> {
        let want = self.width as usize * self.height as usize * 3;
        if self.width == 0 || self.height == 0 || self.rgb.len() != want {
            return Err(format!(
                "frame {}x{} has {} bytes, want {want}",
                self.width,
                self.height,
                self.rgb.len()
            ));
        }
        Ok(())
    }
}

/// Interleaved 16-bit PCM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

impl Pcm {
    /// The legacy `EAUD` payload: rate u32le, channels u16le, i16le samples.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 + self.samples.len() * 2);
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        for sample in &self.samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Pcm, String> {
        if bytes.len() < 6 || (bytes.len() - 6) % 2 != 0 {
            return Err("bad pcm record".into());
        }
        let sample_rate = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let channels = u16::from_le_bytes([bytes[4], bytes[5]]);
        if channels == 0 || sample_rate == 0 {
            return Err("pcm has no channels or rate".into());
        }
        let samples = bytes[6..]
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Ok(Pcm { sample_rate, channels, samples })
    }
}

/// Where coded bytes or decoded frames go. `false` means the reader left.
pub type Sink<T> = Arc<dyn Fn(T) -> bool + Send + Sync>;

/// Status or log line.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

pub fn stderr_log(prefix: &'static str) -> Log {
    Arc::new(move |line: &str| eprintln!("{prefix}: {line}"))
}
