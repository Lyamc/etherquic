//! The player's codec choice for the offer, and the session status line.

use etherquic_media::negotiate::{self, Audio, Limit, Offer, StreamConfig, Video};
use etherquic_media::player::{PlayerEvent, Stats};

use crate::codec::{AudioId, PicturePref};

/// Settings order, any per-computer or Global pin, and limits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecChoice {
    /// Empty means the default order.
    pub video: Vec<PicturePref>,
    pub audio: Vec<AudioId>,
    pub video_pin: Option<PicturePref>,
    pub audio_pin: Option<AudioId>,
    /// Largest picture width to ask for. Zero is the host's choice.
    pub max_width: u32,
    /// Largest picture height to ask for. Zero is the host's choice.
    pub max_height: u32,
    pub bitrate_kbps: u32,
    /// Picture quality 1–100. Zero is the host default (40).
    pub quality: u8,
    /// Ask the host for dirty rectangles (`dirty-rects 1` in the offer).
    pub dirty_rects: bool,
    /// PyroWave colour format. Default is 4:4:4 8-bit.
    pub yuv: etherquic::negotiate::YuvFormat,
    /// Pictures per second. Auto is 15 software / 30 hardware on the host.
    pub fps: etherquic::negotiate::FpsWish,
    /// Stereo-equivalent audio bitrate in kbit/s. Zero is Auto (96 stereo).
    pub audio_bitrate_kbps: u32,
    /// Max audio channels. Zero is Auto (2-channel stereo).
    pub audio_channels: u16,
    /// Watch without injecting input.
    pub view_only: bool,
    /// Kick other viewers and refuse later joins.
    pub exclusive: bool,
    /// Pinned carrier. `None` omits `carrier N` from the offer.
    pub carrier: Option<u8>,
}

pub fn video_row(pref: PicturePref) -> Video {
    Video::from_token(pref.token()).unwrap_or(Video::Mjpeg)
}

pub fn audio_row(id: AudioId) -> Audio {
    Audio::from_token(id.token()).unwrap_or(Audio::Pcm)
}

impl CodecChoice {
    pub fn video_rows(&self) -> Vec<Video> {
        if self.video.is_empty() {
            Video::ALL.to_vec()
        } else {
            self.video.iter().map(|p| video_row(*p)).collect()
        }
    }

    pub fn audio_rows(&self) -> Vec<Audio> {
        if self.audio.is_empty() {
            Audio::ALL.to_vec()
        } else {
            self.audio.iter().map(|a| audio_row(*a)).collect()
        }
    }

    /// The offer, narrowed to what this computer decodes. Probes on first use.
    pub fn offer(&self) -> Offer {
        etherquic_media::player::offer_for(
            &self.video_rows(),
            &self.audio_rows(),
            self.video_pin.map(video_row),
            self.audio_pin.map(audio_row),
            Limit {
                width: self.max_width,
                height: self.max_height,
                fps: 0,
            },
            self.bitrate_kbps,
            self.quality,
            self.dirty_rects,
            self.yuv,
            self.fps,
            self.audio_bitrate_kbps,
            self.audio_channels,
            self.view_only,
            self.exclusive,
            self.carrier,
        )
    }

