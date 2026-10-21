//! Global codec order, this computer's support, and the match for one host.
//!
//! The order is the default for every discovered computer. The first entry
//! that computer can run is the match. AV1 (HW) and H.265 (HW) need hardware
//! decode on that computer, so they fall through when the offer has no decode bit.
//! A connection can pin its own picture, audio, rate, and switches. That pin
//! is stored by address and does not rewrite the default order. Auto is 15 fps
//! for software encode and 30 fps for hardware. Match remote follows the host
//! display.

use std::path::{Path, PathBuf};

use etherquic::discover::{
    AnnouncedCodec, AUDIO_OPUS, AUDIO_PCM, VIDEO_APV, VIDEO_AV1, VIDEO_H264, VIDEO_H265,
    VIDEO_JPEGXS, VIDEO_MJ2K, VIDEO_MJPEG, VIDEO_PYROWAVE,
};
use etherquic::negotiate::Video;
use etherquic::{BackendProbe, MediaBackend};
use etherquic_play::etherquic_media::{caps::Probe, ffmpeg};
use etherquic_play::{
    catalog, choose_audio, choose_codec, codec_name, AudioId, AudioMachine, CodecId, EndSupport,
    MachineCodecs, PicturePref,
};

/// AV1 (HW), H.265 (HW), H.264 (HW), Pyrowave, JPEG XS, OpenAPV, MJ2K, MJPEG, H.264 (SW).
pub fn default_video() -> Vec<PicturePref> {
    PicturePref::order().to_vec()
}

/// Opus, then AAC, then PCM. PCM is what the desktop session sends.
pub fn default_audio() -> Vec<AudioId> {
    AudioId::order().to_vec()
}

/// Name shown for an Opus tool in the preference order.
pub fn backend_label(backend: MediaBackend) -> &'static str {
    match backend {
        MediaBackend::Libopus => "Builtin",
        MediaBackend::Ffmpeg => "FFmpeg",
        MediaBackend::Gstreamer => "GStreamer",
    }
}

/// What the row says under the tool name.
pub fn backend_hint(backend: MediaBackend, present: bool, in_use: bool) -> String {
    let tool = match backend {
        MediaBackend::Libopus => "libopus",
        MediaBackend::Ffmpeg => "ffmpeg",
        MediaBackend::Gstreamer => "GStreamer",
    };
    let state = if present { "found" } else { "not found" };
    if in_use {
        format!("{tool}, {state}, in use")
    } else {
        format!("{tool}, {state}")
    }
}

pub fn swap_earlier<T>(items: &mut [T], index: usize) {
    if index > 0 && index < items.len() {
        items.swap(index, index - 1);
    }
}

pub fn swap_later<T>(items: &mut [T], index: usize) {
    if index + 1 < items.len() {
        items.swap(index, index + 1);
    }
}

pub fn video_hint(pref: PicturePref) -> &'static str {
    match pref {
        PicturePref::Pyrowave => {
            "Hardware encode on a GPU (Vulkan, Direct3D 11, or Metal). The player decodes with Vulkan or Metal"
        }
        PicturePref::PyrowaveCpu => {
            "PyroWave on a CPU Vulkan driver (no GPU). Uses many cores and about 40 Mbit/s"
        }
        PicturePref::JpegXs => {
            "intra JPEG XS through ffmpeg, visually lossless, no encode hardware"
        }
        PicturePref::OpenApv => "intra APV through ffmpeg liboapv, no encode hardware",
        PicturePref::Mj2k => {
            "intra JPEG 2000, stronger than MJPEG, no encode hardware"
        }
        PicturePref::Mjpeg => "intra, weak compression, no encode hardware",
        PicturePref::H264Hw => "compresses well. Hardware encode on the side that sends",
        PicturePref::H264Sw => "CPU encode, only when that CPU can sustain it",
        PicturePref::H265Hw => {
            "compresses better than H.264. Hardware encode, and hardware decode on the remote"
        }
        PicturePref::Av1Hw => "compresses best. Hardware encode, and hardware decode on the remote",
    }
}

pub fn audio_hint(codec: AudioId) -> &'static str {
    match codec {
        AudioId::Pcm => "uncompressed. The desktop session sends this",
        AudioId::Opus => "compresses on the CPU when the selected tool exists on both ends",
        AudioId::Aac => "compresses on the CPU. Used only when both computers have AAC",
    }
}

fn video_wire(codec: CodecId) -> u8 {
    match codec {
        CodecId::Pyrowave => VIDEO_PYROWAVE,
        CodecId::Mj2k => VIDEO_MJ2K,
        CodecId::JpegXs => VIDEO_JPEGXS,
        CodecId::OpenApv => VIDEO_APV,
        CodecId::Mjpeg => VIDEO_MJPEG,
        CodecId::H264 => VIDEO_H264,
        CodecId::H265 => VIDEO_H265,
        CodecId::Av1 => VIDEO_AV1,
    }
}

fn codec_from_wire(id: u8) -> Option<CodecId> {
    match id {
        VIDEO_PYROWAVE => Some(CodecId::Pyrowave),
        VIDEO_MJ2K => Some(CodecId::Mj2k),
        VIDEO_JPEGXS => Some(CodecId::JpegXs),
        VIDEO_APV => Some(CodecId::OpenApv),
        VIDEO_MJPEG => Some(CodecId::Mjpeg),
        VIDEO_H264 => Some(CodecId::H264),
        VIDEO_H265 => Some(CodecId::H265),
        VIDEO_AV1 => Some(CodecId::Av1),
        _ => None,
    }
}

pub fn video_announce(machine: &MachineCodecs) -> Vec<AnnouncedCodec> {
    catalog()
        .into_iter()
        .map(|row| {
            let end = machine.get(row.id);
            AnnouncedCodec {
                id: video_wire(row.id),
                hardware_encode: end.hardware_encode,
                hardware_decode: end.hardware_decode,
                cpu: end.cpu_can_sustain,
                gpu: end.gpu_compute,
                libopus: false,
                ffmpeg: false,
                gstreamer: false,
            }
        })
        .collect()
}

pub fn audio_announce(machine: &AudioMachine) -> Vec<AnnouncedCodec> {
    vec![
        AnnouncedCodec {
            id: AUDIO_PCM,
            hardware_encode: false,
            hardware_decode: false,
            cpu: machine.pcm,
            gpu: false,
            libopus: false,
            ffmpeg: false,
            gstreamer: false,
        },
        AnnouncedCodec {
            id: AUDIO_OPUS,
            hardware_encode: false,
            hardware_decode: false,
            cpu: machine.opus,
            gpu: false,
            libopus: false,
            ffmpeg: false,
            gstreamer: false,
        },
    ]
}

pub fn remote_video(announced: Option<&[AnnouncedCodec]>) -> MachineCodecs {
    let Some(rows) = announced else {
        return MachineCodecs::default();
    };
    let ends = rows
        .iter()
        .filter_map(|row| {
            let id = codec_from_wire(row.id)?;
            Some((
                id,
                EndSupport {
                    hardware_encode: row.hardware_encode,
                    hardware_decode: row.hardware_decode,
                    cpu_can_sustain: row.cpu,
                    gpu_compute: row.gpu,
                },
            ))
        })
        .collect();
    MachineCodecs::from_rows(ends)
}

/// Opus follows the selected tool. Detection of the other tool stays visible.
pub fn audio_for(probe: BackendProbe, selected: MediaBackend) -> AudioMachine {
    AudioMachine {
        pcm: true,
        opus: probe.has(selected),
        aac: false,
    }
}

pub fn backend_line(probe: BackendProbe, selected: MediaBackend) -> String {
    let tools = [
        (probe.libopus, "libopus"),
        (probe.ffmpeg, "ffmpeg"),
        (probe.gstreamer, "GStreamer"),
    ];
    let found: Vec<&str> = tools
        .iter()
        .filter(|tool| tool.0)
        .map(|tool| tool.1)
        .collect();
    let missing: Vec<&str> = tools
        .iter()
        .filter(|tool| !tool.0)
        .map(|tool| tool.1)
        .collect();
    let found = if found.is_empty() {
        "Detected: none.".to_string()
    } else {
        format!("Detected: {}.", found.join(", "))
    };
    let missing = if missing.is_empty() {
        String::new()
    } else {
        format!(" {} not found.", missing.join(", "))
    };
    format!("{found}{missing} Using {}.", backend_label(selected))
}

