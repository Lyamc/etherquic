//! Codec negotiation, version 1.
//!
//! Version signal: a player and host that both speak negotiation agree on the
//! ALPN [`ALPN_NEGOTIATE`] (`etherquic/2`). Either side that only knows the
//! original `etherquic` ALPN gets that one instead, and the session is the
//! legacy one: MJPEG pictures on `EVID`, PCM on `EAUD`, no offer stream.
//!
//! With `etherquic/2` the player opens a uni stream with magic [`OFFER_MAGIC`]
//! first and writes one [`Offer`] record. It may write more offers later on the
//! same stream (a decoder broke, the user changed a preference). The host
//! answers by opening `EVI2` and `EAU2`; the first record on each is a
//! [`StreamConfig`], and a later config record on the same stream switches the
//! codec mid-session.
//!
//! Records are UTF-8 lines of `key value...`. Unknown keys are ignored, so a
//! later version can add fields without breaking this one. The first line is
//! `eqneg <version>`.

use std::fmt::Write as _;

use crate::look::{Dither, Look};

/// ALPN of a session that negotiates codecs.
pub const ALPN_NEGOTIATE: &[u8] = b"etherquic/2";
/// Negotiation record version written by this build.
pub const VERSION: u32 = 1;
/// JPEG quality and the 100% mark for bitrate scaling when the offer omits
/// `quality` or sends 0.
pub const DEFAULT_QUALITY: u8 = 40;

/// Encode rate when Auto picks a software encoder (MJPEG, CPU PyroWave, …).
pub const SOFTWARE_FPS: u32 = 15;
/// Encode rate when Auto picks a hardware encoder.
pub const HARDWARE_FPS: u32 = 30;
/// Capture rate when the display refresh cannot be read. Not an encode cap.
pub const DEFAULT_CAPTURE_FPS: u32 = 60;
/// Highest rate an offer or chip may ask for.
pub const MAX_FPS: u32 = 240;

/// Pictures per second the player wants, or a host default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FpsWish {
    /// 15 fps software, 30 fps hardware. The host's own Auto uses this too.
    #[default]
    Auto,
    /// The host's display refresh. Capture already follows that rate.
    Match,
    Fixed(u32),
}

impl FpsWish {
    pub fn token(self) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Match => "match".into(),
            Self::Fixed(fps) => fps.to_string(),
        }
    }

    pub fn from_token(token: &str) -> Self {
        let token = token.trim();
        if token.is_empty() || token.eq_ignore_ascii_case("auto") {
            return Self::Auto;
        }
        if token.eq_ignore_ascii_case("match")
            || token.eq_ignore_ascii_case("remote")
            || token.eq_ignore_ascii_case("display")
        {
            return Self::Match;
        }
        match token.parse::<u32>() {
            Ok(fps) if (1..=MAX_FPS).contains(&fps) => Self::Fixed(fps),
            _ => Self::Auto,
        }
    }

    pub fn resolve(self, hardware: bool, display_hz: Option<u32>) -> u32 {
        match self {
            Self::Auto => {
                if hardware {
                    HARDWARE_FPS
                } else {
                    SOFTWARE_FPS
                }
            }
            Self::Match => display_hz
                .map(clamp_fps)
                .unwrap_or_else(|| Self::Auto.resolve(hardware, None)),
            Self::Fixed(fps) => clamp_fps(fps),
        }
    }
}

pub fn clamp_fps(fps: u32) -> u32 {
    fps.clamp(1, MAX_FPS)
}

/// Stereo-equivalent Auto audio bitrate. 96 kbps stereo ≡ 108 2.1 ≡ 128 5.1 ≡ 144 7.1.
pub const AUDIO_STEREO_DEFAULT_KBPS: u32 = 96;
/// Auto max channels is stereo. The host never upmixes past capture.
pub const AUDIO_CHANNELS_AUTO: u16 = 2;
/// Highest channel count an offer may ask for (7.1).
pub const AUDIO_CHANNELS_MAX: u16 = 8;

/// Zero in the offer is Auto (stereo).
pub fn audio_channels_or_auto(max: u16) -> u16 {
    if max == 0 {
        AUDIO_CHANNELS_AUTO
    } else {
        max.min(AUDIO_CHANNELS_MAX)
    }
}

/// Scale a stereo-equivalent bitrate to a channel layout.
///
/// 96 kbps stereo ≡ 108 2.1 ≡ 128 5.1 ≡ 144 7.1. Zero stereo kbps uses
/// [`AUDIO_STEREO_DEFAULT_KBPS`].
pub fn scale_audio_kbps(stereo_kbps: u32, channels: u16) -> u32 {
    let stereo = if stereo_kbps == 0 {
        AUDIO_STEREO_DEFAULT_KBPS
    } else {
        stereo_kbps
    };
    (stereo * audio_kbps_numerator(channels)) / 96
}

/// Inverse of [`scale_audio_kbps`]: layout kbps back to a stereo equivalent.
pub fn stereo_audio_kbps(layout_kbps: u32, channels: u16) -> u32 {
    let num = audio_kbps_numerator(channels);
    if num == 0 {
        layout_kbps
    } else {
        (layout_kbps * 96) / num
    }
}

fn audio_kbps_numerator(channels: u16) -> u32 {
    match audio_channels_or_auto(channels) {
        1 => 64,
        2 => 96,
        3 => 108,
        4 => 115,
        5 => 121,
        6 => 128,
        7 => 136,
        _ => 144,
    }
}

/// Player Auto (or an old offer) defers to the host. A number or Match wins.
pub fn session_fps_wish(player: Option<FpsWish>, host: FpsWish) -> FpsWish {
    match player {
        None | Some(FpsWish::Auto) => host,
        Some(wish) => wish,
    }
}

/// `quality` 0 is the default. Anything else is 1–100.
pub fn clamp_quality(quality: u8) -> u8 {
    if quality == 0 {
        DEFAULT_QUALITY
    } else {
        quality.clamp(1, 100)
    }
}

/// Scale a codec's default budget by the offer's quality. 40% is the default.
pub fn scale_budget(default: u32, quality: u8) -> u32 {
    let q = u64::from(clamp_quality(quality));
    let scaled = u64::from(default) * q / u64::from(DEFAULT_QUALITY);
    scaled.clamp(1, u64::from(u32::MAX)) as u32
}

/// Chroma siting for PyroWave. The bitstream has 4:2:0 and 4:4:4; 4:2:2 is
/// encoded as 4:4:4 after a horizontal chroma upsample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum YuvChroma {
    Yuv420,
    Yuv422,
    #[default]
    Yuv444,
}

impl YuvChroma {
    pub const ALL: [YuvChroma; 3] = [Self::Yuv420, Self::Yuv422, Self::Yuv444];

    pub fn token(self) -> &'static str {
        match self {
            Self::Yuv420 => "420",
            Self::Yuv422 => "422",
            Self::Yuv444 => "444",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Yuv420 => "4:2:0",
            Self::Yuv422 => "4:2:2",
            Self::Yuv444 => "4:4:4",
        }
    }

    /// Layout the PyroWave sequence header carries (1 bit: 420 or 444).
    pub fn bitstream(self) -> Self {
        match self {
            Self::Yuv420 => Self::Yuv420,
            Self::Yuv422 | Self::Yuv444 => Self::Yuv444,
        }
    }
}

/// Sample depth for PyroWave planes. The bitstream is float; this is the
/// CPU buffer the encoder feeds and the decoder writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum YuvDepth {
    #[default]
    Depth8,
    Depth10,
}

impl YuvDepth {
    pub const ALL: [YuvDepth; 2] = [Self::Depth8, Self::Depth10];

    pub fn bits(self) -> u8 {
        match self {
            Self::Depth8 => 8,
            Self::Depth10 => 10,
        }
    }

    pub fn max_value(self) -> u16 {
        match self {
            Self::Depth8 => 255,
            Self::Depth10 => 1023,
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Depth8 => "8",
            Self::Depth10 => "10",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Depth8 => "8-bit",
            Self::Depth10 => "10-bit",
        }
    }
}

/// One PyroWave colour format: chroma × depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YuvFormat {
    pub chroma: YuvChroma,
    pub depth: YuvDepth,
}

impl YuvFormat {
    pub const YUV420P8: Self = Self {
        chroma: YuvChroma::Yuv420,
        depth: YuvDepth::Depth8,
    };
    pub const YUV422P8: Self = Self {
        chroma: YuvChroma::Yuv422,
        depth: YuvDepth::Depth8,
    };
    pub const YUV444P8: Self = Self {
        chroma: YuvChroma::Yuv444,
        depth: YuvDepth::Depth8,
    };
    pub const YUV420P10: Self = Self {
        chroma: YuvChroma::Yuv420,
        depth: YuvDepth::Depth10,
    };
    pub const YUV422P10: Self = Self {
        chroma: YuvChroma::Yuv422,
        depth: YuvDepth::Depth10,
    };
    pub const YUV444P10: Self = Self {
        chroma: YuvChroma::Yuv444,
        depth: YuvDepth::Depth10,
    };

