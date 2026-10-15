//! What this computer actually encodes and decodes, found by trying.

use std::sync::OnceLock;

use crate::negotiate::{Audio, EncodeCap, HostCaps, Limit, Video, VideoCodec};
use crate::{ffmpeg, opus, pyrowave, Frame};

/// Why a codec is missing, for logs and the Settings panel.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub caps: HostCaps,
    pub decodes: Vec<VideoCodec>,
    pub audio_decodes: Vec<Audio>,
    pub notes: Vec<String>,
}

fn test_frame(width: u32, height: u32) -> Frame {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            rgb.extend_from_slice(&[(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8]);
        }
    }
    Frame { width, height, rgb }
}

fn limit_for(name: &str) -> Limit {
    if name.contains("264") {
        Limit { width: 4096, height: 4096, fps: 0 }
    } else {
        Limit { width: 8192, height: 8192, fps: 0 }
    }
}

fn run_probe() -> Probe {
    let mut probe = Probe {
        caps: HostCaps::baseline(),
        decodes: vec![VideoCodec::Mjpeg],
        audio_decodes: Vec::new(),
        notes: Vec::new(),
    };
    // PyroWave: a real encode and decode of one small picture.
    match pyrowave::Encoder::new(256, 144, 32 * 1024).and_then(|mut enc| enc.encode(&test_frame(256, 144))) {
        Ok(record) => {
            probe.caps.video.push(EncodeCap {
                video: Video::Pyrowave,
                encoder: "pyrowave".into(),
                limit: Limit { width: 7680, height: 4320, fps: 0 },
            });
            match pyrowave::Decoder::new(256, 144).and_then(|mut dec| dec.decode(&record)) {
                Ok(_) => probe.decodes.push(VideoCodec::Pyrowave),
                Err(error) => probe.notes.push(format!("pyrowave decode: {error}")),
            }
        }
        Err(error) => {
            probe.notes.push(format!("pyrowave encode: {error}"));
            match pyrowave::Decoder::new(256, 144) {
                Ok(_) => probe.decodes.push(VideoCodec::Pyrowave),
                Err(error) => probe.notes.push(format!("pyrowave decode: {error}")),
            }
        }
    }
    match ffmpeg::binary() {
        Some(path) => {
            for spec in ffmpeg::working_encoders() {
                probe.caps.video.push(EncodeCap {
                    video: spec.video,
                    encoder: spec.name.into(),
                    limit: limit_for(spec.name),
                });
            }
            for (codec, _) in ffmpeg::working_decoders() {
                probe.decodes.push(codec);
            }
            probe.notes.push(format!("ffmpeg: {}", path.display()));
        }
        None => probe.notes.push("ffmpeg: not found".into()),
    }
    // Keep the host list in preference order.
    probe.caps.video.sort_by_key(|cap| Video::ALL.iter().position(|v| *v == cap.video));
    match opus::Encoder::new(2, opus::DEFAULT_BITRATE).and_then(|mut enc| enc.push(&[0i16; opus::FRAME * 2])) {
        Ok(_) => probe.caps.audio.insert(0, Audio::Opus),
        Err(error) => probe.notes.push(format!("opus encode: {error}")),
    }
    match opus::Decoder::new(2) {
        Ok(_) => probe.audio_decodes.push(Audio::Opus),
        Err(error) => probe.notes.push(format!("opus decode: {error}")),
    }
    probe
}

/// Probed once per process. The first call can take a second or two.
pub fn probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    PROBE.get_or_init(run_probe)
}

/// Start the probe on a background thread so a later [`probe`] is instant.
pub fn warm() {
    std::thread::spawn(|| {
        let _ = probe();
    });
}

pub fn host_caps() -> HostCaps {
    probe().caps.clone()
}

/// One line per codec, for logs.
pub fn summary(probe: &Probe) -> String {
    let enc: Vec<String> = probe.caps.video.iter().map(|c| format!("{}={}", c.video.token(), c.encoder)).collect();
    let aud: Vec<&str> = probe.caps.audio.iter().map(|a| a.token()).collect();
    let dec: Vec<String> = probe.decodes.iter().map(|c| format!("{c:?}").to_lowercase()).collect();
    let adec: Vec<&str> = probe.audio_decodes.iter().map(|a| a.token()).collect();
    format!(
        "encode video [{}] audio [{}]; decode video [{}] audio [pcm {}]; {}",
        enc.join(" "),
        aud.join(" "),
        dec.join(" "),
        adec.join(" "),
        probe.notes.join("; ")
    )
}