/// Tools the remote named on its Opus row. A version-1 offer names nothing.
pub fn remote_backend_line(audio: Option<&[AnnouncedCodec]>) -> String {
    let Some(rows) = audio else {
        return "Remote audio tools: not announced".into();
    };
    let Some(opus) = rows.iter().find(|row| row.id == AUDIO_OPUS) else {
        return "Remote audio tools: not announced".into();
    };
    let mut tools = Vec::new();
    if opus.libopus {
        tools.push("libopus");
    }
    if opus.ffmpeg {
        tools.push("ffmpeg");
    }
    if opus.gstreamer {
        tools.push("GStreamer");
    }
    if tools.is_empty() {
        if opus.cpu {
            "Remote audio tools: Opus, tool not named".into()
        } else {
            "Remote audio tools: none".into()
        }
    } else {
        format!("Remote audio tools: {}", tools.join(", "))
    }
}

/// PCM stays available when a host does not mention audio. Opus does not.
pub fn remote_audio(announced: Option<&[AnnouncedCodec]>) -> AudioMachine {
    let opus = announced
        .map(|rows| rows.iter().any(|row| row.id == AUDIO_OPUS && row.cpu))
        .unwrap_or(false);
    AudioMachine {
        pcm: true,
        opus,
        aac: false,
    }
}

fn support_words(end: &EndSupport) -> &'static str {
    match (
        end.hardware_encode,
        end.hardware_decode,
        end.gpu_compute,
        end.cpu_can_sustain,
    ) {
        (true, true, _, _) => "encode+decode",
        (true, false, true, _) => "HW encode",
        (true, false, _, _) => "encode, no decode",
        (false, true, _, _) => "decode, no encode",
        (false, false, true, _) => "GPU",
        (false, false, false, true) => "CPU",
        (false, false, false, false) => "no hardware",
    }
}

pub fn local_video_line(machine: &MachineCodecs) -> String {
    catalog()
        .into_iter()
        .map(|row| format!("{} {}", row.name, support_words(&machine.get(row.id))))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// One row of the this-computer codec table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecSupportRow {
    pub id: CodecId,
    pub name: &'static str,
    pub software: bool,
    pub hardware: bool,
    pub backend: String,
    pub error: Option<String>,
    pub latency: &'static str,
    pub psnr_per_byte: &'static str,
    pub i_frame: bool,
    pub p_frame: bool,
    pub b_frame: bool,
}

impl CodecSupportRow {
    pub fn software_label(&self) -> &'static str {
        if self.software {
            "yes"
        } else {
            "no"
        }
    }

    pub fn hardware_label(&self) -> &'static str {
        if self.hardware {
            "yes"
        } else {
            "no"
        }
    }

    pub fn frame_label(on: bool) -> &'static str {
        if on {
            "yes"
        } else {
            "—"
        }
    }
}

fn videos_for(id: CodecId) -> &'static [Video] {
    match id {
        CodecId::Pyrowave => &[Video::Pyrowave, Video::PyrowaveCpu],
        CodecId::Mjpeg => &[Video::Mjpeg],
        CodecId::Mj2k => &[Video::Mj2k],
        CodecId::JpegXs => &[Video::JpegXs],
        CodecId::OpenApv => &[Video::OpenApv],
        CodecId::H264 => &[Video::H264Hw, Video::H264Sw],
        CodecId::H265 => &[Video::H265Hw],
        CodecId::Av1 => &[Video::Av1Hw],
    }
}

fn needs_ffmpeg(id: CodecId) -> bool {
    matches!(
        id,
        CodecId::JpegXs | CodecId::OpenApv | CodecId::H264 | CodecId::H265 | CodecId::Av1
    )
}

fn note_matches(id: CodecId, note: &str) -> bool {
    let n = note.to_ascii_lowercase();
    match id {
        CodecId::Pyrowave => n.contains("pyrowave"),
        CodecId::Mjpeg => n.contains("mjpeg"),
        CodecId::Mj2k => n.contains("mj2k") || n.contains("jpeg2000") || n.contains("jpeg 2000"),
        CodecId::JpegXs => {
            n.contains("jpegxs")
                || n.contains("jpeg xs")
                || n.contains("svtjpegxs")
                || n.contains("libsvtjpegxs")
        }
        CodecId::OpenApv => n.contains("openapv") || n.contains("liboapv") || n.contains("oapv"),
        CodecId::H264 => {
            n.contains("h264")
                || n.contains("h.264")
                || n.contains("libx264")
                || n.contains("264_")
        }
        CodecId::H265 => n.contains("h265") || n.contains("h.265") || n.contains("hevc"),
        CodecId::Av1 => n.contains("av1"),
    }
}

fn is_error_note(note: &str) -> bool {
    let n = note.to_ascii_lowercase();
    n.contains("not found")
        || n.contains("encode:")
        || n.contains("decode:")
        || n.starts_with("mj2k:")
        || n.contains("failed")
        || n.contains("error")
        || n.contains("disabled")
}

fn push_error(slot: &mut Option<String>, msg: String) {
    if msg.is_empty() {
        return;
    }
    match slot {
        Some(existing) if existing.contains(&msg) => {}
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&msg);
        }
        None => *slot = Some(msg),
    }
}

/// SW/HW rows for this computer, with probe backends and library errors.
pub fn local_codec_rows(
    machine: &MachineCodecs,
    probe: Option<&Probe>,
    pyrowave_library: bool,
) -> Vec<CodecSupportRow> {
    let ffmpeg_missing = probe
        .map(|probe| {
            probe
                .notes
                .iter()
                .any(|note| note.eq_ignore_ascii_case("ffmpeg: not found"))
        })
        .unwrap_or(false);
    catalog()
        .into_iter()
        .map(|info| {
            let end = machine.get(info.id);
            let caps: Vec<_> = probe
                .map(|probe| {
                    probe
                        .caps
                        .video
                        .iter()
                        .filter(|cap| videos_for(info.id).contains(&cap.video))
                        .collect()
                })
                .unwrap_or_default();
            let (software, hardware, backend) = if probe.is_some() {
                let software = caps.iter().any(|cap| !cap.video.hardware());
                let hardware = caps.iter().any(|cap| cap.video.hardware());
                let backend = if caps.is_empty() {
                    "—".into()
                } else {
                    let mut names: Vec<String> = caps.iter().map(|cap| cap.encoder.clone()).collect();
                    names.sort();
                    names.dedup();
                    names.join(", ")
                };
                (software, hardware, backend)
            } else {
                let hardware = end.hardware_encode;
                let software = end.cpu_can_sustain
                    || (info.id == CodecId::Pyrowave && end.gpu_compute && !end.hardware_encode);
                (software, hardware, "…".into())
            };
            let mut error = None;
            if info.id == CodecId::Pyrowave && !pyrowave_library {
                push_error(&mut error, "Pyrowave library: not found".into());
            }
            if let Some(probe) = probe {
                for note in &probe.notes {
                    if is_error_note(note) && note_matches(info.id, note) {
                        push_error(&mut error, note.clone());
                    }
                }
                if ffmpeg_missing && needs_ffmpeg(info.id) {
                    push_error(&mut error, "ffmpeg: not found".into());
                } else if !ffmpeg_missing && needs_ffmpeg(info.id) && caps.is_empty() {
                    let wanted: Vec<&str> = videos_for(info.id)
                        .iter()
                        .copied()
                        .flat_map(ffmpeg::candidates)
                        .copied()
                        .collect();
                    if !wanted.is_empty() {
                        push_error(
                            &mut error,
                            format!("ffmpeg encoder {} not found", wanted.join("/")),
                        );
                    }
                }
            }
            CodecSupportRow {
                id: info.id,
                name: info.name,
                software,
                hardware,
                backend,
                error,
                latency: info.display_to_frame,
                psnr_per_byte: info.psnr_per_byte,
                i_frame: info.i_frame,
                p_frame: info.p_frame,
                b_frame: info.b_frame,
            }
        })
        .collect()
}