    pub const ALL: [YuvFormat; 6] = [
        Self::YUV420P8,
        Self::YUV422P8,
        Self::YUV444P8,
        Self::YUV420P10,
        Self::YUV422P10,
        Self::YUV444P10,
    ];

    pub fn token(self) -> &'static str {
        match (self.chroma, self.depth) {
            (YuvChroma::Yuv420, YuvDepth::Depth8) => "420p8",
            (YuvChroma::Yuv422, YuvDepth::Depth8) => "422p8",
            (YuvChroma::Yuv444, YuvDepth::Depth8) => "444p8",
            (YuvChroma::Yuv420, YuvDepth::Depth10) => "420p10",
            (YuvChroma::Yuv422, YuvDepth::Depth10) => "422p10",
            (YuvChroma::Yuv444, YuvDepth::Depth10) => "444p10",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.token() == token)
    }

    pub fn label(self) -> String {
        format!("{} {}", self.chroma.label(), self.depth.label())
    }

    /// Sequence-header chroma the decoder must be created with.
    pub fn bitstream(self) -> Self {
        Self {
            chroma: self.chroma.bitstream(),
            depth: self.depth,
        }
    }

    pub fn needs_444_decoder(self) -> bool {
        self.chroma.bitstream() == YuvChroma::Yuv444
    }
}

impl Default for YuvFormat {
    fn default() -> Self {
        Self::YUV444P8
    }
}
/// Player to host, one stream, offers.
pub const OFFER_MAGIC: &[u8; 4] = b"ENEG";
/// Host to player, negotiated pictures.
pub const VIDEO2_MAGIC: &[u8; 4] = b"EVI2";
/// Host to player, negotiated audio.
pub const AUDIO2_MAGIC: &[u8; 4] = b"EAU2";
/// First byte of an `EVI2`/`EAU2` record: a [`StreamConfig`].
pub const RECORD_CONFIG: u8 = b'C';
/// First byte of an `EVI2`/`EAU2` record: coded media.
pub const RECORD_DATA: u8 = b'D';
/// First byte of an `EVI2` record: the host cursor. A player that does not
/// know it skips the record. It is not sent on the legacy `EVID` stream.
pub const RECORD_CURSOR: u8 = b'P';
/// First byte of an `EVI2` record: a dirty-rectangle update. Payload is
/// x, y, w, h (u16le) then a coded tile. A player that does not know it
/// skips the record. The host only sends these when the offer has
/// `knows dirty-rects`.
pub const RECORD_PATCH: u8 = b'R';

/// One row of a picture preference list. The suffix is the sender's encoder:
/// `-hw` is an encode ASIC (NVENC, VideoToolbox, AMF, Quick Sync), `-sw` the CPU.
/// PyroWave is two rows over one bitstream: `pyrowave` runs on a GPU (or on
/// Metal through KosmicKrisp), `pyrowave-cpu` on a CPU Vulkan driver
/// (lavapipe/llvmpipe, SwiftShader). A host from before the split announces
/// plain `pyrowave` whatever it ran on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Video {
    Av1Hw,
    H265Hw,
    H264Hw,
    Pyrowave,
    JpegXs,
    OpenApv,
    Mj2k,
    Mjpeg,
    H264Sw,
    PyrowaveCpu,
}

/// The bitstream a [`Video`] row produces. The player decodes this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    Av1,
    H265,
    H264,
    Pyrowave,
    JpegXs,
    OpenApv,
    Mj2k,
    Mjpeg,
}

impl Video {
    /// Default order. PyroWave on a CPU is last: it works, but takes many
    /// cores and about 40 Mbit/s, so Auto prefers MJ2K, MJPEG, and x264.
    /// JPEG XS and OpenAPV sit after GPU PyroWave: intra ffmpeg rows, no
    /// encode ASIC.
    pub const ALL: [Video; 10] = [
        Video::Av1Hw,
        Video::H265Hw,
        Video::H264Hw,
        Video::Pyrowave,
        Video::JpegXs,
        Video::OpenApv,
        Video::Mj2k,
        Video::Mjpeg,
        Video::H264Sw,
        Video::PyrowaveCpu,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Video::Av1Hw => "av1-hw",
            Video::H265Hw => "h265-hw",
            Video::H264Hw => "h264-hw",
            Video::Pyrowave => "pyrowave",
            Video::JpegXs => "jpegxs",
            Video::OpenApv => "openapv",
            Video::Mj2k => "mj2k",
            Video::Mjpeg => "mjpeg",
            Video::H264Sw => "h264-sw",
            Video::PyrowaveCpu => "pyrowave-cpu",
        }
    }

    pub fn from_token(token: &str) -> Option<Video> {
        Video::ALL.into_iter().find(|row| row.token() == token)
    }

    pub fn name(self) -> &'static str {
        match self {
            Video::Av1Hw => "AV1 (HW)",
            Video::H265Hw => "H.265 (HW)",
            Video::H264Hw => "H.264 (HW)",
            Video::Pyrowave => "PyroWave (GPU)",
            Video::JpegXs => "JPEG XS",
            Video::OpenApv => "OpenAPV",
            Video::Mj2k => "MJ2K",
            Video::Mjpeg => "MJPEG",
            Video::H264Sw => "H.264 (SW)",
            Video::PyrowaveCpu => "PyroWave (CPU)",
        }
    }

    pub fn codec(self) -> VideoCodec {
        match self {
            Video::Av1Hw => VideoCodec::Av1,
            Video::H265Hw => VideoCodec::H265,
            Video::H264Hw | Video::H264Sw => VideoCodec::H264,
            Video::Pyrowave | Video::PyrowaveCpu => VideoCodec::Pyrowave,
            Video::JpegXs => VideoCodec::JpegXs,
            Video::OpenApv => VideoCodec::OpenApv,
            Video::Mj2k => VideoCodec::Mj2k,
            Video::Mjpeg => VideoCodec::Mjpeg,
        }
    }

    /// Hardware encode on the sender.
    ///
    /// GPU PyroWave counts: a Direct3D 11 device or a non-CPU Vulkan device.
    /// CPU PyroWave does not. MJPEG and H.264 (SW) do not.
    pub fn hardware(self) -> bool {
        matches!(
            self,
            Video::Av1Hw | Video::H265Hw | Video::H264Hw | Video::Pyrowave
        )
    }

    /// One bad coded picture can be skipped. The decoder stays up. H.264,
    /// H.265, and AV1 still fail the row: a gap there needs a new IDR.
    pub fn skip_bad_frame(self) -> bool {
        matches!(
            self,
            Video::Mjpeg
                | Video::Mj2k
                | Video::JpegXs
                | Video::OpenApv
                | Video::Pyrowave
                | Video::PyrowaveCpu
        )
    }

    /// A dirty tile can be coded on its own. The player composites it.
    pub fn patch_updates(self) -> bool {
        matches!(self, Video::Mjpeg | Video::Mj2k)
    }

    /// What [`StreamConfig`] echoes for this row when the player asked for
    /// dirty rectangles. `knows_patches` is the player listing `knows dirty-rects`.
    pub fn dirty_rects_mode(self, want: bool, knows_patches: bool) -> DirtyRectsMode {
        if !want {
            DirtyRectsMode::Off
        } else if self.patch_updates() && knows_patches {
            DirtyRectsMode::Patches
        } else {
            DirtyRectsMode::Pskip
        }
    }
}

/// What the host is doing with dirty rectangles. Written on [`StreamConfig`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirtyRectsMode {
    /// Every captured picture is a full `D`.
    #[default]
    Off,
    /// Intra row: type-`R` tiles when the dirty plan is small.
    Patches,
    /// Skip unchanged pictures; a dirty picture is a full `D`. Temporal
    /// encoders also open a long GOP.
    Pskip,
}

impl DirtyRectsMode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Patches => "patches",
            Self::Pskip => "pskip",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "off" => Some(Self::Off),
            "patches" => Some(Self::Patches),
            "pskip" => Some(Self::Pskip),
            _ => None,
        }
    }

    /// Shown on the session status when the host is skipping unchanged pictures.
    pub fn status_token(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Patches => Some("patches"),
            Self::Pskip => Some("pskip"),
        }
    }
}

/// Audio rows. AAC is listed for preference files but nothing encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Audio {
    Opus,
    Aac,
    Pcm,
}

impl Audio {
    pub const ALL: [Audio; 3] = [Audio::Opus, Audio::Aac, Audio::Pcm];

    pub fn token(self) -> &'static str {
        match self {
            Audio::Opus => "opus",
            Audio::Aac => "aac",
            Audio::Pcm => "pcm",
        }
    }

    pub fn from_token(token: &str) -> Option<Audio> {
        Audio::ALL.into_iter().find(|row| row.token() == token)
    }

    pub fn name(self) -> &'static str {
        match self {
            Audio::Opus => "Opus",
            Audio::Aac => "AAC",
            Audio::Pcm => "PCM",
        }
    }
}

