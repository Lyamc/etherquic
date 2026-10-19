//! One picture encoder or decoder, whichever codec was negotiated.

use crate::negotiate::{scale_budget, Video, VideoCodec};
#[cfg(windows)]
use crate::pyrowave_d3d11;
use crate::{ffmpeg, jpeg, jpeg2000, pyrowave, Frame, Sink};

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
    /// 1–100 picture quality. Zero is [`crate::negotiate::DEFAULT_QUALITY`].
    pub quality: u8,
    /// PyroWave colour format. Other codecs ignore this.
    pub yuv: crate::negotiate::YuvFormat,
    /// Open temporal ffmpeg with a long GOP and skip-unchanged on the host.
    pub dirty_rects: bool,
}

pub enum VideoEncoder {
    Mjpeg {
        sink: Sink<Vec<u8>>,
        quality: u8,
    },
    Mj2k {
        sink: Sink<Vec<u8>>,
        quality: u8,
    },
    Pyrowave {
        inner: pyrowave::Encoder,
        sink: Sink<Vec<u8>>,
    },
    #[cfg(windows)]
    PyrowaveD3d11 {
        inner: pyrowave_d3d11::Encoder,
        sink: Sink<Vec<u8>>,
    },
    Ffmpeg(ffmpeg::Encoder),
}

impl VideoEncoder {
    /// Coded output goes to `sink`, one call per record (ffmpeg calls it from
    /// its own thread as bytes arrive).
    pub fn open(setup: &EncodeSetup, sink: Sink<Vec<u8>>) -> Result<(VideoEncoder, u32), String> {
        match setup.video {
            Video::Mjpeg => Ok((
                VideoEncoder::Mjpeg {
                    sink,
                    quality: crate::negotiate::clamp_quality(setup.quality),
                },
                0,
            )),
            Video::Mj2k => Ok((
                VideoEncoder::Mj2k {
                    sink,
                    quality: crate::negotiate::clamp_quality(setup.quality),
                },
                0,
            )),
            Video::Pyrowave | Video::PyrowaveCpu => {
                let frame_bytes = if setup.kbps > 0 {
                    (setup.kbps as usize * 1000 / 8) / setup.fps.max(1) as usize
                } else {
                    scale_budget(
                        pyrowave::default_frame_bytes(setup.width, setup.height) as u32,
                        setup.quality,
                    ) as usize
                };
                let kbps =
                    (frame_bytes.max(16 * 1024) * 8 * setup.fps.max(1) as usize / 1000) as u32;
                if setup.encoder == "pyrowave-d3d11" {
                    #[cfg(windows)]
                    {
                        let inner = pyrowave_d3d11::Encoder::new(
                            setup.width,
                            setup.height,
                            frame_bytes,
                            setup.yuv,
                        )?;
                        return Ok((VideoEncoder::PyrowaveD3d11 { inner, sink }, kbps));
                    }
                    #[cfg(not(windows))]
                    {
                        return Err("pyrowave-d3d11 is only built for Windows".into());
                    }
                }
                let inner =
                    pyrowave::Encoder::new(setup.width, setup.height, frame_bytes, setup.yuv)?;
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
                    scale_budget(
                        ffmpeg::default_kbps(video.codec(), setup.width, setup.height, setup.fps),
                        setup.quality,
                    )
                };
                let spec = ffmpeg::EncoderSpec { video, name };
                let inner = ffmpeg::Encoder::new(
                    &spec,
                    setup.width,
                    setup.height,
                    setup.fps,
                    kbps,
                    setup.dirty_rects,
                    sink,
                )?;
                Ok((VideoEncoder::Ffmpeg(inner), kbps))
            }
        }
    }

    /// JPEG or JPEG 2000 of `frame` without writing it. Used for dirty tiles.
    pub fn encode_intra(&self, frame: &Frame) -> Result<Vec<u8>, String> {
        match self {
            VideoEncoder::Mjpeg { quality, .. } => jpeg::encode(frame, *quality),
            VideoEncoder::Mj2k { quality, .. } => jpeg2000::encode(frame, *quality),
            _ => Err("this codec does not encode a tile on its own".into()),
        }
    }

    pub fn push(&mut self, frame: &Frame) -> Result<bool, String> {
        match self {
            VideoEncoder::Mjpeg { sink, quality } => Ok(sink(jpeg::encode(frame, *quality)?)),
            VideoEncoder::Mj2k { sink, quality } => Ok(sink(jpeg2000::encode(frame, *quality)?)),
            VideoEncoder::Pyrowave { inner, sink } => Ok(sink(inner.encode(frame)?)),
            #[cfg(windows)]
            VideoEncoder::PyrowaveD3d11 { inner, sink } => Ok(sink(inner.encode(frame)?)),
            VideoEncoder::Ffmpeg(inner) => inner.push(frame).map(|_| true),
        }
    }
}

pub enum VideoDecoder {
    Mjpeg {
        sink: Sink<Frame>,
    },
    Mj2k {
        sink: Sink<Frame>,
    },
    Pyrowave {
        inner: pyrowave::Decoder,
        sink: Sink<Frame>,
    },
    Ffmpeg(ffmpeg::Decoder),
}

impl VideoDecoder {
    pub fn open(
        video: Video,
        width: u32,
        height: u32,
        sink: Sink<Frame>,
        yuv: crate::negotiate::YuvFormat,
    ) -> Result<VideoDecoder, String> {
        match video.codec() {
            VideoCodec::Mjpeg => Ok(VideoDecoder::Mjpeg { sink }),
            VideoCodec::Mj2k => Ok(VideoDecoder::Mj2k { sink }),
            VideoCodec::Pyrowave => Ok(VideoDecoder::Pyrowave {
                inner: pyrowave::Decoder::new(width, height, yuv)?,
                sink,
            }),
            codec => Ok(VideoDecoder::Ffmpeg(ffmpeg::Decoder::new(
                codec, width, height, sink,
            )?)),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            VideoDecoder::Mjpeg { sink } => {
                sink(jpeg::decode(bytes)?);
                Ok(())
            }
            VideoDecoder::Mj2k { sink } => {
                sink(jpeg2000::decode(bytes)?);
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