pub fn codec_table_text(rows: &[CodecSupportRow]) -> String {
    let mut lines = vec![
        "codec\tSW\tHW\tbackend\terror\tlatency\tPSNR/B\tI\tP\tB".to_string(),
    ];
    for row in rows {
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.name,
            row.software_label(),
            row.hardware_label(),
            row.backend,
            row.error.as_deref().unwrap_or(""),
            row.latency,
            row.psnr_per_byte,
            CodecSupportRow::frame_label(row.i_frame),
            CodecSupportRow::frame_label(row.p_frame),
            CodecSupportRow::frame_label(row.b_frame),
        ));
    }
    lines.join("\n")
}

pub fn remote_hardware_line(video: Option<&[AnnouncedCodec]>) -> String {
    let Some(rows) = video else {
        return "Codec hardware: not announced".into();
    };
    if rows.is_empty() {
        return "Codec hardware: none".into();
    }
    let parts: Vec<String> = rows
        .iter()
        .map(|row| {
            let name = codec_from_wire(row.id).map(codec_name).unwrap_or("codec");
            let end = EndSupport {
                hardware_encode: row.hardware_encode,
                hardware_decode: row.hardware_decode,
                cpu_can_sustain: row.cpu,
                gpu_compute: row.gpu,
            };
            format!("{name} {}", support_words(&end))
        })
        .collect();
    format!("Codec hardware: {}", parts.join(" · "))
}

/// Match against a discovered host. That host encodes, this PC decodes.
pub fn video_match_line(
    preference: &[PicturePref],
    local: &MachineCodecs,
    video: Option<&[AnnouncedCodec]>,
    skip_without_hardware_encode: bool,
) -> String {
    let remote = remote_video(video);
    let (chosen, skipped) = choose_codec(
        preference,
        local,
        &remote,
        false,
        skip_without_hardware_encode,
    );
    let mut line = match chosen {
        Some(pref) => format!("Match: {}", pref.name()),
        None => "Match: none of the preferences fit this computer".into(),
    };
    for skip in skipped {
        line.push_str(&format!(". {} skipped: {}", skip.pref.name(), skip.reason));
    }
    line
}

/// The picture codec this order gives with that computer, or `None`.
pub fn chosen_video(
    preference: &[PicturePref],
    local: &MachineCodecs,
    video: Option<&[AnnouncedCodec]>,
    skip_without_hardware_encode: bool,
) -> Option<PicturePref> {
    choose_codec(
        preference,
        local,
        &remote_video(video),
        false,
        skip_without_hardware_encode,
    )
    .0
}

/// The audio codec this order gives with that computer, or `None`.
pub fn chosen_audio(
    preference: &[AudioId],
    local: &AudioMachine,
    audio: Option<&[AnnouncedCodec]>,
) -> Option<AudioId> {
    choose_audio(preference, local, &remote_audio(audio)).0
}

/// What a session with this host will use: this computer's offer against the
/// encoders the host put in its capability block. A host that announced none
/// does not negotiate, so it sends MJPEG and PCM. `None` until this computer's
/// own codec probe has finished; the probe runs in the background.
pub fn predicted_pair(
    choice: &etherquic_play::CodecChoice,
    caps: Option<&etherquic::discover::Capabilities>,
) -> Option<Predicted> {
    etherquic_play::etherquic_media::caps::ready()?;
    let host = caps.and_then(|caps| etherquic::negotiate::HostCaps::from_text(&caps.codecs));
    let (video, audio) = etherquic::negotiate::predict(&choice.offer(), host.as_ref());
    Some(Predicted {
        video: PicturePref::from_token(video.token())?,
        audio: AudioId::from_token(audio.token())?,
        negotiates: host.is_some(),
    })
}

/// The pair [`predicted_pair`] expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Predicted {
    pub video: PicturePref,
    pub audio: AudioId,
    /// False for a host that does not negotiate codecs (MJPEG and PCM).
    pub negotiates: bool,
}

/// The offer for one connection: the applied order with any pin first.
pub fn codec_choice_for(
    applied: &Applied,
    video_pin: Option<PicturePref>,
    audio_pin: Option<AudioId>,
    quality: u8,
    dirty_rects: bool,
    yuv: etherquic::negotiate::YuvFormat,
    view_only: bool,
    exclusive: bool,
    carrier: Option<u8>,
) -> etherquic_play::CodecChoice {
    let mut video = applied.video.clone();
    if let Some(pin) = video_pin {
        move_to_front(&mut video, pin);
    }
    let mut audio = applied.audio.clone();
    if let Some(pin) = audio_pin {
        move_to_front(&mut audio, pin);
    }
    etherquic_play::CodecChoice {
        video,
        audio,
        video_pin,
        audio_pin,
        max_width: 0,
        max_height: 0,
        bitrate_kbps: 0,
        quality,
        dirty_rects,
        yuv,
        fps: applied.fps.wish(),
        audio_bitrate_kbps: applied.audio_bitrate_kbps,
        audio_channels: applied.audio_channels,
        view_only,
        exclusive,
        carrier,
    }
}

/// "PyroWave 1280x720 (pyrowave) · Opus 128 kbit/s" out of a "Viewing …" status.
pub fn negotiated_from_status(text: &str) -> Option<String> {
    let rest = text.strip_prefix("Viewing ")?;
    let parts: Vec<&str> = rest.split(" · ").collect();
    if parts.len() < 3 {
        return None;
    }
    let audio = parts[2].trim_end_matches('.');
    let audio = audio.split(" (the host").next().unwrap_or(audio);
    if parts[1].ends_with("starting") {
        return None;
    }
    Some(format!("{} · {}", parts[1], audio))
}

/// Move `item` to the front of `order`, adding it when missing.
pub fn move_to_front<T: PartialEq + Copy>(order: &mut Vec<T>, item: T) {
    order.retain(|entry| *entry != item);
    order.insert(0, item);
}

pub fn audio_match_line(
    preference: &[AudioId],
    local: &AudioMachine,
    audio: Option<&[AnnouncedCodec]>,
) -> String {
    let remote = remote_audio(audio);
    let (chosen, skipped) = choose_audio(preference, local, &remote);
    let mut line = match chosen {
        Some(codec) => format!("Audio match: {}", codec.name()),
        None => "Audio match: none".into(),
    };
    for skip in skipped {
        line.push_str(&format!(". {} skipped: {}", skip.codec.name(), skip.reason));
    }
    line
}

/// Pictures per second a sender without a hardware encoder is held to.
pub const SOFTWARE_FPS: u32 = etherquic::negotiate::SOFTWARE_FPS;
/// Pictures per second Auto uses with a hardware encoder.
pub const HARDWARE_FPS: u32 = etherquic::negotiate::HARDWARE_FPS;

/// Rates the window offers beside Auto and Match remote.
pub const FPS_CHOICES: [u32; 4] = [15, 30, 60, 120];

/// Stereo-equivalent audio bitrates the Connect row offers. Displayed values
/// scale with the selected channel layout.
pub const AUDIO_BITRATE_CHOICES: [u32; 6] = [48, 64, 96, 128, 160, 192];

/// Channel layouts next to the audio dropdown. Auto is 2-channel stereo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioLayout {
    pub channels: u16,
    pub name: &'static str,
}

pub const AUDIO_LAYOUTS: [AudioLayout; 6] = [
    AudioLayout {
        channels: 0,
        name: "Auto",
    },
    AudioLayout {
        channels: 1,
        name: "Mono",
    },
    AudioLayout {
        channels: 2,
        name: "Stereo",
    },
    AudioLayout {
        channels: 3,
        name: "2.1",
    },
    AudioLayout {
        channels: 6,
        name: "5.1",
    },
    AudioLayout {
        channels: 8,
        name: "7.1",
    },
];

pub fn audio_layout_name(channels: u16) -> &'static str {
    AUDIO_LAYOUTS
        .iter()
        .find(|layout| layout.channels == channels)
        .map(|layout| layout.name)
        .unwrap_or("Auto")
}

