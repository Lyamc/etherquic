//! One audio encoder or decoder, whichever codec was negotiated.

use crate::negotiate::Audio;
use crate::{opus, Pcm};

/// Linear resampler that keeps its phase across chunks.
pub struct Resampler {
    from: u32,
    to: u32,
    channels: usize,
    /// Position in input frames, relative to the start of `carry`.
    pos: f64,
    carry: Vec<i16>,
}

impl Resampler {
    fn new(from: u32, to: u32, channels: usize) -> Resampler {
        Resampler {
            from,
            to,
            channels,
            pos: 0.0,
            carry: Vec::new(),
        }
    }

    fn run(&mut self, input: &[i16]) -> Vec<i16> {
        if self.from == self.to {
            return input.to_vec();
        }
        self.carry.extend_from_slice(input);
        let ch = self.channels;
        let frames = self.carry.len() / ch;
        let step = self.from as f64 / self.to as f64;
        let mut out = Vec::new();
        while self.pos + 1.0 < frames as f64 {
            let i = self.pos as usize;
            let t = self.pos - i as f64;
            for c in 0..ch {
                let a = self.carry[i * ch + c] as f64;
                let b = self.carry[(i + 1) * ch + c] as f64;
                out.push((a + (b - a) * t).round() as i16);
            }
            self.pos += step;
        }
        let used = (self.pos as usize).min(frames);
        self.carry.drain(..used * ch);
        self.pos -= used as f64;
        out
    }
}

pub enum AudioEncoder {
    Pcm {
        in_channels: u16,
        channels: u16,
    },
    Opus {
        inner: opus::Encoder,
        resampler: Resampler,
        in_rate: u32,
        in_channels: u16,
        channels: u16,
    },
}

/// Cap at capture. Auto (0) is stereo. Never upmix.
pub fn capped_channels(in_channels: u16, max_channels: u16) -> u16 {
    let want = crate::negotiate::audio_channels_or_auto(max_channels);
    in_channels.min(want).max(1)
}

/// Downmix interleaved PCM to `out_ch` channels. Extra channels are dropped;
/// mono is an average of the frame.
pub fn mix_channels(samples: &[i16], in_ch: usize, out_ch: usize) -> Vec<i16> {
    if in_ch == 0 || out_ch == 0 {
        return Vec::new();
    }
    if in_ch == out_ch {
        return samples.to_vec();
    }
    let mut out = Vec::with_capacity((samples.len() / in_ch) * out_ch);
    for frame in samples.chunks_exact(in_ch) {
        if out_ch == 1 {
            let sum: i32 = frame.iter().map(|s| i32::from(*s)).sum();
            out.push((sum / in_ch as i32) as i16);
        } else {
            let take = in_ch.min(out_ch);
            out.extend_from_slice(&frame[..take]);
            if out_ch > in_ch {
                out.extend(std::iter::repeat(0i16).take(out_ch - in_ch));
            }
        }
    }
    out
}

impl AudioEncoder {
    /// Rate and channel count the stream carries.
    ///
    /// `max_channels` 0 is Auto (stereo). `stereo_kbps` 0 is Auto (96 kbps stereo,
    /// scaled to the channels actually sent). Opus is 1 or 2 channels.
    pub fn open(
        audio: Audio,
        in_rate: u32,
        in_channels: u16,
        max_channels: u16,
        stereo_kbps: u32,
    ) -> Result<(AudioEncoder, u32, u16), String> {
        let channels = capped_channels(in_channels, max_channels);
        match audio {
            Audio::Pcm => Ok((
                AudioEncoder::Pcm {
                    in_channels,
                    channels,
                },
                in_rate,
                channels,
            )),
            Audio::Opus => {
                let channels = channels.min(2);
                let kbps = crate::negotiate::scale_audio_kbps(stereo_kbps, channels);
                let bitrate = (kbps.max(1) * 1000) as i32;
                let inner = opus::Encoder::new(channels, bitrate)?;
                let resampler = Resampler::new(in_rate, opus::RATE, channels as usize);
                Ok((
                    AudioEncoder::Opus {
                        inner,
                        resampler,
                        in_rate,
                        in_channels,
                        channels,
                    },
                    opus::RATE,
                    channels,
                ))
            }
            Audio::Aac => Err("no AAC encoder in this build".into()),
        }
    }

    /// True when the next chunk has a format this encoder was not opened for.
    pub fn changed(&self, pcm: &Pcm) -> bool {
        match self {
            AudioEncoder::Pcm { in_channels, .. } => *in_channels != pcm.channels,
            AudioEncoder::Opus {
                in_rate,
                in_channels,
                ..
            } => *in_rate != pcm.sample_rate || *in_channels != pcm.channels,
        }
    }

    /// Data record payloads for this chunk.
    pub fn push(&mut self, pcm: &Pcm) -> Result<Vec<Vec<u8>>, String> {
        match self {
            AudioEncoder::Pcm {
                in_channels,
                channels,
            } => {
                let mixed = mix_channels(
                    &pcm.samples,
                    (*in_channels).max(1) as usize,
                    (*channels).max(1) as usize,
                );
                Ok(vec![Pcm {
                    sample_rate: pcm.sample_rate,
                    channels: *channels,
                    samples: mixed,
                }
                .encode()])
            }
            AudioEncoder::Opus {
                inner,
                resampler,
                in_channels,
                channels,
                ..
            } => {
                let picked = mix_channels(
                    &pcm.samples,
                    (*in_channels).max(1) as usize,
                    (*channels).max(1) as usize,
                );
                inner.push(&resampler.run(&picked))
            }
        }
    }
}

pub enum AudioDecoder {
    Pcm,
    Opus { inner: opus::Decoder, channels: u16 },
}

impl AudioDecoder {
    pub fn open(audio: Audio, channels: u16) -> Result<AudioDecoder, String> {
        match audio {
            Audio::Pcm => Ok(AudioDecoder::Pcm),
            Audio::Opus => Ok(AudioDecoder::Opus {
                inner: opus::Decoder::new(channels)?,
                channels,
            }),
            Audio::Aac => Err("no AAC decoder in this build".into()),
        }
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Result<Pcm, String> {
        match self {
            AudioDecoder::Pcm => Pcm::decode(bytes),
            AudioDecoder::Opus { inner, channels } => Ok(Pcm {
                sample_rate: opus::RATE,
                channels: *channels,
                samples: inner.decode(bytes)?,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_length_ratio() {
        let mut r = Resampler::new(44_100, 48_000, 2);
        let mut total = 0;
        for _ in 0..100 {
            total += r.run(&vec![100i16; 441 * 2]).len() / 2;
        }
        // 1 s of 44.1 kHz is 48000 frames, give or take one.
        assert!((total as i64 - 48_000).abs() <= 2, "{total}");
    }

    #[test]
    fn mix_drops_extra_channels_and_averages_to_mono() {
        let five_one: Vec<i16> = (0..6).collect();
        assert_eq!(mix_channels(&five_one, 6, 2), vec![0, 1]);
        assert_eq!(mix_channels(&five_one, 6, 6), five_one);
        let stereo = vec![10i16, 30];
        assert_eq!(mix_channels(&stereo, 2, 1), vec![20]);
        assert_eq!(capped_channels(8, 0), 2);
        assert_eq!(capped_channels(8, 6), 6);
        assert_eq!(capped_channels(2, 8), 2);
    }
}
