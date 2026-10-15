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

/// ALPN of a session that negotiates codecs.
pub const ALPN_NEGOTIATE: &[u8] = b"etherquic/2";
/// Negotiation record version written by this build.
pub const VERSION: u32 = 1;
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

/// One row of a picture preference list. The suffix is the sender's encoder:
/// `-hw` is an encode ASIC (NVENC, VideoToolbox, AMF, Quick Sync), `-sw` the CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Video {
    Av1Hw,
    H265Hw,
    H264Hw,
    Pyrowave,
    Mjpeg,
    H264Sw,
}

/// The bitstream a [`Video`] row produces. The player decodes this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    Av1,
    H265,
    H264,
    Pyrowave,
    Mjpeg,
}

impl Video {
    pub const ALL: [Video; 6] = [
        Video::Av1Hw,
        Video::H265Hw,
        Video::H264Hw,
        Video::Pyrowave,
        Video::Mjpeg,
        Video::H264Sw,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Video::Av1Hw => "av1-hw",
            Video::H265Hw => "h265-hw",
            Video::H264Hw => "h264-hw",
            Video::Pyrowave => "pyrowave",
            Video::Mjpeg => "mjpeg",
            Video::H264Sw => "h264-sw",
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
            Video::Pyrowave => "Pyrowave",
            Video::Mjpeg => "MJPEG",
            Video::H264Sw => "H.264 (SW)",
        }
    }

    pub fn codec(self) -> VideoCodec {
        match self {
            Video::Av1Hw => VideoCodec::Av1,
            Video::H265Hw => VideoCodec::H265,
            Video::H264Hw | Video::H264Sw => VideoCodec::H264,
            Video::Pyrowave => VideoCodec::Pyrowave,
            Video::Mjpeg => VideoCodec::Mjpeg,
        }
    }

    /// An encode ASIC on the sender.
    pub fn hardware(self) -> bool {
        matches!(self, Video::Av1Hw | Video::H265Hw | Video::H264Hw)
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
        let mut caps = HostCaps { video: Vec::new(), audio: Vec::new() };
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
                self.audio = words.iter().filter_map(|word| Audio::from_token(word)).collect();
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
        out.into_bytes()
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
        };
        for (key, words) in &lines.rows {
            match key.as_str() {
                "video" => offer.video = dedup(words.iter().filter_map(|w| Video::from_token(w))),
                "audio" => offer.audio = dedup(words.iter().filter_map(|w| Audio::from_token(w))),
                "max" => {
                    offer.max = words.first().and_then(|w| Limit::parse(w)).unwrap_or_default()
                }
                "bitrate" => {
                    offer.bitrate_kbps = words.first().and_then(|w| w.parse().ok()).unwrap_or(0)
                }
                "failed" => offer.failed = dedup(words.iter().filter_map(|w| Video::from_token(w))),
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
}

/// Pin first, then the Settings order, then anything left over, keeping only
/// what this player decodes. MJPEG and PCM always end the lists so an offer
/// can never come up empty.
pub fn build_offer(input: &OfferInput<'_>) -> Offer {
    let decodes = |row: &Video| row.codec() == VideoCodec::Mjpeg || input.decodes.contains(&row.codec());
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
    }
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
            skipped.push(Skip { row: *row, reason: "failed earlier in this session" });
            continue;
        }
        let Some(cap) = host.encodes(*row) else {
            skipped.push(Skip { row: *row, reason: "the host has no such encoder" });
            continue;
        };
        if let Some((w, h)) = frame {
            if !cap.limit.fits(w, h) {
                skipped.push(Skip { row: *row, reason: "the picture is larger than the encoder takes" });
                continue;
            }
        }
        return Choice { row: *row, encoder: cap.encoder.clone(), skipped, fallback: false };
    }
    Choice { row: Video::Mjpeg, encoder: "jpeg".into(), skipped, fallback: true }
}

