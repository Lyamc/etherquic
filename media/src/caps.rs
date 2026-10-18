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
    // The row says where it ran: a CPU Vulkan device is `pyrowave-cpu`.
    let encoded = pyrowave::Encoder::new(256, 144, 32 * 1024).and_then(|mut enc| {
        let record = enc.encode(&test_frame(256, 144))?;
        Ok((record, enc.vulkan_device()))
    });
    // Hardware Vulkan wins. D3D11 is only for a failed encode or a CPU device.
    // An unknown device type is counted as a GPU, same as the row above.
    let try_d3d11 = match &encoded {
        Err(_) => true,
        Ok((_, device)) => device.as_ref().is_some_and(|d| d.cpu),
    };
    match encoded {
        Ok((record, device)) => {
            let cpu = device.as_ref().is_some_and(|d| d.cpu);
            match &device {
                Some(d) => probe.notes.push(format!(
                    "pyrowave vulkan: {} ({})",
                    d.name,
                    if d.cpu { "CPU" } else { "GPU" }
                )),
                None => probe.notes.push("pyrowave vulkan: device type unknown, counted as GPU".into()),
            }
            probe.caps.video.push(EncodeCap {
                video: if cpu { Video::PyrowaveCpu } else { Video::Pyrowave },
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
    // Same bitstream, Direct3D 11 feature level 11_0. Kept behind the Vulkan
    // attempt so a real GPU is not probed twice. A failed D3D11 encode leaves
    // the CPU Vulkan row in place when that one succeeded.
    #[cfg(windows)]
    if try_d3d11 && !crate::pyrowave_d3d11::disabled() {
        // 64x64 stays inside the 32 KiB probe budget. A 256x144 high-frequency
        // frame does not: the packet walk then rejects it and the GPU row is
        // dropped even though the HD 6670 encoded it.
        let d3d11 = crate::pyrowave_d3d11::Encoder::new(64, 64, 32 * 1024).and_then(|mut enc| {
            let record = enc.encode(&test_frame(64, 64))?;
            let adapter = enc.adapter().to_string();
            crate::pyrowave_d3d11::record_acceptable(64, 64, &record)?;
            Ok(adapter)
        });
        match d3d11 {
            Ok(adapter) => {
                probe.notes.push(format!("pyrowave d3d11: {adapter}"));
                probe.caps.video.push(EncodeCap {
                    video: Video::Pyrowave,
                    encoder: "pyrowave-d3d11".into(),
                    limit: Limit { width: 7680, height: 4320, fps: 0 },
                });
                if !probe.decodes.contains(&VideoCodec::Pyrowave) && pyrowave::Decoder::new(256, 144).is_ok() {
                    probe.decodes.push(VideoCodec::Pyrowave);
                }
            }
            Err(error) => probe.notes.push(format!("pyrowave d3d11: {error}")),
        }
    }
    #[cfg(not(windows))]
    let _ = try_d3d11;
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

static PROBE: OnceLock<Probe> = OnceLock::new();

/// Probed once per process. The first call can take a second or two.
pub fn probe() -> &'static Probe {
    PROBE.get_or_init(run_probe)
}

/// The probe when it has finished, without waiting. For UI threads.
pub fn ready() -> Option<&'static Probe> {
    PROBE.get()
}

/// The host caps text for the discovery capability block. Empty until the
/// probe has finished, so an offer never waits on it.
pub fn caps_text_if_ready() -> String {
    ready().map(|probe| probe.caps.to_text()).unwrap_or_default()
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
