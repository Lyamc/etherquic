//! One picture encoder or decoder, whichever codec was negotiated.

use crate::negotiate::{Video, VideoCodec};
use crate::{ffmpeg, jpeg, pyrowave, Frame, Sink};

/// Encoder settings the host chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeSetup {
    pub video: Video,
    /// From [`crate::negotiate::EncodeCap::encoder`].
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target in kbit/s. Zero picks the codec default.
    pub kbps: u32,
}

pub enum VideoEncoder {
    Mjpeg { sink: Sink<Vec<u8>> },
    Pyrowave { inner: pyrowave::Encoder, sink: Sink<Vec<u8>> },
    Ffmpeg(ffmpeg::Encoder),
}

impl VideoEncoder {
    /// Coded output goes to `sink`, one call per record (ffmpeg calls it from
    /// its own thread as bytes arrive).
    pub fn open(setup: &EncodeSetup, sink: Sink<Vec<u8>>) -> Result<(VideoEncoder, u32), String> {
        match setup.video {
            Video::Mjpeg => Ok((VideoEncoder::Mjpeg { sink }, 0)),
            Video::Pyrowave | Video::PyrowaveCpu => {
                let frame_bytes = if setup.kbps > 0 {
                    (setup.kbps as usize * 1000 / 8) / setup.fps.max(1) as usize
                } else {
                    pyrowave::default_frame_bytes(setup.width, setup.height)
                };
                let inner = pyrowave::Encoder::new(setup.width, setup.height, frame_bytes)?;
                let kbps = (frame_bytes.max(16 * 1024) * 8 * setup.fps.max(1) as usize / 1000) as u32;
                Ok((VideoEncoder::Pyrowave { inner, sink }, kbps))
            }
            video => {
                let name = ffmpeg::candidates(video)
                    .iter()
                    .copied()
                    .find(|name| *name == setup.encoder)
                    .ok_or_else(|| format!("no encoder {} for {}", setup.encoder, video.token()))?;
                let kbps = if setup.kbps > 0 {
                    setup.kbps
                } else {
                    ffmpeg::default_kbps(video.codec(), setup.width, setup.height, setup.fps)
                };
                let spec = ffmpeg::EncoderSpec { video, name };
                let inner = ffmpeg::Encoder::new(&spec, setup.width, setup.height, setup.fps, kbps, sink)?;
                Ok((VideoEncoder::Ffmpeg(inner), kbps))
            }
        }
    }

    pub fn push(&mut self, frame: &Frame) -> Result<bool, String> {
        match self {
            VideoEncoder::Mjpeg { sink } => Ok(sink(jpeg::encode(frame, jpeg::QUALITY)?)),
            VideoEncoder::Pyrowave { inner, sink } => Ok(sink(inner.encode(frame)?)),
            VideoEncoder::Ffmpeg(inner) => inner.push(frame).map(|_| true),
        }
    }
}

pub enum VideoDecoder {
    Mjpeg { sink: Sink<Frame> },
    Pyrowave { inner: pyrowave::Decoder, sink: Sink<Frame> },
    Ffmpeg(ffmpeg::Decoder),
}

impl VideoDecoder {
    pub fn open(video: Video, width: u32, height: u32, sink: Sink<Frame>) -> Result<VideoDecoder, String> {
        match video.codec() {
            VideoCodec::Mjpeg => Ok(VideoDecoder::Mjpeg { sink }),
            VideoCodec::Pyrowave => Ok(VideoDecoder::Pyrowave { inner: pyrowave::Decoder::new(width, height)?, sink }),
            codec => Ok(VideoDecoder::Ffmpeg(ffmpeg::Decoder::new(codec, width, height, sink)?)),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            VideoDecoder::Mjpeg { sink } => {
                sink(jpeg::decode(bytes)?);
                Ok(())
            }
            VideoDecoder::Pyrowave { inner, sink } => {
                sink(inner.decode(bytes)?);
                Ok(())
            }
            VideoDecoder::Ffmpeg(inner) => inner.push(bytes),
        }
    }
}