/// Largest picture and rate an encoder or decoder takes. Zero is "no limit".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limit {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Limit {
    pub const NONE: Limit = Limit {
        width: 0,
        height: 0,
        fps: 0,
    };

    pub fn fits(&self, width: u32, height: u32) -> bool {
        (self.width == 0 || width <= self.width) && (self.height == 0 || height <= self.height)
    }

    fn text(&self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.fps)
    }

    fn parse(text: &str) -> Option<Limit> {
        let (size, fps) = text.split_once('@').unwrap_or((text, "0"));
        let (w, h) = size.split_once('x')?;
        Some(Limit {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            fps: fps.parse().ok()?,
        })
    }
}

/// One encoder a host can run, verified by a test encode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeCap {
    pub video: Video,
    /// Encoder name for the status line (`libx264`, `h264_videotoolbox`, `pyrowave`).
    pub encoder: String,
    pub limit: Limit,
}

/// What a host encodes. MJPEG and PCM are always there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCaps {
    pub video: Vec<EncodeCap>,
    pub audio: Vec<Audio>,
}

impl HostCaps {
    /// What every host, even one with no encoder libraries, can send.
    pub fn baseline() -> HostCaps {
        HostCaps {
            video: vec![EncodeCap {
                video: Video::Mjpeg,
                encoder: "jpeg".into(),
                limit: Limit::NONE,
            }],
            audio: vec![Audio::Pcm],
        }
    }

    pub fn encodes(&self, video: Video) -> Option<&EncodeCap> {
        self.video.iter().find(|cap| cap.video == video)
    }

    /// The two caps lines, for the discovery capability block.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write_lines(&mut out);
        out
    }

    /// Read [`HostCaps::to_text`]. `None` when it names no video encoder.
    pub fn from_text(text: &str) -> Option<HostCaps> {
        let mut caps = HostCaps {
            video: Vec::new(),
            audio: Vec::new(),
        };
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let Some(key) = words.next() else { continue };
            let rest: Vec<&str> = words.collect();
            caps.read_line(key, &rest);
        }
        (!caps.video.is_empty()).then_some(caps)
    }

    /// `host-video` and `host-audio` lines.
    pub fn write_lines(&self, out: &mut String) {
        let video: Vec<String> = self
            .video
            .iter()
            .map(|cap| format!("{}={}:{}", cap.video.token(), cap.encoder, cap.limit.text()))
            .collect();
        let audio: Vec<&str> = self.audio.iter().map(|codec| codec.token()).collect();
        let _ = writeln!(out, "host-video {}", video.join(" "));
        let _ = writeln!(out, "host-audio {}", audio.join(" "));
    }

    fn read_line(&mut self, key: &str, words: &[&str]) {
        match key {
            "host-video" => {
                self.video = words
                    .iter()
                    .filter_map(|word| {
                        let (token, rest) = word.split_once('=')?;
                        let (encoder, limit) = rest.rsplit_once(':').unwrap_or((rest, "0x0@0"));
                        Some(EncodeCap {
                            video: Video::from_token(token)?,
                            encoder: encoder.to_string(),
                            limit: Limit::parse(limit).unwrap_or_default(),
                        })
                    })
                    .collect();
            }
            "host-audio" => {
                self.audio = words
                    .iter()
                    .filter_map(|word| Audio::from_token(word))
                    .collect();
            }
            _ => {}
        }
    }
}

/// The player's ordered wishes, already narrowed to what it can decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub version: u32,
    pub video: Vec<Video>,
    pub audio: Vec<Audio>,
    /// Largest picture the player wants. Zero is "no limit".
    pub max: Limit,
    /// Picture bitrate ceiling in kbit/s. Zero lets the host pick.
    pub bitrate_kbps: u32,
    /// Rows the player could not decode in this session.
    pub failed: Vec<Video>,
    /// The player knows `pyrowave-cpu` (line `knows pyrowave-cpu`). A player
    /// from before the split does not, and its plain `pyrowave` also takes a
    /// CPU PyroWave encoder, as it did then.
    pub knows_pyrowave_cpu: bool,
    /// The player composites [`RECORD_PATCH`] tiles (`knows dirty-rects`).
    pub knows_dirty_rects: bool,
    /// The player can open a 4:4:4 PyroWave decoder (`knows yuv-444`).
    pub knows_yuv_444: bool,
    /// Picture look the host paints before it encodes. An offer without the
    /// line is [`Look::Off`].
    pub look: Look,
    /// Ordered or error-diffusion pattern. `None` is an older offer: the host
    /// uses Bayer 4×4.
    pub dither: Option<Dither>,
    /// Source pixels per output pixel, 1–32. `None` is an older offer: the host
    /// uses the look's default block size.
    pub pixel_size: Option<u8>,
    /// Dither strength 0–100. `None` is an older offer: the host uses 60.
    pub dither_strength: Option<u8>,
    /// Picture quality, 1–100. Zero lets the host use [`DEFAULT_QUALITY`].
    /// MJPEG uses this as JPEG quality. MJ2K maps it to a JPEG 2000 byte
    /// budget. Other codecs scale their default bitrate.
    pub quality: u8,
    /// The player asks the host to skip unchanged pictures and, with MJPEG or
    /// MJ2K, send dirty tiles. `None` is an older offer with no `dirty-rects`
    /// line: the host keeps its own setting.
    pub want_dirty_rects: Option<bool>,
    /// PyroWave colour format. `None` is an older offer: the host keeps 4:2:0 8-bit.
    pub yuv: Option<YuvFormat>,
    /// Pictures per second. An older offer with no `fps` line is [`FpsWish::Auto`].
    pub fps: FpsWish,
    /// Stereo-equivalent audio bitrate in kbit/s. Zero is Auto (96 stereo).
    pub audio_bitrate_kbps: u32,
    /// Max channels the player wants. Zero is Auto (2-channel stereo). The host
    /// caps at capture and does not upmix.
    pub audio_channels: u16,
    /// Watch without injecting keyboard, mouse, or pads (`view-only 1`).
    pub view_only: bool,
    /// Take the room: kick everyone else and refuse later joins (`exclusive 1`).
    pub exclusive: bool,
    /// Queue a keyframe. If none arrives in a few seconds the host sends an
    /// I-frame on every copy of this stream (`keyframe 1`).
    pub want_keyframe: bool,
    /// Carrier the player asked for (`carrier N`). `None` is an older offer:
    /// the host stays on QUIC streams.
    pub carrier: Option<u8>,
}

/// Format the host encodes, given the player's request and whether it can
/// open a 4:4:4 decoder. An older offer with no `yuv` line stays 4:2:0 8-bit.
pub fn pick_pyrowave_yuv(offer: &Offer) -> YuvFormat {
    let want = offer.yuv.unwrap_or(YuvFormat::YUV420P8);
    if want.needs_444_decoder() && !offer.knows_yuv_444 {
        YuvFormat {
            chroma: YuvChroma::Yuv420,
            depth: want.depth,
        }
    } else {
        want
    }
}