pub fn scale_audio_kbps(stereo_kbps: u32, channels: u16) -> u32 {
    etherquic::negotiate::scale_audio_kbps(stereo_kbps, channels)
}

/// Auto is 15/30. Match remote follows the host display. A fixed rate is the number picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRate {
    Auto,
    Match,
    Fixed(u32),
}

impl FrameRate {
    pub fn token(self) -> String {
        self.wish().token()
    }

    pub fn from_token(token: &str) -> Option<Self> {
        let token = token.trim();
        if token.eq_ignore_ascii_case("auto") {
            return Some(Self::Auto);
        }
        if token.eq_ignore_ascii_case("match")
            || token.eq_ignore_ascii_case("remote")
            || token.eq_ignore_ascii_case("display")
        {
            return Some(Self::Match);
        }
        let fps: u32 = token.parse().ok()?;
        (1..=etherquic::negotiate::MAX_FPS)
            .contains(&fps)
            .then_some(Self::Fixed(fps))
    }

    pub fn wish(self) -> etherquic::negotiate::FpsWish {
        match self {
            Self::Auto => etherquic::negotiate::FpsWish::Auto,
            Self::Match => etherquic::negotiate::FpsWish::Match,
            Self::Fixed(fps) => etherquic::negotiate::FpsWish::Fixed(fps),
        }
    }
}

fn clamp_fps(fps: u32) -> u32 {
    etherquic::negotiate::clamp_fps(fps)
}

/// Hardware-encode rows. GPU PyroWave counts. CPU PyroWave, MJPEG, and
/// H.264 (SW) do not.
pub fn sender_hardware_encode(pref: Option<PicturePref>) -> bool {
    matches!(
        pref,
        Some(
            PicturePref::Av1Hw | PicturePref::H265Hw | PicturePref::H264Hw | PicturePref::Pyrowave
        )
    )
}

/// Auto is 15 fps software / 30 fps hardware. Match remote uses `server_fps`
/// (the host display). An explicit rate is the number the user picked.
pub fn resolve_fps(choice: FrameRate, server_fps: Option<u32>, hardware_encode: bool) -> u32 {
    choice.wish().resolve(hardware_encode, server_fps)
}

/// Short rate for a connection headline.
pub fn fps_headline(choice: FrameRate, server_fps: Option<u32>, hardware_encode: bool) -> String {
    match choice {
        FrameRate::Fixed(fps) => format!("{} fps", clamp_fps(fps)),
        FrameRate::Match => match server_fps.map(clamp_fps) {
            Some(fps) => format!("{fps} fps"),
            None => "Match remote".to_string(),
        },
        FrameRate::Auto => format!(
            "{} fps",
            resolve_fps(choice, server_fps, hardware_encode)
        ),
    }
}

/// Rate this PC would encode at. Capture itself follows the display refresh.
pub fn capture_rate(
    video: &[PicturePref],
    local: &MachineCodecs,
    skip_without_hardware_encode: bool,
    fps: FrameRate,
    display_hz: Option<u32>,
) -> u32 {
    let (picture, _) = choose_codec(video, local, local, true, skip_without_hardware_encode);
    resolve_fps(fps, display_hz, sender_hardware_encode(picture))
}

/// The codec pair and rate shown on a connection.
pub fn top_selection(
    video_pref: &[PicturePref],
    audio_pref: &[AudioId],
    local_video: &MachineCodecs,
    local_audio: &AudioMachine,
    video: Option<&[AnnouncedCodec]>,
    audio: Option<&[AnnouncedCodec]>,
    skip_without_hardware_encode: bool,
    fps: FrameRate,
    server_fps: Option<u32>,
) -> String {
    let (picture, _) = choose_codec(
        video_pref,
        local_video,
        &remote_video(video),
        false,
        skip_without_hardware_encode,
    );
    let (sound, _) = choose_audio(audio_pref, local_audio, &remote_audio(audio));
    format!(
        "{} · {} · {}",
        picture.map(PicturePref::name).unwrap_or("none"),
        sound.map(AudioId::name).unwrap_or("none"),
        fps_headline(fps, server_fps, sender_hardware_encode(picture)),
    )
}

/// Top of the preference lists, before a host has announced anything.
pub fn listed_top(video: &[PicturePref], audio: &[AudioId], fps: FrameRate) -> String {
    format!(
        "{} · {} · {}",
        video
            .first()
            .copied()
            .map(PicturePref::name)
            .unwrap_or("none"),
        audio.first().copied().map(AudioId::name).unwrap_or("none"),
        fps_headline(fps, None, sender_hardware_encode(video.first().copied())),
    )
}

/// The preference panel, in the order the window last saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub video: Vec<PicturePref>,
    /// Rows left out of offers unless pinned.
    pub video_off: Vec<PicturePref>,
    pub audio: Vec<AudioId>,
    pub opus: Vec<MediaBackend>,
    pub skip_without_hw: bool,
    pub trace_input: bool,
    /// Default rate for every connection, and the rate this PC captures at.
    pub fps: FrameRate,
    /// Stereo-equivalent audio bitrate in kbit/s. Zero is Auto (96 stereo).
    pub audio_bitrate_kbps: u32,
    /// Max audio channels. Zero is Auto (2-channel stereo).
    pub audio_channels: u16,
}

impl Preferences {
    pub fn baseline() -> Self {
        Self {
            video: default_video(),
            video_off: Vec::new(),
            audio: default_audio(),
            opus: etherquic::default_backend_order().to_vec(),
            skip_without_hw: false,
            trace_input: false,
            fps: FrameRate::Auto,
            audio_bitrate_kbps: 0,
            audio_channels: 0,
        }
    }
}

/// `XDG_CONFIG_HOME/etherquic/preferences.txt`, or `~/.config`, or `%APPDATA%`.
pub fn preferences_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("etherquic").join("preferences.txt"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return Some(
                PathBuf::from(home)
                    .join(".config")
                    .join("etherquic")
                    .join("preferences.txt"),
            );
        }
    }
    if let Some(dir) = std::env::var_os("APPDATA") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("etherquic").join("preferences.txt"));
        }
    }
    None
}

/// Read the saved panel. A missing file, or a row this build does not know,
/// falls back to the default and keeps every current row.
pub fn load_preferences() -> Preferences {
    let Some(path) = preferences_path() else {
        return Preferences::baseline();
    };
    match std::fs::read_to_string(path) {
        Ok(text) => parse_preferences(&text),
        Err(_) => Preferences::baseline(),
    }
}

pub fn store_preferences(prefs: &Preferences) -> Result<(), String> {
    let path = preferences_path().ok_or("no home directory for preferences")?;
    store_preferences_at(&path, prefs)
}

fn store_preferences_at(path: &Path, prefs: &Preferences) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let body = render_preferences(prefs);
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, &body).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("save {}: {err}", path.display()))?;
    Ok(())
}

fn render_preferences(prefs: &Preferences) -> String {
    format!(
        "video={}\nvideo-off={}\naudio={}\nopus={}\nskip-without-hw={}\ntrace-input={}\nfps={}\naudio-bitrate={}\naudio-channels={}\n",
        join_tokens(prefs.video.iter().map(|pref| pref.token())),
        join_tokens(prefs.video_off.iter().map(|pref| pref.token())),
        join_tokens(prefs.audio.iter().map(|codec| codec.token())),
        join_tokens(prefs.opus.iter().copied().map(backend_token)),
        yes_no(prefs.skip_without_hw),
        yes_no(prefs.trace_input),
        prefs.fps.token(),
        if prefs.audio_bitrate_kbps == 0 {
            "auto".to_string()
        } else {
            prefs.audio_bitrate_kbps.to_string()
        },
        if prefs.audio_channels == 0 {
            "auto".to_string()
        } else {
            prefs.audio_channels.to_string()
        },
    )
}

