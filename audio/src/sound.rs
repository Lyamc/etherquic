//! Userspace virtual sound interface.
//!
//! [`VirtualSound::loopback`] pairs a playback end with a capture end. Bytes
//! played are the bytes captured, which is the cable a physical loopback
//! device would give you. There is no OS audio endpoint and no Opus codec:
//! the samples are the same Ogg Opus or LOC bytes the send and receive tools
//! already move.
//!
//! Two processes share a cable through [`VirtualSound::play_file`] and
//! [`VirtualSound::capture_file`]. The file appears only when the whole
//! playback is written, so capture never reads a torn buffer.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::Error;

const MAGIC: &[u8; 4] = b"EQVS";
const VERSION: u8 = 1;
const HEADER: usize = 13;

/// Default cable path when `--virtual` is given without a path.
pub const DEFAULT_CABLE: &str = "etherquic-audio.eqvs";

enum Frame {
    Data(Vec<u8>),
    End,
}

/// Speaker side of the virtual interface.
pub struct Playback {
    tx: Sender<Frame>,
    closed: bool,
}

/// Microphone side of the virtual interface.
pub struct Capture {
    rx: Receiver<Frame>,
}

/// The virtual device. [`Self::loopback`] is the in-process cable.
pub struct VirtualSound;

impl VirtualSound {
    pub fn loopback() -> (Playback, Capture) {
        let (tx, rx) = mpsc::channel();
        (Playback { tx, closed: false }, Capture { rx })
    }

    /// Publish one buffer as a cable file other processes can capture.
    pub fn play_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
        let mut wire = Vec::with_capacity(HEADER + bytes.len());
        wire.extend_from_slice(MAGIC);
        wire.push(VERSION);
        let len = u64::try_from(bytes.len())
            .map_err(|_| Error::Virtual("playback does not fit in the cable header".into()))?;
        wire.extend_from_slice(&len.to_le_bytes());
        wire.extend_from_slice(bytes);
        let tmp = tmp_path(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::Virtual(format!("create {}: {e}", parent.display())))?;
            }
        }
        std::fs::write(&tmp, &wire)
            .map_err(|e| Error::Virtual(format!("write {}: {e}", tmp.display())))?;
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path).map_err(|e| {
            Error::Virtual(format!(
                "publish {} -> {}: {e}",
                tmp.display(),
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Wait until a cable file is complete, then return its playback bytes.
    pub fn capture_file(path: &Path, timeout: Duration) -> Result<Vec<u8>, Error> {
        let start = Instant::now();
        loop {
            match std::fs::read(path) {
                Ok(bytes) => match parse_cable(&bytes)? {
                    Some(payload) => return Ok(payload),
                    None => {}
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(Error::Virtual(format!("read {}: {e}", path.display())));
                }
            }
            if start.elapsed() >= timeout {
                return Err(Error::Virtual(format!(
                    "timed out waiting for {}",
                    path.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Playback {
    pub fn play(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Virtual("playback is closed".into()));
        }
        self.tx
            .send(Frame::Data(bytes.to_vec()))
            .map_err(|_| Error::Virtual("capture end is gone".into()))
    }

    pub fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.tx
            .send(Frame::End)
            .map_err(|_| Error::Virtual("capture end is gone".into()))
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.tx.send(Frame::End);
        }
    }
}

impl Capture {
    /// Read until playback closes. The chunks are concatenated in order.
    pub fn read_to_end(&mut self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        loop {
            match self.rx.recv() {
                Ok(Frame::Data(chunk)) => out.extend_from_slice(&chunk),
                Ok(Frame::End) => return Ok(out),
                Err(_) => {
                    return Err(Error::Virtual(
                        "playback closed without an end marker".into(),
                    ));
                }
            }
        }
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "etherquic-audio".into());
    name.push(".tmp");
    path.with_file_name(name)
}

fn parse_cable(bytes: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    if bytes.len() >= 4 && &bytes[..4] != MAGIC {
        return Err(Error::Virtual("cable file is not EQVS".into()));
    }
    if bytes.len() >= 5 && bytes[4] != VERSION {
        return Err(Error::Virtual(format!(
            "unsupported cable version {}",
            bytes[4]
        )));
    }
    if bytes.len() < HEADER {
        return Ok(None);
    }
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&bytes[5..13]);
    let len = usize::try_from(u64::from_le_bytes(len_bytes))
        .map_err(|_| Error::Virtual("cable length does not fit".into()))?;
    if bytes.len() < HEADER + len {
        return Ok(None);
    }
    if bytes.len() != HEADER + len {
        return Err(Error::Virtual("cable file has trailing bytes".into()));
    }
    Ok(Some(bytes[HEADER..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_as, mux_opus, opus_id_header, parse_id, store, OpusPacket, OpusStream};

    #[test]
    fn loopback_plays_into_capture() {
        let (mut play, mut capture) = VirtualSound::loopback();
        play.play(b"one").unwrap();
        play.play(b"two").unwrap();
        play.close().unwrap();
        assert_eq!(capture.read_to_end().unwrap(), b"onetwo");
    }

    #[test]
    fn drop_ends_capture() {
        let (mut play, mut capture) = VirtualSound::loopback();
        play.play(&[1, 2, 3]).unwrap();
        drop(play);
        assert_eq!(capture.read_to_end().unwrap(), &[1, 2, 3]);
    }

    #[test]
    fn file_cable_roundtrips_opus_bytes() {
        let stream = OpusStream {
            id: parse_id(&opus_id_header(1, 120, 48_000)).unwrap(),
            packets: vec![OpusPacket {
                data: vec![0x98],
                start_granule: 0,
                samples: 960,
            }],
            end_trim: 0,
        };
        let ogg = mux_opus(&stream).unwrap();
        let dir =
            std::env::temp_dir().join(format!("etherquic-audio-cable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(DEFAULT_CABLE);
        let capture_path = path.clone();
        let played = ogg.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            VirtualSound::play_file(&capture_path, &played).unwrap();
        });
        let got = VirtualSound::capture_file(&path, Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(got, ogg);
        let objects = load_as(&got, Some("opus")).unwrap();
        let again = load_as(&store(&objects, true).unwrap(), Some("loc")).unwrap();
        assert_eq!(again.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incomplete_cable_is_not_ready() {
        assert!(parse_cable(b"EQVS").unwrap().is_none());
        assert!(parse_cable(b"NOPE\x01").is_err());
    }
}