impl Offer {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("eqneg {}\n", self.version);
        let video: Vec<&str> = self.video.iter().map(|row| row.token()).collect();
        let audio: Vec<&str> = self.audio.iter().map(|row| row.token()).collect();
        let _ = writeln!(out, "video {}", video.join(" "));
        let _ = writeln!(out, "audio {}", audio.join(" "));
        let _ = writeln!(out, "max {}", self.max.text());
        let _ = writeln!(out, "bitrate {}", self.bitrate_kbps);
        if !self.failed.is_empty() {
            let failed: Vec<&str> = self.failed.iter().map(|row| row.token()).collect();
            let _ = writeln!(out, "failed {}", failed.join(" "));
        }
        let mut knows = Vec::new();
        if self.knows_pyrowave_cpu {
            knows.push("pyrowave-cpu");
        }
        if self.knows_dirty_rects {
            knows.push("dirty-rects");
        }
        if self.knows_yuv_444 {
            knows.push("yuv-444");
        }
        if !knows.is_empty() {
            let _ = writeln!(out, "knows {}", knows.join(" "));
        }
        let _ = writeln!(out, "look {}", self.look.token());
        if let Some(dither) = self.dither {
            let _ = writeln!(out, "dither {}", dither.token());
        }
        if let Some(pixel_size) = self.pixel_size {
            let _ = writeln!(out, "pixel-size {pixel_size}");
        }
        if let Some(strength) = self.dither_strength {
            let _ = writeln!(out, "dither-strength {strength}");
        }
        let _ = writeln!(out, "quality {}", self.quality);
        if let Some(want) = self.want_dirty_rects {
            let _ = writeln!(out, "dirty-rects {}", if want { 1 } else { 0 });
        }
        if let Some(yuv) = self.yuv {
            let _ = writeln!(out, "yuv {}", yuv.token());
        }
        let _ = writeln!(out, "fps {}", self.fps.token());
        let _ = writeln!(out, "audio-bitrate {}", self.audio_bitrate_kbps);
        let _ = writeln!(out, "audio-channels {}", self.audio_channels);
        if self.view_only {
            let _ = writeln!(out, "view-only 1");
        }
        if self.exclusive {
            let _ = writeln!(out, "exclusive 1");
        }
        if self.want_keyframe {
            let _ = writeln!(out, "keyframe 1");
        }
        if let Some(carrier) = self.carrier {
            let _ = writeln!(out, "carrier {carrier}");
        }
        out.into_bytes()
    }

    /// Codec lists, limits, and failures match. Picture look, dither, and
    /// pixel size may differ so the host can keep the encoder it already
    /// opened. Quality and the dirty-rect request reopen it: GOP and
    /// skip-unchanged follow those flags. View-only, exclusive, and a
    /// keyframe pulse do not reopen it.
    pub fn same_codecs(&self, other: &Offer) -> bool {
        let mut other = other.clone();
        other.look = self.look;
        other.dither = self.dither;
        other.pixel_size = self.pixel_size;
        other.dither_strength = self.dither_strength;
        other.view_only = self.view_only;
        other.exclusive = self.exclusive;
        other.want_keyframe = self.want_keyframe;
        other.carrier = self.carrier;
        *self == other
    }

    pub fn decode(bytes: &[u8]) -> Result<Offer, String> {
        let lines = parse_lines(bytes)?;
        let mut offer = Offer {
            version: lines.version,
            video: Vec::new(),
            audio: Vec::new(),
            max: Limit::NONE,
            bitrate_kbps: 0,
            failed: Vec::new(),
            knows_pyrowave_cpu: false,
            knows_dirty_rects: false,
            knows_yuv_444: false,
            look: Look::Off,
            dither: None,
            pixel_size: None,
            dither_strength: None,
            quality: 0,
            want_dirty_rects: None,
            yuv: None,
            fps: FpsWish::Auto,
            audio_bitrate_kbps: 0,
            audio_channels: 0,
            view_only: false,
            exclusive: false,
            want_keyframe: false,
            carrier: None,
        };
        for (key, words) in &lines.rows {
            match key.as_str() {
                "video" => offer.video = dedup(words.iter().filter_map(|w| Video::from_token(w))),
                "audio" => offer.audio = dedup(words.iter().filter_map(|w| Audio::from_token(w))),
                "max" => {
                    offer.max = words
                        .first()
                        .and_then(|w| Limit::parse(w))
                        .unwrap_or_default()
                }
                "bitrate" => {
                    offer.bitrate_kbps = words.first().and_then(|w| w.parse().ok()).unwrap_or(0)
                }
                "failed" => offer.failed = dedup(words.iter().filter_map(|w| Video::from_token(w))),
                "knows" => {
                    offer.knows_pyrowave_cpu |= words.contains(&"pyrowave-cpu");
                    offer.knows_dirty_rects |= words.contains(&"dirty-rects");
                    offer.knows_yuv_444 |= words.contains(&"yuv-444");
                }
                "look" => {
                    offer.look = words
                        .first()
                        .and_then(|word| Look::from_token(word))
                        .unwrap_or(Look::Off);
                }
                "dither" => {
                    offer.dither = words.first().and_then(|word| Dither::from_token(word));
                }
                "pixel-size" => {
                    offer.pixel_size = words
                        .first()
                        .and_then(|w| w.parse().ok())
                        .and_then(|n| (1..=32).contains(&n).then_some(n));
                }
                "dither-strength" => {
                    offer.dither_strength = words
                        .first()
                        .and_then(|w| w.parse().ok())
                        .filter(|n: &u8| *n <= 100);
                }
                "quality" => {
                    offer.quality = words.first().and_then(|w| w.parse().ok()).unwrap_or(0)
                }
                "dirty-rects" => {
                    offer.want_dirty_rects = Some(parse_flag(words.first().copied().unwrap_or("1")))
                }
                "yuv" => offer.yuv = words.first().copied().and_then(YuvFormat::from_token),
                "fps" => {
                    offer.fps = FpsWish::from_token(words.first().copied().unwrap_or("auto"))
                }
                "audio-bitrate" => {
                    offer.audio_bitrate_kbps =
                        words.first().and_then(|w| w.parse().ok()).unwrap_or(0)
                }
                "audio-channels" => {
                    offer.audio_channels = words
                        .first()
                        .and_then(|w| w.parse().ok())
                        .map(|n: u16| n.min(AUDIO_CHANNELS_MAX))
                        .unwrap_or(0)
                }
                "view-only" => {
                    offer.view_only = parse_flag(words.first().copied().unwrap_or("1"))
                }
                "exclusive" => {
                    offer.exclusive = parse_flag(words.first().copied().unwrap_or("1"))
                }
                "keyframe" => {
                    offer.want_keyframe = parse_flag(words.first().copied().unwrap_or("1"))
                }
                "carrier" => {
                    offer.carrier = words.first().and_then(|word| word.parse().ok()).filter(|id: &u8| {
                        (1..=crate::carrier::MAX_CARRIERS as u8).contains(id)
                    });
                }
                _ => {}
            }
        }
        Ok(offer)
    }
}

/// First record of `EVI2` or `EAU2`, and any later codec switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamConfig {
    pub version: u32,
    /// Set on `EVI2`.
    pub video: Option<Video>,
    /// Set on `EAU2`.
    pub audio: Option<Audio>,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Why the codec changed, when it did.
    pub reason: String,
    pub host: HostCaps,
    /// PyroWave colour format. Missing on an older config is 4:2:0 8-bit.
    pub yuv: YuvFormat,
    /// How the host handles unchanged pictures. Missing on an older config is
    /// [`DirtyRectsMode::Off`].
    pub dirty_rects: DirtyRectsMode,
    /// Carrier the host will use. Missing on an older config is QUIC streams.
    pub carrier: Option<u8>,
}

impl StreamConfig {
    pub fn video(video: Video, encoder: &str, width: u32, height: u32, fps: u32) -> Self {
        StreamConfig {
            version: VERSION,
            video: Some(video),
            audio: None,
            encoder: encoder.to_string(),
            width,
            height,
            fps,
            bitrate_kbps: 0,
            sample_rate: 0,
            channels: 0,
            reason: String::new(),
            host: HostCaps::default(),
            yuv: YuvFormat::YUV420P8,
            dirty_rects: DirtyRectsMode::Off,
            carrier: None,
        }
    }

    pub fn audio(audio: Audio, encoder: &str, sample_rate: u32, channels: u16) -> Self {
        StreamConfig {
            version: VERSION,
            video: None,
            audio: Some(audio),
            encoder: encoder.to_string(),
            width: 0,
            height: 0,
            fps: 0,
            bitrate_kbps: 0,
            sample_rate,
            channels,
            reason: String::new(),
            host: HostCaps::default(),
            yuv: YuvFormat::YUV420P8,
            dirty_rects: DirtyRectsMode::Off,
            carrier: None,
        }
    }

    /// Record payload, with the leading [`RECORD_CONFIG`] byte.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("eqneg {}\n", self.version);
        if let Some(video) = self.video {
            let _ = writeln!(out, "video {}", video.token());
            let _ = writeln!(out, "size {}x{}", self.width, self.height);
            let _ = writeln!(out, "fps {}", self.fps);
        }
        if let Some(audio) = self.audio {
            let _ = writeln!(out, "audio {}", audio.token());
            let _ = writeln!(out, "rate {}", self.sample_rate);
            let _ = writeln!(out, "channels {}", self.channels);
        }
        let _ = writeln!(out, "encoder {}", self.encoder);
        let _ = writeln!(out, "bitrate {}", self.bitrate_kbps);
        if self.video.is_some() && self.yuv != YuvFormat::YUV420P8 {
            let _ = writeln!(out, "yuv {}", self.yuv.bitstream().token());
        }
        if self.video.is_some() {
            let _ = writeln!(out, "dirty-rects {}", self.dirty_rects.token());
        }
        if let Some(carrier) = self.carrier {
            let _ = writeln!(out, "carrier {carrier}");
        }
        if !self.reason.is_empty() {
            let _ = writeln!(out, "reason {}", self.reason.replace('\n', " "));
        }
        if !self.host.video.is_empty() || !self.host.audio.is_empty() {
            self.host.write_lines(&mut out);
        }
        let mut bytes = vec![RECORD_CONFIG];
        bytes.extend_from_slice(out.as_bytes());
        bytes
    }

    /// Decode a record payload that starts with [`RECORD_CONFIG`].
    pub fn decode(bytes: &[u8]) -> Result<StreamConfig, String> {
        let body = match bytes.split_first() {
            Some((&RECORD_CONFIG, body)) => body,
            _ => return Err("not a config record".into()),
        };
        let lines = parse_lines(body)?;
        let mut config = StreamConfig {
            version: lines.version,
            video: None,
            audio: None,
            encoder: String::new(),
            width: 0,
            height: 0,
            fps: 0,
            bitrate_kbps: 0,
            sample_rate: 0,
            channels: 0,
            reason: String::new(),
            host: HostCaps::default(),
            yuv: YuvFormat::YUV420P8,
            dirty_rects: DirtyRectsMode::Off,
            carrier: None,
        };
        for (key, words) in &lines.rows {
            let first = words.first().copied().unwrap_or("");
            match key.as_str() {
                "video" => config.video = Video::from_token(first),
                "audio" => config.audio = Audio::from_token(first),
                "size" => {
                    if let Some((w, h)) = first.split_once('x') {
                        config.width = w.parse().unwrap_or(0);
                        config.height = h.parse().unwrap_or(0);
                    }
                }
                "fps" => config.fps = first.parse().unwrap_or(0),
                "rate" => config.sample_rate = first.parse().unwrap_or(0),
                "channels" => config.channels = first.parse().unwrap_or(0),
                "encoder" => config.encoder = first.to_string(),
                "bitrate" => config.bitrate_kbps = first.parse().unwrap_or(0),
                "reason" => config.reason = words.join(" "),
                "yuv" => {
                    if let Some(yuv) = YuvFormat::from_token(first) {
                        config.yuv = yuv.bitstream();
                    }
                }
                "dirty-rects" => {
                    if let Some(mode) = DirtyRectsMode::from_token(first) {
                        config.dirty_rects = mode;
                    }
                }
                "carrier" => {
                    config.carrier = first.parse().ok().filter(|id: &u8| {
                        (1..=crate::carrier::MAX_CARRIERS as u8).contains(id)
                    });
                }
                other => config.host.read_line(other, words),
            }
        }
        if config.video.is_none() && config.audio.is_none() {
            return Err("config names no codec this build knows".into());
        }
        Ok(config)
    }
}