    /// Flags for `etherquic-play connect`, so a child process offers the same.
    pub fn args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.video.is_empty() {
            let list: Vec<&str> = self.video.iter().map(|p| p.token()).collect();
            out.extend(["--video".to_string(), list.join(",")]);
        }
        if !self.audio.is_empty() {
            let list: Vec<&str> = self.audio.iter().map(|a| a.token()).collect();
            out.extend(["--audio".to_string(), list.join(",")]);
        }
        if let Some(pin) = self.video_pin {
            out.extend(["--video-pin".to_string(), pin.token().to_string()]);
        }
        if let Some(pin) = self.audio_pin {
            out.extend(["--audio-pin".to_string(), pin.token().to_string()]);
        }
        if self.max_width > 0 {
            out.extend(["--max-width".to_string(), self.max_width.to_string()]);
        }
        if self.max_height > 0 {
            out.extend(["--max-height".to_string(), self.max_height.to_string()]);
        }
        if self.bitrate_kbps > 0 {
            out.extend(["--bitrate".to_string(), self.bitrate_kbps.to_string()]);
        }
        if self.quality > 0 {
            out.extend(["--quality".to_string(), self.quality.to_string()]);
        }
        if self.dirty_rects {
            out.push("--dirty-rects".to_string());
        }
        if self.yuv != etherquic::negotiate::YuvFormat::YUV444P8 {
            out.extend(["--yuv".to_string(), self.yuv.token().to_string()]);
        }
        if self.fps != etherquic::negotiate::FpsWish::Auto {
            out.extend(["--fps".to_string(), self.fps.token()]);
        }
        if self.audio_bitrate_kbps > 0 {
            out.extend([
                "--audio-bitrate".to_string(),
                self.audio_bitrate_kbps.to_string(),
            ]);
        }
        if self.audio_channels > 0 {
            out.extend([
                "--audio-channels".to_string(),
                self.audio_channels.to_string(),
            ]);
        }
        if self.view_only {
            out.push("--view-only".to_string());
        }
        if self.exclusive {
            out.push("--exclusive".to_string());
        }
        if let Some(carrier) = self.carrier {
            out.extend(["--carrier".to_string(), carrier.to_string()]);
        }
        out
    }

    /// Parse the comma lists [`CodecChoice::args`] writes. Unknown tokens are skipped.
    pub fn parse_video(list: &str) -> Vec<PicturePref> {
        list.split(',')
            .filter_map(|t| PicturePref::from_token(t.trim()))
            .collect()
    }

    pub fn parse_audio(list: &str) -> Vec<AudioId> {
        list.split(',')
            .filter_map(|t| AudioId::from_token(t.trim()))
            .collect()
    }
}

/// What the session status says about media.
#[derive(Clone, Debug)]
pub struct SessionLine {
    peer: String,
    negotiates: bool,
    video: Option<StreamConfig>,
    audio: Option<StreamConfig>,
    stats: Option<Stats>,
}

impl SessionLine {
    pub fn new(peer: String, negotiates: bool) -> SessionLine {
        SessionLine {
            peer,
            negotiates,
            video: None,
            audio: None,
            stats: None,
        }
    }

    /// Record an event. True when the status line should be redrawn.
    pub fn apply(&mut self, event: &PlayerEvent) -> bool {
        match event {
            PlayerEvent::Video(config) => self.video = Some(config.clone()),
            PlayerEvent::Audio(config) => self.audio = Some(config.clone()),
            PlayerEvent::Stats(stats) => self.stats = Some(stats.clone()),
            PlayerEvent::VideoFailed { .. } | PlayerEvent::AudioFailed { .. } => {}
            PlayerEvent::Cursor(_) => return false,
        }
        true
    }

    pub fn video_name(&self) -> String {
        match &self.video {
            Some(config) => {
                let name = config.video.map(Video::name).unwrap_or("?");
                let dirty = config
                    .dirty_rects
                    .status_token()
                    .map(|token| format!(" {token}"))
                    .unwrap_or_default();
                format!(
                    "{name} {}x{} ({}){dirty}",
                    config.width, config.height, config.encoder
                )
            }
            None if !self.negotiates => "MJPEG".into(),
            None => "video starting".into(),
        }
    }

    pub fn audio_name(&self) -> String {
        match &self.audio {
            Some(config) => {
                let name = config.audio.map(Audio::name).unwrap_or("?");
                if config.bitrate_kbps > 0 {
                    format!("{name} {} kbit/s", config.bitrate_kbps)
                } else {
                    format!("{name} {} Hz", config.sample_rate)
                }
            }
            None if !self.negotiates => "PCM".into(),
            None => "audio starting".into(),
        }
    }

    fn rate(&self) -> String {
        match &self.stats {
            Some(stats) => format!(" · {:.0} fps · {:.1} Mbit/s", stats.fps, stats.video_mbps),
            None => String::new(),
        }
    }

    pub fn text(&self) -> String {
        let legacy = if self.negotiates {
            ""
        } else {
            " (the host does not negotiate codecs)"
        };
        format!(
            "Viewing {} · {} · {}{}{legacy}.",
            self.peer,
            self.video_name(),
            self.audio_name(),
            self.rate()
        )
    }