fn parse_preferences(text: &str) -> Preferences {
    let mut video = None;
    let mut video_off = Vec::new();
    let mut audio = None;
    let mut opus = None;
    let mut skip_without_hw = false;
    let mut trace_input = false;
    let mut fps = FrameRate::Auto;
    let mut audio_bitrate_kbps = 0;
    let mut audio_channels = 0;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "video" => video = Some(parse_list(value, PicturePref::from_token)),
            "video-off" => video_off = parse_list(value, PicturePref::from_token),
            "audio" => audio = Some(parse_list(value, AudioId::from_token)),
            "opus" => opus = Some(parse_list(value, backend_from_token)),
            "skip-without-hw" => skip_without_hw = parse_yes(value),
            "trace-input" => trace_input = parse_yes(value),
            "fps" => {
                if let Some(rate) = FrameRate::from_token(value) {
                    fps = rate;
                }
            }
            "audio-bitrate" => {
                audio_bitrate_kbps = parse_audio_bitrate(value);
            }
            "audio-channels" => {
                audio_channels = parse_audio_channels(value);
            }
            _ => {}
        }
    }
    Preferences {
        video: fill(video.unwrap_or_default(), &PicturePref::order()),
        video_off,
        audio: fill(audio.unwrap_or_default(), &AudioId::order()),
        opus: fill(
            opus.unwrap_or_default(),
            &etherquic::default_backend_order(),
        ),
        skip_without_hw,
        trace_input,
        fps,
        audio_bitrate_kbps,
        audio_channels,
    }
}

fn parse_audio_bitrate(value: &str) -> u32 {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        0
    } else {
        value.parse().unwrap_or(0)
    }
}

fn parse_audio_channels(value: &str) -> u16 {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        0
    } else {
        value
            .parse::<u16>()
            .ok()
            .map(|n| n.min(etherquic::negotiate::AUDIO_CHANNELS_MAX))
            .unwrap_or(0)
    }
}

fn join_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for token in tokens {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(token);
    }
    out
}

fn parse_list<T: Copy>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Vec<T> {
    value
        .split(',')
        .filter_map(|token| parse(token.trim()))
        .collect()
}

fn fill<T: Copy + PartialEq>(saved: Vec<T>, full: &[T]) -> Vec<T> {
    let mut out = Vec::new();
    for item in saved {
        if full.contains(&item) && !out.contains(&item) {
            out.push(item);
        }
    }
    for item in full {
        if !out.contains(item) {
            out.push(*item);
        }
    }
    out
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn parse_yes(value: &str) -> bool {
    matches!(value.trim(), "yes" | "true" | "1")
}

fn backend_token(backend: MediaBackend) -> &'static str {
    match backend {
        MediaBackend::Libopus => "libopus",
        MediaBackend::Ffmpeg => "ffmpeg",
        MediaBackend::Gstreamer => "gstreamer",
    }
}

fn backend_from_token(token: &str) -> Option<MediaBackend> {
    match token {
        "libopus" => Some(MediaBackend::Libopus),
        "ffmpeg" => Some(MediaBackend::Ffmpeg),
        "gstreamer" => Some(MediaBackend::Gstreamer),
        _ => None,
    }
}

/// 12 lowercase hex digits. Colons are ignored.
pub fn mac_key(mac: [u8; 6]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn normalize_mac(text: &str) -> Option<String> {
    let hex: String = text.chars().filter(|ch| *ch != ':' && *ch != '-').collect();
    if hex.len() != 12 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex.to_ascii_lowercase())
}

/// Fields set here replace the global default for one computer.
///
/// `None` means "use my default". A picture or audio pin is tried first, and
/// the rest of the default order still applies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionOverride {
    pub video: Option<PicturePref>,
    pub audio: Option<AudioId>,
    pub fps: Option<FrameRate>,
    pub skip_without_hw: Option<bool>,
    pub trace_input: Option<bool>,
    /// Carrier for the next session with this host. `None` keeps QUIC streams
    /// and leaves the offer line off.
    pub carrier: Option<u8>,
    /// `plain` or `gcm`. Unset keeps UDP/RTP on `gcm` and ST 2110 on `plain`.
    pub profile: Option<String>,
}

impl ConnectionOverride {
    pub fn is_empty(&self) -> bool {
        self.video.is_none()
            && self.audio.is_none()
            && self.fps.is_none()
            && self.skip_without_hw.is_none()
            && self.trace_input.is_none()
            && self.carrier.is_none()
            && self.profile.is_none()
    }
}

/// Global defaults after one computer's pin is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub video: Vec<PicturePref>,
    pub audio: Vec<AudioId>,
    pub fps: FrameRate,
    pub skip_without_hw: bool,
    pub trace_input: bool,
    pub audio_bitrate_kbps: u32,
    pub audio_channels: u16,
    /// Pinned carrier, or QUIC streams when this host has no pin.
    pub carrier: u8,
    /// `true` when at least one field is pinned for this computer.
    pub custom: bool,
}

pub fn apply_override(prefs: &Preferences, over: Option<&ConnectionOverride>) -> Applied {
    let over = over.filter(|over| !over.is_empty());
    let pin = over.and_then(|over| over.video);
    let mut video = match pin {
        Some(pin) => pin_first(&prefs.video, pin),
        None => prefs.video.clone(),
    };
    // A switched-off row is not offered, unless this computer pins it.
    video.retain(|row| Some(*row) == pin || !prefs.video_off.contains(row));
    Applied {
        video,
        audio: match over.and_then(|over| over.audio) {
            Some(pin) => pin_first(&prefs.audio, pin),
            None => prefs.audio.clone(),
        },
        fps: over.and_then(|over| over.fps).unwrap_or(prefs.fps),
        skip_without_hw: over
            .and_then(|over| over.skip_without_hw)
            .unwrap_or(prefs.skip_without_hw),
        trace_input: over
            .and_then(|over| over.trace_input)
            .unwrap_or(prefs.trace_input),
        audio_bitrate_kbps: prefs.audio_bitrate_kbps,
        audio_channels: prefs.audio_channels,
        carrier: over
            .and_then(|over| over.carrier)
            .unwrap_or(etherquic::carrier::QUIC_STREAMS),
        custom: over.is_some(),
    }
}

fn pin_first<T: Copy + PartialEq>(order: &[T], pin: T) -> Vec<T> {
    let mut out = vec![pin];
    for item in order {
        if *item != pin {
            out.push(*item);
        }
    }
    out
}

/// Saved pins, keyed by [`mac_key`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionPrefs {
    by_mac: Vec<(String, ConnectionOverride)>,
    /// Host RTP input port from a standalone `rtp=<port>` line.
    rtp_port: Option<u16>,
}

impl ConnectionPrefs {
    pub fn get(&self, mac: &str) -> Option<&ConnectionOverride> {
        self.by_mac
            .iter()
            .find(|(key, _)| key == mac)
            .map(|(_, over)| over)
    }

    pub fn set(&mut self, mac: String, over: ConnectionOverride) {
        if let Some(index) = self.by_mac.iter().position(|(key, _)| key == &mac) {
            if over.is_empty() {
                self.by_mac.remove(index);
            } else {
                self.by_mac[index].1 = over;
            }
        } else if !over.is_empty() {
            self.by_mac.push((mac, over));
        }
    }
}

pub fn connections_path() -> Option<PathBuf> {
    preferences_path().map(|path| path.with_file_name("connections.txt"))
}

pub fn load_connections() -> ConnectionPrefs {
    let Some(path) = connections_path() else {
        return ConnectionPrefs::default();
    };
    match std::fs::read_to_string(path) {
        Ok(text) => parse_connections(&text),
        Err(_) => ConnectionPrefs::default(),
    }
}

pub fn store_connections(prefs: &ConnectionPrefs) -> Result<(), String> {
    let path = connections_path().ok_or("no home directory for connections")?;
    store_connections_at(&path, prefs)
}

fn store_connections_at(path: &Path, prefs: &ConnectionPrefs) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let body = render_connections(prefs);
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, &body).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("save {}: {err}", path.display()))?;
    Ok(())
}