struct Lines<'a> {
    version: u32,
    rows: Vec<(String, Vec<&'a str>)>,
}

fn parse_lines(bytes: &[u8]) -> Result<Lines<'_>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "negotiation record is not UTF-8")?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let head: Vec<&str> = lines.next().unwrap_or("").split_whitespace().collect();
    let version = match head.as_slice() {
        ["eqneg", version, ..] => version
            .parse::<u32>()
            .map_err(|_| format!("bad negotiation version {version}"))?,
        _ => return Err("negotiation record has no eqneg line".into()),
    };
    if version == 0 {
        return Err("negotiation version 0".into());
    }
    let rows = lines
        .map(|line| {
            let mut words = line.split_whitespace();
            let key = words.next().unwrap_or("").to_string();
            (key, words.collect())
        })
        .collect();
    Ok(Lines { version, rows })
}

fn dedup<T: PartialEq>(items: impl Iterator<Item = T>) -> Vec<T> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// What the player builds an offer from.
#[derive(Clone, Debug, Default)]
pub struct OfferInput<'a> {
    /// Settings order, top first.
    pub video_order: &'a [Video],
    pub audio_order: &'a [Audio],
    /// A per-computer or Global pin, tried first.
    pub video_pin: Option<Video>,
    pub audio_pin: Option<Audio>,
    /// Bitstreams this player decodes. MJPEG is added when missing.
    pub decodes: &'a [VideoCodec],
    /// Audio this player decodes. PCM is added when missing.
    pub audio_decodes: &'a [Audio],
    pub max: Limit,
    pub bitrate_kbps: u32,
    pub quality: u8,
    /// Skip unchanged pictures and send MJPEG/MJ2K dirty tiles when the host
    /// can. Written as `dirty-rects 0` or `dirty-rects 1`.
    pub want_dirty_rects: bool,
    /// PyroWave colour format. Default is 4:4:4 8-bit.
    pub yuv: YuvFormat,
    /// Pictures per second. Default is Auto (15 software / 30 hardware).
    pub fps: FpsWish,
    /// Stereo-equivalent audio bitrate in kbit/s. Zero is Auto (96 stereo).
    pub audio_bitrate_kbps: u32,
    /// Max channels. Zero is Auto (stereo).
    pub audio_channels: u16,
    /// Watch without injecting input.
    pub view_only: bool,
    /// Take the room exclusively.
    pub exclusive: bool,
    /// `carrier N` on the offer. `None` leaves the line off (QUIC streams).
    pub carrier: Option<u8>,
}

/// Pin first, then the Settings order, then anything left over, keeping only
/// what this player decodes. MJPEG and PCM always end the lists so an offer
/// can never come up empty.
pub fn build_offer(input: &OfferInput<'_>) -> Offer {
    let decodes =
        |row: &Video| row.codec() == VideoCodec::Mjpeg || input.decodes.contains(&row.codec());
    let mut video: Vec<Video> = Vec::new();
    for row in input.video_pin.iter().chain(input.video_order.iter()) {
        if decodes(row) && !video.contains(row) {
            video.push(*row);
        }
    }
    if !video.contains(&Video::Mjpeg) {
        video.push(Video::Mjpeg);
    }
    let audio_ok = |row: &Audio| *row == Audio::Pcm || input.audio_decodes.contains(row);
    let mut audio: Vec<Audio> = Vec::new();
    for row in input.audio_pin.iter().chain(input.audio_order.iter()) {
        if audio_ok(row) && !audio.contains(row) {
            audio.push(*row);
        }
    }
    if !audio.contains(&Audio::Pcm) {
        audio.push(Audio::Pcm);
    }
    Offer {
        version: VERSION,
        video,
        audio,
        max: input.max,
        bitrate_kbps: input.bitrate_kbps,
        failed: Vec::new(),
        knows_pyrowave_cpu: true,
        knows_dirty_rects: true,
        knows_yuv_444: true,
        look: Look::Off,
        dither: None,
        pixel_size: None,
        dither_strength: None,
        quality: input.quality,
        want_dirty_rects: Some(input.want_dirty_rects),
        yuv: Some(input.yuv),
        fps: input.fps,
        audio_bitrate_kbps: input.audio_bitrate_kbps,
        audio_channels: input.audio_channels,
        view_only: input.view_only,
        exclusive: input.exclusive,
        want_keyframe: true,
        carrier: input.carrier,
    }
}

fn parse_flag(word: &str) -> bool {
    !matches!(word, "0" | "off" | "false" | "no")
}

/// A row that was passed over, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skip<T> {
    pub row: T,
    pub reason: &'static str,
}

/// The host's pick for one stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice<T> {
    pub row: T,
    /// The encoder name, empty for PCM.
    pub encoder: String,
    pub skipped: Vec<Skip<T>>,
    /// Nothing in the offer fit, so this is the MJPEG/PCM floor.
    pub fallback: bool,
}

/// First offered row the host encodes, skipping `excluded` (encoders that
/// already failed this session, or rows the player said it failed to decode)
/// and encoders whose limit is smaller than `frame`.
pub fn choose_video(
    offer: &Offer,
    host: &HostCaps,
    excluded: &[Video],
    frame: Option<(u32, u32)>,
) -> Choice<Video> {
    let mut skipped = Vec::new();
    for row in &offer.video {
        if excluded.contains(row) || offer.failed.contains(row) {
            skipped.push(Skip {
                row: *row,
                reason: "failed earlier in this session",
            });
            continue;
        }
        let cap = host.encodes(*row).or_else(|| {
            // An old player's plain `pyrowave` is served by a CPU encoder too,
            // under the token it knows.
            if *row == Video::Pyrowave && !offer.knows_pyrowave_cpu {
                host.encodes(Video::PyrowaveCpu)
            } else {
                None
            }
        });
        let Some(cap) = cap else {
            skipped.push(Skip {
                row: *row,
                reason: "the host has no such encoder",
            });
            continue;
        };
        if let Some((w, h)) = frame {
            if !cap.limit.fits(w, h) {
                skipped.push(Skip {
                    row: *row,
                    reason: "the picture is larger than the encoder takes",
                });
                continue;
            }
        }
        return Choice {
            row: *row,
            encoder: cap.encoder.clone(),
            skipped,
            fallback: false,
        };
    }
    Choice {
        row: Video::Mjpeg,
        encoder: "jpeg".into(),
        skipped,
        fallback: true,
    }
}

/// First offered audio codec the host encodes.
pub fn choose_audio(offer: &Offer, host: &HostCaps, excluded: &[Audio]) -> Choice<Audio> {
    let mut skipped = Vec::new();
    for row in &offer.audio {
        if excluded.contains(row) {
            skipped.push(Skip {
                row: *row,
                reason: "failed earlier in this session",
            });
            continue;
        }
        if *row == Audio::Pcm || host.audio.contains(row) {
            let encoder = match row {
                Audio::Opus => "libopus",
                Audio::Aac => "aac",
                Audio::Pcm => "pcm",
            };
            return Choice {
                row: *row,
                encoder: encoder.into(),
                skipped,
                fallback: false,
            };
        }
        skipped.push(Skip {
            row: *row,
            reason: "the host has no such encoder",
        });
    }
    Choice {
        row: Audio::Pcm,
        encoder: "pcm".into(),
        skipped,
        fallback: true,
    }
}

/// What a session that did not negotiate carries.
pub fn legacy_pair() -> (Video, Audio) {
    (Video::Mjpeg, Audio::Pcm)
}

/// Codec pair for a session, given whether the peer agreed on [`ALPN_NEGOTIATE`].
/// An old peer cannot negotiate, so it is MJPEG and PCM whatever the offer says.
pub fn session_pair(negotiates: bool, offer: &Offer, host: &HostCaps) -> (Video, Audio) {
    if !negotiates {
        return legacy_pair();
    }
    (
        choose_video(offer, host, &[], None).row,
        choose_audio(offer, host, &[]).row,
    )
}