    pub fn short(&self) -> String {
        format!(
            "{} · {}{}",
            self.video_name(),
            self.audio_name(),
            self.rate()
        )
    }

    /// A stderr line for logs and test scripts.
    pub fn log_line(&self, event: &PlayerEvent) -> Option<String> {
        match event {
            PlayerEvent::Video(config) => Some(format!(
                "session video {} encoder {} {}x{} {} fps {} kbit/s{}{}",
                config.video.map(Video::token).unwrap_or("?"),
                config.encoder,
                config.width,
                config.height,
                config.fps,
                config.bitrate_kbps,
                if config.reason.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", config.reason)
                },
                config
                    .carrier
                    .map(|id| format!(" carrier {id}"))
                    .unwrap_or_default()
            )),
            PlayerEvent::Audio(config) => Some(format!(
                "session audio {} encoder {} {} Hz {} ch",
                config.audio.map(Audio::token).unwrap_or("?"),
                config.encoder,
                config.sample_rate,
                config.channels
            )),
            PlayerEvent::Stats(stats) => Some(format!(
                "stats video {} {}x{} {:.1} fps {:.2} Mbit/s",
                stats.video.map(Video::token).unwrap_or("?"),
                stats.width,
                stats.height,
                stats.fps,
                stats.video_mbps
            )),
            PlayerEvent::VideoFailed { video, reason } => {
                Some(format!("decoder {} failed: {reason}", video.token()))
            }
            PlayerEvent::AudioFailed { audio, reason } => {
                Some(format!("audio decoder {} failed: {reason}", audio.token()))
            }
            PlayerEvent::Cursor(_) => None,
        }
    }
}

/// The host's announced encoders, as negotiation sees them.
pub fn host_caps_from(config: &StreamConfig) -> Option<negotiate::HostCaps> {
    (!config.host.video.is_empty()).then(|| config.host.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic_media::negotiate::DirtyRectsMode;

    #[test]
    fn session_line_shows_patches_and_pskip() {
        let mut line = SessionLine::new("peer".into(), true);
        let mut config = StreamConfig::video(Video::Mjpeg, "jpeg", 64, 48, 30);
        config.dirty_rects = DirtyRectsMode::Patches;
        assert!(line.apply(&PlayerEvent::Video(config.clone())));
        let text = line.text();
        assert!(text.contains("patches"), "{text}");
        assert!(text.contains("MJPEG"), "{text}");

        config.dirty_rects = DirtyRectsMode::Pskip;
        config.video = Some(Video::H264Sw);
        config.encoder = "libx264".into();
        line.apply(&PlayerEvent::Video(config));
        let text = line.text();
        assert!(text.contains("pskip"), "{text}");
        assert!(!text.contains("patches"), "{text}");

        let off = StreamConfig::video(Video::Mjpeg, "jpeg", 64, 48, 30);
        line.apply(&PlayerEvent::Video(off));
        let text = line.text();
        assert!(!text.contains("patches"), "{text}");
        assert!(!text.contains("pskip"), "{text}");
    }

    #[test]
    fn session_video_log_names_the_carrier() {
        let line = SessionLine::new("peer".into(), true);
        let mut config = StreamConfig::video(Video::Mjpeg, "jpeg", 64, 48, 30);
        config.carrier = Some(etherquic::carrier::ST2110);
        let text = line
            .log_line(&PlayerEvent::Video(config))
            .expect("session video line");
        assert!(text.contains("carrier 5"), "{text}");
    }

    #[test]
    fn a_pinned_carrier_is_on_the_next_offer() {
        let mut choice = CodecChoice::default();
        assert!(choice.offer().carrier.is_none());
        choice.carrier = Some(etherquic::carrier::UDP_RTP);
        let offer = choice.offer();
        assert_eq!(offer.carrier, Some(etherquic::carrier::UDP_RTP));
        let text = String::from_utf8(offer.encode()).expect("offer text");
        assert!(text.contains("carrier 3\n"), "{text}");
        choice.carrier = None;
        let text = String::from_utf8(choice.offer().encode()).expect("offer text");
        assert!(!text.contains("carrier "), "{text}");
    }
}