/// First offered audio codec the host encodes.
pub fn choose_audio(offer: &Offer, host: &HostCaps, excluded: &[Audio]) -> Choice<Audio> {
    let mut skipped = Vec::new();
    for row in &offer.audio {
        if excluded.contains(row) {
            skipped.push(Skip { row: *row, reason: "failed earlier in this session" });
            continue;
        }
        if *row == Audio::Pcm || host.audio.contains(row) {
            let encoder = match row {
                Audio::Opus => "libopus",
                Audio::Aac => "aac",
                Audio::Pcm => "pcm",
            };
            return Choice { row: *row, encoder: encoder.into(), skipped, fallback: false };
        }
        skipped.push(Skip { row: *row, reason: "the host has no such encoder" });
    }
    Choice { row: Audio::Pcm, encoder: "pcm".into(), skipped, fallback: true }
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
    (choose_video(offer, host, &[], None).row, choose_audio(offer, host, &[]).row)
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
        EncodeCap { video, encoder: encoder.into(), limit: Limit::NONE }
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
        vec![VideoCodec::Av1, VideoCodec::H265, VideoCodec::H264, VideoCodec::Pyrowave, VideoCodec::Mjpeg]
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
    fn player_order_wins_over_host_order() {
        let order = [Video::H264Sw, Video::Pyrowave, Video::Mjpeg];
        let offer = offer(&order, &all_decodes());
        let host = host(&[Video::Pyrowave, Video::H264Sw], &[]);
        assert_eq!(choose_video(&offer, &host, &[], None).row, Video::H264Sw);
    }

    #[test]
    fn offer_drops_what_the_player_cannot_decode() {
        let offer = offer(&Video::ALL, &[VideoCodec::H264]);
        assert_eq!(offer.video, vec![Video::H264Hw, Video::Mjpeg, Video::H264Sw]);
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
        assert_eq!(offer.video.len(), 6);
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
        let last = choose_video(&offer, &host, &[Video::H264Hw, Video::Pyrowave, Video::Mjpeg], None);
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
                cap.limit = Limit { width: 1920, height: 1080, fps: 60 };
            }
        });
        assert_eq!(choose_video(&offer, &host, &[], Some((1280, 720))).row, Video::H264Sw);
        assert_eq!(choose_video(&offer, &host, &[], Some((3840, 2160))).row, Video::Mjpeg);
    }

    #[test]
    fn old_peer_gets_mjpeg_and_pcm() {
        let offer = offer(&Video::ALL, &all_decodes());
        let host = host(&[Video::Pyrowave, Video::H264Sw], &[Audio::Opus]);
        assert_eq!(session_pair(false, &offer, &host), (Video::Mjpeg, Audio::Pcm));
        assert_eq!(session_pair(true, &offer, &host), (Video::Pyrowave, Audio::Opus));
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
        assert_eq!(choose_audio(&offer, &HostCaps::baseline(), &[]).row, Audio::Pcm);
        assert_eq!(choose_audio(&offer, &host, &[]).row, Audio::Opus);
    }

    #[test]
    fn offer_round_trips_and_ignores_unknown_lines() {
        let mut offer = offer(&Video::ALL, &all_decodes());
        offer.max = Limit { width: 2560, height: 1440, fps: 120 };
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
        config.host.video[1].limit = Limit { width: 4096, height: 2304, fps: 60 };
        let bytes = config.encode();
        assert_eq!(bytes[0], RECORD_CONFIG);
        assert_eq!(StreamConfig::decode(&bytes).unwrap(), config);
        let audio = StreamConfig::audio(Audio::Opus, "libopus", 48_000, 2);
        assert_eq!(StreamConfig::decode(&audio.encode()).unwrap(), audio);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(Offer::decode(b"hello").is_err());
        assert!(Offer::decode(b"eqneg 0\n").is_err());
        assert!(Offer::decode(&[0xff, 0xfe]).is_err());
        assert!(StreamConfig::decode(b"Dxyz").is_err());
        assert!(StreamConfig::decode(b"Ceqneg 1\nvideo nope\n").is_err());
    }
}