/// What a player expects before connecting: the same rule the host applies,
/// run on announced or remembered host encoders. `None` host caps means the
/// host never announced any, which predicts the legacy pair.
pub fn predict(offer: &Offer, host: Option<&HostCaps>) -> (Video, Audio) {
    match host {
        Some(host) => session_pair(true, offer, host),
        None => legacy_pair(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_caps_text_round_trip() {
        let caps = host(&[Video::Pyrowave, Video::H264Sw], &[Audio::Opus]);
        let back = HostCaps::from_text(&caps.to_text()).expect("caps");
        assert_eq!(back, caps);
        assert!(HostCaps::from_text("").is_none());
        assert!(HostCaps::from_text("host-video\nhost-audio pcm\n").is_none());
    }

    fn cap(video: Video, encoder: &str) -> EncodeCap {
        EncodeCap {
            video,
            encoder: encoder.into(),
            limit: Limit::NONE,
        }
    }

    fn host(video: &[Video], audio: &[Audio]) -> HostCaps {
        let mut caps = HostCaps::baseline();
        for row in video {
            if *row != Video::Mjpeg {
                caps.video.push(cap(*row, row.token()));
            }
        }
        for row in audio {
            if !caps.audio.contains(row) {
                caps.audio.push(*row);
            }
        }
        caps
    }

    fn all_decodes() -> Vec<VideoCodec> {
        vec![
            VideoCodec::Av1,
            VideoCodec::H265,
            VideoCodec::H264,
            VideoCodec::Pyrowave,
            VideoCodec::JpegXs,
            VideoCodec::OpenApv,
            VideoCodec::Mj2k,
            VideoCodec::Mjpeg,
        ]
    }

    fn offer(order: &[Video], decodes: &[VideoCodec]) -> Offer {
        build_offer(&OfferInput {
            video_order: order,
            audio_order: &Audio::ALL,
            decodes,
            audio_decodes: &[Audio::Opus],
            ..OfferInput::default()
        })
    }

    #[test]
    fn intersection_picks_first_both_support() {
        let offer = offer(&Video::ALL, &all_decodes());
        let host = host(&[Video::Pyrowave, Video::H264Sw], &[]);
        let pick = choose_video(&offer, &host, &[], None);
        assert_eq!(pick.row, Video::Pyrowave);
        assert!(!pick.fallback);
        let reasons: Vec<Video> = pick.skipped.iter().map(|s| s.row).collect();
        assert_eq!(reasons, vec![Video::Av1Hw, Video::H265Hw, Video::H264Hw]);
    }

    #[test]
    fn cpu_pyrowave_is_its_own_row_and_last_by_default() {
        assert_eq!(Video::from_token("pyrowave-cpu"), Some(Video::PyrowaveCpu));
        assert_eq!(Video::PyrowaveCpu.codec(), VideoCodec::Pyrowave);
        assert_eq!(Video::PyrowaveCpu.name(), "PyroWave (CPU)");
        assert_eq!(Video::Pyrowave.name(), "PyroWave (GPU)");
        let pos = |v: Video| Video::ALL.iter().position(|x| *x == v).unwrap();
        assert!(pos(Video::PyrowaveCpu) > pos(Video::Mjpeg));
        assert!(pos(Video::PyrowaveCpu) > pos(Video::H264Sw));
        let caps = host(&[Video::PyrowaveCpu, Video::H264Sw], &[Audio::Opus]);
        let back = HostCaps::from_text(&caps.to_text()).unwrap();
        assert!(back.encodes(Video::PyrowaveCpu).is_some());
        assert!(back.encodes(Video::Pyrowave).is_none());
    }

    #[test]
    fn auto_does_not_pick_cpu_pyrowave_but_a_pin_does() {
        let host = host(&[Video::PyrowaveCpu, Video::H264Sw], &[]);
        let auto = offer(&Video::ALL, &all_decodes());
        assert_eq!(choose_video(&auto, &host, &[], None).row, Video::Mjpeg);
        let pinned = build_offer(&OfferInput {
            video_order: &Video::ALL,
            video_pin: Some(Video::PyrowaveCpu),
            decodes: &all_decodes(),
            ..OfferInput::default()
        });
        assert_eq!(
            choose_video(&pinned, &host, &[], None).row,
            Video::PyrowaveCpu
        );
        // Asking for GPU PyroWave does not get the CPU one.
        let gpu = offer(&[Video::Pyrowave, Video::Mjpeg], &all_decodes());
        assert_eq!(choose_video(&gpu, &host, &[], None).row, Video::Mjpeg);
    }

    #[test]
    fn an_old_player_still_gets_pyrowave_from_a_cpu_host() {
        let old =
            Offer::decode(b"eqneg 1\nvideo pyrowave mjpeg\naudio opus pcm\nmax 0x0@0\nbitrate 0\n")
                .unwrap();
        assert!(!old.knows_pyrowave_cpu);
        let host = host(&[Video::PyrowaveCpu], &[]);
        let pick = choose_video(&old, &host, &[], None);
        assert_eq!(pick.row, Video::Pyrowave);
        assert_eq!(old.look, Look::Off);
        assert_eq!(
            StreamConfig::video(pick.row, &pick.encoder, 64, 64, 30).encode()[1..]
                .starts_with(b"eqneg 1\nvideo pyrowave\n"),
            true
        );
    }

    #[test]
    fn look_round_trips_and_a_missing_line_stays_off() {
        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        assert_eq!(offer.look, Look::Off);
        offer.look = Look::Pico8;
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("look pico8\n"), "{text}");
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.look, Look::Pico8);
        assert!(back.same_codecs(&offer));
        let mut other = offer.clone();
        other.look = Look::GameBoy;
        assert!(offer.same_codecs(&other));
        other.bitrate_kbps = offer.bitrate_kbps + 1;
        assert!(!offer.same_codecs(&other));
        other.bitrate_kbps = offer.bitrate_kbps;
        other.quality = 7;
        assert!(!offer.same_codecs(&other));
        let unknown = Offer::decode(b"eqneg 1\nvideo mjpeg\naudio pcm\nlook crt\n").unwrap();
        assert_eq!(unknown.look, Look::Off);
        assert_eq!(unknown.dither, None);
        assert_eq!(unknown.pixel_size, None);
        assert_eq!(unknown.dither_strength, None);
        assert_eq!(unknown.quality, 0);
        assert_eq!(unknown.want_dirty_rects, None);
    }

    #[test]
    fn dither_and_pixel_size_round_trip_and_leave_the_encoder() {
        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        offer.look = Look::Pico8;
        offer.dither = Some(Dither::BlueNoise);
        offer.pixel_size = Some(8);
        offer.dither_strength = Some(70);
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("dither bluenoise\n"), "{text}");
        assert!(text.contains("pixel-size 8\n"), "{text}");
        assert!(text.contains("dither-strength 70\n"), "{text}");
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.dither, Some(Dither::BlueNoise));
        assert_eq!(back.pixel_size, Some(8));
        assert_eq!(back.dither_strength, Some(70));
        assert!(offer.same_codecs(&back));
        let mut other = offer.clone();
        other.dither = Some(Dither::FloydSteinberg);
        other.pixel_size = Some(4);
        assert!(offer.same_codecs(&other));
    }

    #[test]
    fn dirty_rects_request_round_trips_and_restarts_the_encoder() {
        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        assert_eq!(offer.want_dirty_rects, Some(false));
        offer.want_dirty_rects = Some(true);
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("dirty-rects 1\n"), "{text}");
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.want_dirty_rects, Some(true));
        assert!(offer.same_codecs(&back));
        let mut other = offer.clone();
        other.want_dirty_rects = Some(false);
        assert!(!offer.same_codecs(&other));
        other.want_dirty_rects = None;
        assert!(!offer.same_codecs(&other));
        let old = Offer::decode(b"eqneg 1\nvideo mjpeg\naudio pcm\n").unwrap();
        assert_eq!(old.want_dirty_rects, None);
        assert_eq!(old.fps, FpsWish::Auto);
        assert!(!parse_flag("0"));
        assert!(parse_flag("1"));
        assert!(!parse_flag("off"));
    }

    #[test]
    fn view_only_exclusive_and_keyframe_round_trip_without_reopening() {
        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        assert!(offer.want_keyframe);
        assert!(!offer.view_only);
        assert!(!offer.exclusive);
        offer.view_only = true;
        offer.exclusive = true;
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("view-only 1\n"), "{text}");
        assert!(text.contains("exclusive 1\n"), "{text}");
        assert!(text.contains("keyframe 1\n"), "{text}");
        let back = Offer::decode(&offer.encode()).unwrap();
        assert!(back.view_only);
        assert!(back.exclusive);
        assert!(back.want_keyframe);
        assert!(offer.same_codecs(&back));
        let mut other = offer.clone();
        other.view_only = false;
        other.exclusive = false;
        other.want_keyframe = false;
        assert!(offer.same_codecs(&other));
        let old = Offer::decode(b"eqneg 1\nvideo mjpeg\naudio pcm\n").unwrap();
        assert!(!old.view_only);
        assert!(!old.exclusive);
        assert!(!old.want_keyframe);
    }

    #[test]
    fn fps_wish_round_trips_and_restarts_the_encoder() {
        assert_eq!(FpsWish::Auto.resolve(false, Some(144)), SOFTWARE_FPS);
        assert_eq!(FpsWish::Auto.resolve(true, Some(144)), HARDWARE_FPS);
        assert_eq!(FpsWish::Match.resolve(true, Some(144)), 144);
        assert_eq!(FpsWish::Match.resolve(false, None), SOFTWARE_FPS);
        assert_eq!(FpsWish::Fixed(60).resolve(false, None), 60);
        assert_eq!(FpsWish::from_token("match"), FpsWish::Match);
        assert_eq!(FpsWish::from_token("120"), FpsWish::Fixed(120));
        assert_eq!(session_fps_wish(Some(FpsWish::Auto), FpsWish::Fixed(60)), FpsWish::Fixed(60));
        assert_eq!(session_fps_wish(Some(FpsWish::Fixed(120)), FpsWish::Auto), FpsWish::Fixed(120));

        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        assert_eq!(offer.fps, FpsWish::Auto);
        offer.fps = FpsWish::Fixed(60);
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("fps 60\n"), "{text}");
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.fps, FpsWish::Fixed(60));
        assert!(offer.same_codecs(&back));
        let mut other = offer.clone();
        other.fps = FpsWish::Match;
        assert!(!offer.same_codecs(&other));
        let missing = Offer::decode(b"eqneg 1\nvideo mjpeg\naudio pcm\n").unwrap();
        assert_eq!(missing.fps, FpsWish::Auto);
    }

    #[test]
    fn stream_config_echoes_dirty_rects_mode() {
        for mode in [
            DirtyRectsMode::Patches,
            DirtyRectsMode::Pskip,
            DirtyRectsMode::Off,
        ] {
            let mut config = StreamConfig::video(Video::Mjpeg, "jpeg", 64, 48, 30);
            config.dirty_rects = mode;
            let text = String::from_utf8(config.encode()[1..].to_vec()).unwrap();
            assert!(
                text.contains(&format!("dirty-rects {}\n", mode.token())),
                "{text}"
            );
            let back = StreamConfig::decode(&config.encode()).unwrap();
            assert_eq!(back.dirty_rects, mode);
        }
        let missing = StreamConfig::decode(
            b"Ceqneg 1\nvideo mjpeg\nencoder jpeg\nsize 64x48\nfps 30\nbitrate 0\n",
        )
        .unwrap();
        assert_eq!(missing.dirty_rects, DirtyRectsMode::Off);
        let future = StreamConfig::decode(
            b"Ceqneg 1\nvideo mjpeg\nencoder jpeg\nsize 64x48\nfps 30\nbitrate 0\ndirty-rects future\n",
        )
        .unwrap();
        assert_eq!(future.video, Some(Video::Mjpeg));
        assert_eq!(future.dirty_rects, DirtyRectsMode::Off);
        assert_eq!(
            Video::Mjpeg.dirty_rects_mode(true, true),
            DirtyRectsMode::Patches
        );
        assert_eq!(
            Video::Mj2k.dirty_rects_mode(true, true),
            DirtyRectsMode::Patches
        );
        assert_eq!(
            Video::H264Sw.dirty_rects_mode(true, true),
            DirtyRectsMode::Pskip
        );
        assert_eq!(
            Video::Pyrowave.dirty_rects_mode(true, true),
            DirtyRectsMode::Pskip
        );
        assert_eq!(
            Video::Mjpeg.dirty_rects_mode(true, false),
            DirtyRectsMode::Pskip
        );
        assert_eq!(
            Video::Mjpeg.dirty_rects_mode(false, true),
            DirtyRectsMode::Off
        );
    }

    #[test]
    fn quality_round_trips_and_zero_is_the_default() {
        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        assert_eq!(offer.quality, 0);
        assert_eq!(clamp_quality(0), DEFAULT_QUALITY);
        assert_eq!(clamp_quality(7), 7);
        assert_eq!(scale_budget(40_000, 7), 7_000);
        offer.quality = 7;
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("quality 7\n"), "{text}");
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.quality, 7);
        assert_eq!(back.want_dirty_rects, Some(false));
        assert!(Video::Pyrowave.skip_bad_frame());
        assert!(Video::Mjpeg.skip_bad_frame());
        assert!(Video::Mj2k.skip_bad_frame());
        assert!(Video::JpegXs.skip_bad_frame());
        assert!(Video::OpenApv.skip_bad_frame());
        assert!(!Video::H264Hw.skip_bad_frame());
        assert!(!Video::JpegXs.patch_updates());
        assert!(!Video::OpenApv.patch_updates());
        assert_eq!(
            Video::JpegXs.dirty_rects_mode(true, true),
            DirtyRectsMode::Pskip
        );
        assert_eq!(
            Video::OpenApv.dirty_rects_mode(true, true),
            DirtyRectsMode::Pskip
        );
        assert_eq!(Video::from_token("jpegxs"), Some(Video::JpegXs));
        assert_eq!(Video::from_token("openapv"), Some(Video::OpenApv));
    }

    #[test]
    fn new_offers_say_they_know_cpu_pyrowave_and_old_hosts_ignore_the_row() {
        let offer = offer(&Video::ALL, &all_decodes());
        let back = Offer::decode(&offer.encode()).unwrap();
        assert!(back.knows_pyrowave_cpu);
        assert!(back.knows_dirty_rects);
        assert!(back.knows_yuv_444);
        assert_eq!(back.yuv, Some(YuvFormat::YUV444P8));
        assert!(String::from_utf8(offer.encode())
            .unwrap()
            .contains("knows pyrowave-cpu dirty-rects yuv-444\n"));
        assert!(String::from_utf8(offer.encode())
            .unwrap()
            .contains("yuv 444p8\n"));
        assert!(String::from_utf8(offer.encode())
            .unwrap()
            .contains("dirty-rects 0\n"));
        assert_eq!(back.video, offer.video);
        // An old host's plain pyrowave is treated as GPU.
        let old_host = HostCaps::from_text(
            "host-video mjpeg=jpeg:0x0@0 pyrowave=pyrowave:7680x4320@0\nhost-audio pcm\n",
        )
        .unwrap();
        assert_eq!(
            choose_video(&offer, &old_host, &[], None).row,
            Video::Pyrowave
        );
    }

    #[test]
    fn player_order_wins_over_host_order() {
        let order = [Video::H264Sw, Video::Pyrowave, Video::Mjpeg];
        let offer = offer(&order, &all_decodes());
        let host = host(&[Video::Pyrowave, Video::H264Sw], &[]);
        assert_eq!(choose_video(&offer, &host, &[], None).row, Video::H264Sw);
    }

    #[test]
    fn offer_drops_what_the_player_cannot_decode() {
        let offer = offer(&Video::ALL, &[VideoCodec::H264]);
        assert_eq!(
            offer.video,
            vec![Video::H264Hw, Video::Mjpeg, Video::H264Sw]
        );
        let host = host(&[Video::Pyrowave], &[]);
        assert_eq!(choose_video(&offer, &host, &[], None).row, Video::Mjpeg);
    }

    #[test]
    fn override_pin_goes_first_and_keeps_the_rest() {
        let offer = build_offer(&OfferInput {
            video_order: &Video::ALL,
            audio_order: &Audio::ALL,
            video_pin: Some(Video::H264Sw),
            audio_pin: Some(Audio::Pcm),
            decodes: &all_decodes(),
            audio_decodes: &[Audio::Opus],
            ..OfferInput::default()
        });
        assert_eq!(offer.video[0], Video::H264Sw);
        assert_eq!(offer.video.len(), Video::ALL.len());
        assert_eq!(offer.audio, vec![Audio::Pcm, Audio::Opus]);
        let host = host(&[Video::Pyrowave, Video::H264Sw], &[Audio::Opus]);
        assert_eq!(choose_video(&offer, &host, &[], None).row, Video::H264Sw);
        assert_eq!(choose_audio(&offer, &host, &[]).row, Audio::Pcm);
    }

    #[test]
    fn pin_the_player_cannot_decode_is_dropped() {
        let offer = build_offer(&OfferInput {
            video_order: &Video::ALL,
            video_pin: Some(Video::Av1Hw),
            decodes: &[VideoCodec::H264],
            ..OfferInput::default()
        });
        assert_eq!(offer.video[0], Video::H264Hw);
    }

    #[test]
    fn mjpeg_and_pcm_always_close_the_offer() {
        let offer = build_offer(&OfferInput {
            video_order: &[Video::Pyrowave],
            audio_order: &[Audio::Opus],
            decodes: &[VideoCodec::Pyrowave],
            audio_decodes: &[],
            ..OfferInput::default()
        });
        assert_eq!(offer.video, vec![Video::Pyrowave, Video::Mjpeg]);
        assert_eq!(offer.audio, vec![Audio::Pcm]);
    }

    #[test]
    fn failed_encoder_moves_to_the_next_row() {
        let offer = offer(&Video::ALL, &all_decodes());
        let host = host(&[Video::H264Hw, Video::Pyrowave, Video::H264Sw], &[]);
        assert_eq!(choose_video(&offer, &host, &[], None).row, Video::H264Hw);
        let next = choose_video(&offer, &host, &[Video::H264Hw], None);
        assert_eq!(next.row, Video::Pyrowave);
        let last = choose_video(
            &offer,
            &host,
            &[Video::H264Hw, Video::Pyrowave, Video::Mjpeg],
            None,
        );
        assert_eq!(last.row, Video::H264Sw);
    }

    #[test]
    fn player_reported_decode_failure_is_skipped() {
        let mut offer = offer(&Video::ALL, &all_decodes());
        offer.failed = vec![Video::Pyrowave];
        let back = Offer::decode(&offer.encode()).unwrap();
        let host = host(&[Video::Pyrowave], &[]);
        assert_eq!(choose_video(&back, &host, &[], None).row, Video::Mjpeg);
    }

    #[test]
    fn nothing_left_falls_back_to_mjpeg_and_pcm() {
        let offer = offer(&Video::ALL, &all_decodes());
        let host = HostCaps::baseline();
        let pick = choose_video(&offer, &host, &Video::ALL, None);
        assert_eq!(pick.row, Video::Mjpeg);
        assert!(pick.fallback);
        let audio = choose_audio(&offer, &host, &Audio::ALL);
        assert_eq!(audio.row, Audio::Pcm);
        assert!(audio.fallback);
    }

    #[test]
    fn encoder_limit_skips_large_pictures() {
        let offer = offer(&[Video::H264Sw, Video::Mjpeg], &all_decodes());
        let mut host = host(&[Video::H264Sw], &[]);
        host.video.iter_mut().for_each(|cap| {
            if cap.video == Video::H264Sw {
                cap.limit = Limit {
                    width: 1920,
                    height: 1080,
                    fps: 60,
                };
            }
        });
        assert_eq!(
            choose_video(&offer, &host, &[], Some((1280, 720))).row,
            Video::H264Sw
        );
        assert_eq!(
            choose_video(&offer, &host, &[], Some((3840, 2160))).row,
            Video::Mjpeg
        );
    }

    #[test]
    fn old_peer_gets_mjpeg_and_pcm() {
        let offer = offer(&Video::ALL, &all_decodes());
        let host = host(&[Video::Pyrowave, Video::H264Sw], &[Audio::Opus]);
        assert_eq!(
            session_pair(false, &offer, &host),
            (Video::Mjpeg, Audio::Pcm)
        );
        assert_eq!(
            session_pair(true, &offer, &host),
            (Video::Pyrowave, Audio::Opus)
        );
        assert_eq!(predict(&offer, None), (Video::Mjpeg, Audio::Pcm));
    }

    #[test]
    fn opus_needs_both_ends() {
        let offer_no_opus = build_offer(&OfferInput {
            audio_order: &Audio::ALL,
            audio_decodes: &[],
            ..OfferInput::default()
        });
        let host = host(&[], &[Audio::Opus]);
        assert_eq!(choose_audio(&offer_no_opus, &host, &[]).row, Audio::Pcm);
        let offer = offer(&Video::ALL, &all_decodes());
        assert_eq!(
            choose_audio(&offer, &HostCaps::baseline(), &[]).row,
            Audio::Pcm
        );
        assert_eq!(choose_audio(&offer, &host, &[]).row, Audio::Opus);
    }

    #[test]
    fn offer_round_trips_and_ignores_unknown_lines() {
        let mut offer = offer(&Video::ALL, &all_decodes());
        offer.max = Limit {
            width: 2560,
            height: 1440,
            fps: 120,
        };
        offer.bitrate_kbps = 20_000;
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back, offer);
        let future = b"eqneg 7\nvideo zz9-hw pyrowave mjpeg\nnewkey 1 2 3\naudio pcm\n";
        let parsed = Offer::decode(future).unwrap();
        assert_eq!(parsed.version, 7);
        assert_eq!(parsed.video, vec![Video::Pyrowave, Video::Mjpeg]);
    }

    #[test]
    fn config_round_trips_with_host_caps() {
        let mut config = StreamConfig::video(Video::H264Sw, "libx264", 1280, 720, 30);
        config.bitrate_kbps = 8000;
        config.reason = "pyrowave encoder failed".into();
        config.host = host(&[Video::Pyrowave, Video::H264Sw], &[Audio::Opus]);
        config.host.video[1].limit = Limit {
            width: 4096,
            height: 2304,
            fps: 60,
        };
        let bytes = config.encode();
        assert_eq!(bytes[0], RECORD_CONFIG);
        assert_eq!(StreamConfig::decode(&bytes).unwrap(), config);
        let audio = StreamConfig::audio(Audio::Opus, "libopus", 48_000, 2);
        assert_eq!(StreamConfig::decode(&audio.encode()).unwrap(), audio);
    }

    #[test]
    fn yuv_request_round_trips_and_old_players_stay_420() {
        let mut offer = offer(&[Video::Pyrowave, Video::Mjpeg], &all_decodes());
        assert_eq!(offer.yuv, Some(YuvFormat::YUV444P8));
        assert!(offer.knows_yuv_444);
        offer.yuv = Some(YuvFormat::YUV422P10);
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.yuv, Some(YuvFormat::YUV422P10));
        assert_eq!(pick_pyrowave_yuv(&back), YuvFormat::YUV422P10);
        assert_eq!(pick_pyrowave_yuv(&back).bitstream(), YuvFormat::YUV444P10);
        let old =
            Offer::decode(b"eqneg 1\nvideo pyrowave mjpeg\naudio pcm\nmax 0x0@0\nbitrate 0\n")
                .unwrap();
        assert_eq!(old.yuv, None);
        assert!(!old.knows_yuv_444);
        assert_eq!(pick_pyrowave_yuv(&old), YuvFormat::YUV420P8);
        let mut no_444 = back.clone();
        no_444.knows_yuv_444 = false;
        assert_eq!(pick_pyrowave_yuv(&no_444), YuvFormat::YUV420P10);
        let mut other = back.clone();
        other.yuv = Some(YuvFormat::YUV420P8);
        assert!(!back.same_codecs(&other));
        let mut config = StreamConfig::video(Video::Pyrowave, "pyrowave", 64, 64, 30);
        config.yuv = YuvFormat::YUV444P10;
        let decoded = StreamConfig::decode(&config.encode()).unwrap();
        assert_eq!(decoded.yuv, YuvFormat::YUV444P10);
        assert!(YuvFormat::from_token("nope").is_none());
    }

    #[test]
    fn garbage_is_refused() {
        assert!(Offer::decode(b"hello").is_err());
        assert!(Offer::decode(b"eqneg 0\n").is_err());
        assert!(Offer::decode(&[0xff, 0xfe]).is_err());
        assert!(StreamConfig::decode(b"Dxyz").is_err());
        assert!(StreamConfig::decode(b"Ceqneg 1\nvideo nope\n").is_err());
    }

    #[test]
    fn audio_bitrate_scales_with_channel_layout() {
        assert_eq!(scale_audio_kbps(96, 2), 96);
        assert_eq!(scale_audio_kbps(96, 3), 108);
        assert_eq!(scale_audio_kbps(96, 6), 128);
        assert_eq!(scale_audio_kbps(96, 8), 144);
        assert_eq!(scale_audio_kbps(0, 2), AUDIO_STEREO_DEFAULT_KBPS);
        assert_eq!(scale_audio_kbps(0, 6), 128);
        assert_eq!(stereo_audio_kbps(108, 3), 96);
        assert_eq!(stereo_audio_kbps(128, 6), 96);
        assert_eq!(stereo_audio_kbps(144, 8), 96);
        assert_eq!(audio_channels_or_auto(0), 2);
        assert_eq!(audio_channels_or_auto(8), 8);
        assert_eq!(audio_channels_or_auto(99), 8);
    }

    #[test]
    fn audio_bitrate_and_channels_round_trip_and_renegotiate() {
        let mut offer = offer(&[Video::Mjpeg], &[VideoCodec::Mjpeg]);
        assert_eq!(offer.audio_bitrate_kbps, 0);
        assert_eq!(offer.audio_channels, 0);
        offer.audio_bitrate_kbps = 96;
        offer.audio_channels = 6;
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.audio_bitrate_kbps, 96);
        assert_eq!(back.audio_channels, 6);
        assert!(offer.same_codecs(&back));
        let mut other = back.clone();
        other.audio_channels = 2;
        assert!(!offer.same_codecs(&other));
        other.audio_channels = 6;
        other.audio_bitrate_kbps = 128;
        assert!(!offer.same_codecs(&other));
        let old = Offer::decode(b"eqneg 1\nvideo mjpeg\naudio pcm\nbitrate 0\n").unwrap();
        assert_eq!(old.audio_bitrate_kbps, 0);
        assert_eq!(old.audio_channels, 0);
    }
}
