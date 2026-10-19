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
    Pcm,
    Opus {
        inner: opus::Encoder,
        resampler: Resampler,
        in_rate: u32,
        in_channels: u16,
        channels: u16,
    },
}

impl AudioEncoder {
    /// Rate and channel count the stream carries.
    pub fn open(
        audio: Audio,
        in_rate: u32,
        in_channels: u16,
    ) -> Result<(AudioEncoder, u32, u16), String> {
        match audio {
            Audio::Pcm => Ok((AudioEncoder::Pcm, in_rate, in_channels)),
            Audio::Opus => {
                let channels = in_channels.clamp(1, 2);
                let inner = opus::Encoder::new(channels, opus::DEFAULT_BITRATE)?;
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
            AudioEncoder::Pcm => false,
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
            AudioEncoder::Pcm => Ok(vec![pcm.encode()]),
            AudioEncoder::Opus {
                inner,
                resampler,
                in_channels,
                channels,
                ..
            } => {
                let inch = (*in_channels).max(1) as usize;
                let outch = *channels as usize;
                let picked: Vec<i16> = if inch == outch {
                    pcm.samples.clone()
                } else if outch == 1 {
                    pcm.samples
                        .chunks_exact(inch)
                        .map(|f| (f.iter().map(|s| *s as i32).sum::<i32>() / inch as i32) as i16)
                        .collect()
                } else {
                    pcm.samples
                        .chunks_exact(inch)
                        .flat_map(|f| [f[0], f[1]])
                        .collect()
                };
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
}