fn render_connections(prefs: &ConnectionPrefs) -> String {
    let mut rows: Vec<_> = prefs
        .by_mac
        .iter()
        .filter(|(_, over)| !over.is_empty())
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let mut out = String::new();
    if let Some(port) = prefs.rtp_port {
        out.push_str("rtp=");
        out.push_str(&port.to_string());
        out.push('\n');
    }
    for (mac, over) in rows {
        out.push_str(mac);
        if let Some(video) = over.video {
            out.push_str(" video=");
            out.push_str(video.token());
        }
        if let Some(audio) = over.audio {
            out.push_str(" audio=");
            out.push_str(audio.token());
        }
        if let Some(fps) = over.fps {
            out.push_str(" fps=");
            out.push_str(&fps.token());
        }
        if let Some(skip) = over.skip_without_hw {
            out.push_str(" skip-without-hw=");
            out.push_str(yes_no(skip));
        }
        if let Some(trace) = over.trace_input {
            out.push_str(" trace-input=");
            out.push_str(yes_no(trace));
        }
        if let Some(carrier) = over.carrier {
            out.push_str(" carrier=");
            out.push_str(&carrier.to_string());
        }
        if let Some(profile) = &over.profile {
            out.push_str(" profile=");
            out.push_str(profile);
        }
        out.push('\n');
    }
    out
}

fn parse_connections(text: &str) -> ConnectionPrefs {
    let mut prefs = ConnectionPrefs::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A whole line `rtp=<port>`. A bad value does not clear a port already read.
        if let Some(rest) = line.strip_prefix("rtp=") {
            if !rest.is_empty()
                && !rest.bytes().any(|byte| byte.is_ascii_whitespace())
                && rest.bytes().all(|byte| byte.is_ascii_digit())
            {
                if let Ok(port) = rest.parse::<u16>() {
                    if port != 0 {
                        prefs.rtp_port = Some(port);
                    }
                }
            }
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(mac) = parts.next().and_then(normalize_mac) else {
            continue;
        };
        let mut over = ConnectionOverride::default();
        for part in parts {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            match key {
                "video" => over.video = PicturePref::from_token(value),
                "audio" => over.audio = AudioId::from_token(value),
                "fps" => over.fps = FrameRate::from_token(value),
                "skip-without-hw" => over.skip_without_hw = Some(parse_yes(value)),
                "trace-input" => over.trace_input = Some(parse_yes(value)),
                "carrier" => {
                    if let Some(id) = value.parse::<u8>().ok().filter(|id| (1..=5).contains(id)) {
                        over.carrier = Some(id);
                    }
                }
                "profile" => {
                    if value == "plain" || value == "gcm" {
                        over.profile = Some(value.to_string());
                    }
                }
                _ => {}
            }
        }
        prefs.set(mac, over);
    }
    prefs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h265(encode: bool, decode: bool) -> AnnouncedCodec {
        AnnouncedCodec {
            id: VIDEO_H265,
            hardware_encode: encode,
            hardware_decode: decode,
            cpu: false,
            gpu: false,
            libopus: false,
            ffmpeg: false,
            gstreamer: false,
        }
    }

    #[test]
    fn h265_then_mjpeg_when_the_remote_cannot_decode() {
        let remote = [h265(true, false)];
        let local = MachineCodecs::from_rows(vec![(
            CodecId::H265,
            EndSupport {
                hardware_encode: true,
                hardware_decode: true,
                ..EndSupport::default()
            },
        )]);
        let line = video_match_line(
            &[PicturePref::H265Hw, PicturePref::Mjpeg],
            &local,
            Some(&remote),
            false,
        );
        assert!(line.starts_with("Match: MJPEG"), "{line}");
        assert!(
            line.contains("H.265 (HW) skipped: remote has no hardware decode"),
            "{line}"
        );
        let hardware = remote_hardware_line(Some(&remote));
        assert!(hardware.contains("H.265 encode, no decode"), "{hardware}");
    }

    #[test]
    fn an_old_offer_with_no_codecs_also_falls_through_to_mjpeg() {
        let line = video_match_line(
            &[PicturePref::H265Hw, PicturePref::Mjpeg],
            &MachineCodecs::default(),
            None,
            false,
        );
        assert!(line.starts_with("Match: MJPEG"), "{line}");
        assert!(line.contains("remote has no hardware decode"), "{line}");
        assert_eq!(remote_hardware_line(None), "Codec hardware: not announced");
    }

    #[test]
    fn h265_stays_when_the_remote_has_hardware_decode() {
        let remote = [h265(true, true)];
        let local = MachineCodecs::from_rows(vec![(
            CodecId::H265,
            EndSupport {
                hardware_decode: true,
                ..EndSupport::default()
            },
        )]);
        let line = video_match_line(
            &[PicturePref::H265Hw, PicturePref::Mjpeg],
            &local,
            Some(&remote),
            false,
        );
        assert!(line.starts_with("Match: H.265 (HW)"), "{line}");
        assert!(!line.contains("still MJPEG"), "{line}");
    }

    #[test]
    fn negotiated_codecs_come_out_of_the_viewing_status() {
        let line = "Viewing 192.168.0.58:4434 · PyroWave 1280x720 (pyrowave) · Opus 128 kbit/s · 30 fps · 41.5 Mbit/s.";
        assert_eq!(
            negotiated_from_status(line).as_deref(),
            Some("PyroWave 1280x720 (pyrowave) · Opus 128 kbit/s")
        );
        let legacy = "Viewing peer · MJPEG · PCM (the host does not negotiate codecs).";
        assert_eq!(
            negotiated_from_status(legacy).as_deref(),
            Some("MJPEG · PCM")
        );
        assert!(
            negotiated_from_status("Viewing peer · video starting · audio starting.").is_none()
        );
        assert!(negotiated_from_status("Dialing …").is_none());
    }

    #[test]
    fn a_pin_leads_the_offer() {
        let prefs = Preferences::baseline();
        let applied = apply_override(&prefs, None);
        let choice = codec_choice_for(
            &applied,
            Some(PicturePref::Mjpeg),
            None,
            0,
            false,
            etherquic::negotiate::YuvFormat::YUV444P8,
            false,
            false,
            None,
        );
        assert_eq!(choice.video.first(), Some(&PicturePref::Mjpeg));
        assert_eq!(choice.carrier, None);
        assert_eq!(choice.video_pin, Some(PicturePref::Mjpeg));
        assert_eq!(choice.video.len(), applied.video.len());
        assert_eq!(choice.max_width, 0);
        assert_eq!(choice.max_height, 0);
    }

    #[test]
    fn cpu_pyrowave_is_last_can_be_switched_off_and_pinned() {
        let order = default_video();
        assert_eq!(order.last(), Some(&PicturePref::PyrowaveCpu));
        assert_eq!(PicturePref::PyrowaveCpu.name(), "PyroWave (CPU)");
        // A file saved before the row existed gets it appended at the end.
        let old = parse_preferences("video=pyrowave,mjpeg,h264-sw\n");
        assert_eq!(old.video.last(), Some(&PicturePref::PyrowaveCpu));
        let mut prefs = Preferences::baseline();
        prefs.video_off = vec![PicturePref::PyrowaveCpu];
        let back = parse_preferences(&render_preferences(&prefs));
        assert_eq!(back.video_off, vec![PicturePref::PyrowaveCpu]);
        let applied = apply_override(&back, None);
        assert!(!applied.video.contains(&PicturePref::PyrowaveCpu));
        let pinned = ConnectionOverride {
            video: Some(PicturePref::PyrowaveCpu),
            ..ConnectionOverride::default()
        };
        let applied = apply_override(&back, Some(&pinned));
        assert_eq!(applied.video.first(), Some(&PicturePref::PyrowaveCpu));
    }

    #[test]
    fn default_order_is_hardware_first_and_can_move() {
        let mut order = default_video();
        assert_eq!(order, PicturePref::order().to_vec());
        assert_eq!(default_audio(), AudioId::order().to_vec());
        let index = order
            .iter()
            .position(|id| *id == PicturePref::Mjpeg)
            .unwrap();
        swap_earlier(&mut order, index);
        assert_eq!(order[index - 1], PicturePref::Mjpeg);
        assert_eq!(order[index], PicturePref::Mj2k);
    }

    #[test]
    fn the_connection_shows_only_the_winning_codec() {
        let remote = [h265(true, false)];
        let local = MachineCodecs::from_rows(vec![(
            CodecId::H265,
            EndSupport {
                hardware_encode: true,
                hardware_decode: true,
                ..EndSupport::default()
            },
        )]);
        let line = top_selection(
            &[PicturePref::H265Hw, PicturePref::Mjpeg],
            &[AudioId::Pcm],
            &local,
            &AudioMachine::default(),
            Some(&remote),
            None,
            false,
            FrameRate::Auto,
            Some(60),
        );
        assert_eq!(line, "MJPEG · PCM · 15 fps");
    }

    #[test]
    fn a_missing_preferred_tool_is_named_and_the_other_tool_stays_visible() {
        let probe = BackendProbe {
            libopus: false,
            ffmpeg: false,
            gstreamer: true,
        };
        assert!(!audio_for(probe, MediaBackend::Libopus).opus);
        assert!(audio_for(probe, MediaBackend::Gstreamer).opus);
        let line = backend_line(probe, MediaBackend::Gstreamer);
        assert!(line.contains("Detected: GStreamer."), "{line}");
        assert!(line.contains("libopus, ffmpeg not found"), "{line}");
        assert!(line.contains("Using GStreamer"), "{line}");
    }

    #[test]
    fn opus_tools_start_with_builtin_and_follow_the_order() {
        assert_eq!(
            etherquic::default_backend_order(),
            [
                MediaBackend::Libopus,
                MediaBackend::Ffmpeg,
                MediaBackend::Gstreamer,
            ]
        );
        assert_eq!(backend_label(MediaBackend::Libopus), "Builtin");
        assert_eq!(backend_label(MediaBackend::Ffmpeg), "FFmpeg");
        let probe = BackendProbe {
            libopus: false,
            ffmpeg: false,
            gstreamer: true,
        };
        let mut order = etherquic::default_backend_order().to_vec();
        assert_eq!(
            etherquic::pick_backend(&order, &probe),
            MediaBackend::Gstreamer
        );
        let index = order
            .iter()
            .position(|tool| *tool == MediaBackend::Gstreamer)
            .unwrap();
        swap_earlier(&mut order, index);
        swap_earlier(&mut order, index - 1);
        assert_eq!(order[0], MediaBackend::Gstreamer);
        assert_eq!(
            etherquic::pick_backend(&order, &probe),
            MediaBackend::Gstreamer
        );
        assert_eq!(
            backend_hint(MediaBackend::Libopus, false, false),
            "libopus, not found"
        );
        assert_eq!(
            backend_hint(MediaBackend::Gstreamer, true, true),
            "GStreamer, found, in use"
        );
    }

    #[test]
    fn the_remote_names_ffmpeg_when_the_offer_sets_that_bit() {
        let audio = [AnnouncedCodec {
            id: AUDIO_OPUS,
            hardware_encode: false,
            hardware_decode: false,
            cpu: true,
            gpu: false,
            libopus: false,
            ffmpeg: true,
            gstreamer: false,
        }];
        assert_eq!(
            remote_backend_line(Some(&audio)),
            "Remote audio tools: ffmpeg"
        );
        assert_eq!(
            remote_backend_line(None),
            "Remote audio tools: not announced"
        );
    }

    #[test]
    fn a_saved_order_comes_back_and_a_missing_row_is_appended() {
        let mut prefs = Preferences::baseline();
        prefs.video.swap(0, 4);
        prefs.audio = vec![AudioId::Aac, AudioId::Opus, AudioId::Pcm];
        prefs.opus = vec![
            MediaBackend::Gstreamer,
            MediaBackend::Libopus,
            MediaBackend::Ffmpeg,
        ];
        prefs.skip_without_hw = true;
        prefs.trace_input = true;
        prefs.fps = FrameRate::Fixed(30);
        let loaded = parse_preferences(&render_preferences(&prefs));
        assert_eq!(loaded, prefs);

        let partial = parse_preferences("video=mjpeg,nope\naudio=pcm\nopus=ffmpeg\n");
        assert_eq!(partial.video[0], PicturePref::Mjpeg);
        assert_eq!(partial.video.len(), PicturePref::order().len());
        assert!(partial.video.contains(&PicturePref::H264Sw));
        assert_eq!(
            partial.audio,
            vec![AudioId::Pcm, AudioId::Opus, AudioId::Aac]
        );
        assert_eq!(partial.opus[0], MediaBackend::Ffmpeg);
        assert_eq!(partial.opus.len(), 3);
        assert!(!partial.skip_without_hw);
        assert_eq!(partial.fps, FrameRate::Auto);
        assert_eq!(partial.audio_bitrate_kbps, 0);
        assert_eq!(partial.audio_channels, 0);

        let path = std::env::temp_dir().join(format!(
            "etherquic-prefs-{}-{}.txt",
            std::process::id(),
            "order"
        ));
        store_preferences_at(&path, &prefs).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(parse_preferences(&text), prefs);
    }

    #[test]
    fn auto_is_fifteen_software_and_thirty_hardware() {
        assert_eq!(resolve_fps(FrameRate::Auto, Some(60), true), HARDWARE_FPS);
        assert_eq!(resolve_fps(FrameRate::Auto, Some(60), false), SOFTWARE_FPS);
        assert_eq!(resolve_fps(FrameRate::Auto, Some(8), false), SOFTWARE_FPS);
        assert_eq!(resolve_fps(FrameRate::Auto, None, true), HARDWARE_FPS);
        assert_eq!(resolve_fps(FrameRate::Match, Some(144), true), 144);
        assert_eq!(resolve_fps(FrameRate::Match, None, false), SOFTWARE_FPS);
        assert_eq!(resolve_fps(FrameRate::Fixed(30), Some(60), false), 30);
        assert_eq!(resolve_fps(FrameRate::Fixed(500), None, false), 240);
        assert_eq!(
            fps_headline(FrameRate::Auto, None, true),
            format!("{HARDWARE_FPS} fps")
        );
        assert_eq!(
            fps_headline(FrameRate::Auto, Some(60), true),
            format!("{HARDWARE_FPS} fps")
        );
        assert_eq!(
            fps_headline(FrameRate::Auto, Some(60), false),
            format!("{SOFTWARE_FPS} fps")
        );
        assert_eq!(fps_headline(FrameRate::Match, None, true), "Match remote");
        assert!(sender_hardware_encode(Some(PicturePref::H264Hw)));
        assert!(!sender_hardware_encode(Some(PicturePref::Mjpeg)));
        assert!(sender_hardware_encode(Some(PicturePref::Pyrowave)));
        assert!(!sender_hardware_encode(Some(PicturePref::PyrowaveCpu)));
        assert!(!sender_hardware_encode(Some(PicturePref::H264Sw)));
    }

    #[test]
    fn audio_bitrate_chips_scale_with_layout() {
        assert_eq!(scale_audio_kbps(96, 2), 96);
        assert_eq!(scale_audio_kbps(96, 3), 108);
        assert_eq!(scale_audio_kbps(96, 6), 128);
        assert_eq!(scale_audio_kbps(96, 8), 144);
        assert_eq!(audio_layout_name(0), "Auto");
        assert_eq!(audio_layout_name(6), "5.1");
        let mut prefs = Preferences::baseline();
        prefs.audio_bitrate_kbps = 96;
        prefs.audio_channels = 6;
        let loaded = parse_preferences(&render_preferences(&prefs));
        assert_eq!(loaded.audio_bitrate_kbps, 96);
        assert_eq!(loaded.audio_channels, 6);
        assert_eq!(
            parse_preferences("audio-bitrate=auto\naudio-channels=auto\n").audio_channels,
            0
        );
    }

    #[test]
    fn pyrowave_hardware_is_chosen_by_default() {
        let remote = [AnnouncedCodec {
            id: VIDEO_PYROWAVE,
            hardware_encode: true,
            hardware_decode: false,
            cpu: false,
            gpu: true,
            libopus: false,
            ffmpeg: false,
            gstreamer: false,
        }];
        let local = MachineCodecs::from_rows(vec![(
            CodecId::Pyrowave,
            EndSupport {
                gpu_compute: true,
                ..EndSupport::default()
            },
        )]);
        let chosen = chosen_video(&default_video(), &local, Some(&remote), true);
        assert_eq!(chosen, Some(PicturePref::Pyrowave));
        let line = remote_hardware_line(Some(&remote));
        assert!(line.contains("Pyrowave HW encode"), "{line}");
        let mut both = remote;
        both[0].hardware_decode = true;
        let line = remote_hardware_line(Some(&both));
        assert!(line.contains("Pyrowave encode+decode"), "{line}");
    }

    #[test]
    fn a_connection_can_pin_mjpeg_without_changing_the_global_order() {
        let prefs = Preferences::baseline();
        let global = prefs.video.clone();
        let over = ConnectionOverride {
            video: Some(PicturePref::Mjpeg),
            fps: Some(FrameRate::Fixed(30)),
            ..ConnectionOverride::default()
        };
        let applied = apply_override(&prefs, Some(&over));
        assert!(applied.custom);
        assert_eq!(applied.video[0], PicturePref::Mjpeg);
        assert_eq!(applied.video.len(), global.len());
        assert!(applied.video.contains(&PicturePref::H264Sw));
        assert_eq!(prefs.video, global);
        assert_eq!(applied.fps, FrameRate::Fixed(30));
        assert_eq!(applied.audio, prefs.audio);
        assert!(!apply_override(&prefs, None).custom);

        let line = top_selection(
            &applied.video,
            &applied.audio,
            &MachineCodecs::default(),
            &AudioMachine::default(),
            Some(&[]),
            None,
            false,
            applied.fps,
            Some(60),
        );
        assert!(line.starts_with("MJPEG · "), "{line}");
        assert!(line.ends_with("30 fps"), "{line}");
    }

    #[test]
    fn a_connection_file_round_trips_and_drops_an_empty_override() {
        let mac = "02:00:00:00:00:02";
        let key = normalize_mac(mac).unwrap();
        assert_eq!(key, mac_key([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]));
        let text = "\
# ignored
02:00:00:00:00:02 video=mjpeg audio=pcm fps=30 skip-without-hw=yes trace-input=no
not-a-mac video=mjpeg
";
        let loaded = parse_connections(text);
        let over = loaded.get(&key).unwrap();
        assert_eq!(over.video, Some(PicturePref::Mjpeg));
        assert_eq!(over.audio, Some(AudioId::Pcm));
        assert_eq!(over.fps, Some(FrameRate::Fixed(30)));
        assert_eq!(over.skip_without_hw, Some(true));
        assert_eq!(over.trace_input, Some(false));
        assert_eq!(parse_connections(&render_connections(&loaded)), loaded);

        let mut cleared = loaded.clone();
        cleared.set(key.clone(), ConnectionOverride::default());
        assert!(cleared.get(&key).is_none());
        assert!(render_connections(&cleared).is_empty());

        let path = std::env::temp_dir().join(format!(
            "etherquic-connections-{}-{}.txt",
            std::process::id(),
            "pin"
        ));
        store_connections_at(&path, &loaded).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(parse_connections(&body), loaded);
        assert!(body.contains("video=mjpeg"));
        assert!(!body.contains("not-a-mac"));

        let pinned = "020000000003 carrier=3 carrier=9\n020000000004 carrier=0\n";
        let loaded = parse_connections(pinned);
        let key = mac_key([2, 0, 0, 0, 0, 3]);
        let over = loaded.get(&key).unwrap();
        assert_eq!(over.carrier, Some(3));
        assert!(!over.is_empty());
        assert!(loaded.get(&mac_key([2, 0, 0, 0, 0, 4])).is_none());
        let again = parse_connections(&render_connections(&loaded));
        assert_eq!(again.get(&key).unwrap().carrier, Some(3));
        let marked = "\
020000000006 carrier=3 profile=gcm profile=nope
020000000007 profile=bogus
";
        let loaded = parse_connections(marked);
        let key = mac_key([2, 0, 0, 0, 0, 6]);
        let over = loaded.get(&key).unwrap();
        assert_eq!(over.carrier, Some(3));
        assert_eq!(over.profile.as_deref(), Some("gcm"));
        assert!(!over.is_empty());
        assert!(loaded.get(&mac_key([2, 0, 0, 0, 0, 7])).is_none());
        let again = parse_connections(&render_connections(&loaded));
        assert_eq!(again.get(&key).unwrap().profile.as_deref(), Some("gcm"));
        assert!(render_connections(&loaded).contains("profile=gcm"));
        let applied = apply_override(&Preferences::baseline(), Some(over));
        assert_eq!(applied.carrier, 3);
        assert_eq!(
            apply_override(&Preferences::baseline(), None).carrier,
            etherquic::carrier::QUIC_STREAMS
        );
        let choice = codec_choice_for(
            &applied,
            None,
            None,
            0,
            false,
            etherquic::negotiate::YuvFormat::YUV444P8,
            false,
            false,
            over.carrier,
        );
        assert_eq!(choice.carrier, Some(3));

        let with_rtp = "\
rtp=21002
020000000006 carrier=3 profile=gcm
rtp=0
rtp=nope
";
        let loaded = parse_connections(with_rtp);
        assert_eq!(loaded.rtp_port, Some(21002));
        assert_eq!(loaded.get(&key).unwrap().carrier, Some(3));
        let again = parse_connections(&render_connections(&loaded));
        assert_eq!(again, loaded);
        assert!(render_connections(&loaded).starts_with("rtp=21002\n"));
        let mut dropped = loaded.clone();
        dropped.rtp_port = None;
        dropped.set(key, ConnectionOverride::default());
        assert!(render_connections(&dropped).is_empty());
    }

    #[test]
    fn local_codec_rows_put_library_errors_on_the_codec() {
        use etherquic::negotiate::{EncodeCap, HostCaps, Limit};
        let machine = MachineCodecs::from_rows(vec![
            (
                CodecId::Mjpeg,
                EndSupport {
                    cpu_can_sustain: true,
                    ..EndSupport::default()
                },
            ),
            (
                CodecId::H264,
                EndSupport {
                    hardware_encode: true,
                    ..EndSupport::default()
                },
            ),
        ]);
        let probe = Probe {
            caps: HostCaps {
                video: vec![
                    EncodeCap {
                        video: Video::Mjpeg,
                        encoder: "jpeg".into(),
                        limit: Limit::NONE,
                    },
                    EncodeCap {
                        video: Video::H264Hw,
                        encoder: "h264_nvenc".into(),
                        limit: Limit::NONE,
                    },
                ],
                audio: vec![],
            },
            decodes: vec![],
            audio_decodes: vec![],
            notes: vec![
                "ffmpeg: /usr/bin/ffmpeg".into(),
                "pyrowave encode: libpyrowave-shared not found".into(),
            ],
        };
        let rows = local_codec_rows(&machine, Some(&probe), false);
        let pyro = rows.iter().find(|row| row.id == CodecId::Pyrowave).unwrap();
        assert!(!pyro.software && !pyro.hardware);
        assert_eq!(pyro.backend, "—");
        let err = pyro.error.as_deref().unwrap();
        assert!(err.contains("Pyrowave library: not found"), "{err}");
        assert!(err.contains("pyrowave encode:"), "{err}");
        let jpegxs = rows.iter().find(|row| row.id == CodecId::JpegXs).unwrap();
        assert!(!jpegxs.software && !jpegxs.hardware);
        assert!(jpegxs
            .error
            .as_deref()
            .unwrap()
            .contains("ffmpeg encoder"));
        let h264 = rows.iter().find(|row| row.id == CodecId::H264).unwrap();
        assert!(!h264.software && h264.hardware);
        assert_eq!(h264.backend, "h264_nvenc");
        assert!(h264.error.is_none());
        assert!(h264.i_frame && h264.p_frame && !h264.b_frame);
        let mjpeg = rows.iter().find(|row| row.id == CodecId::Mjpeg).unwrap();
        assert!(mjpeg.software && !mjpeg.hardware);
        assert_eq!(mjpeg.backend, "jpeg");
        assert!(mjpeg.error.is_none());
        let text = codec_table_text(&rows);
        assert!(text.contains("codec\tSW\tHW\tbackend\terror"));
        assert!(text.contains("H.264\tno\tyes\th264_nvenc"));
    }
}
