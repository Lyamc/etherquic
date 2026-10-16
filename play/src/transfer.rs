//! One named file on the live session.
//!
//! This is not the video file tool. A record is a file name plus the bytes.
//! An empty name is refused before anything is written.

use tokio::sync::mpsc;

/// A file either peer can hand the other. The receiver keeps this name and
/// these bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl NamedFile {
    /// `Err` when `name` is empty. The bytes are kept as given.
    pub fn try_new(name: &str, bytes: &[u8]) -> Result<Self, String> {
        if name.is_empty() {
            return Err("file name is empty".into());
        }
        if name.len() > u16::MAX as usize {
            return Err("file name is too long".into());
        }
        Ok(Self {
            name: name.to_string(),
            bytes: bytes.to_vec(),
        })
    }
}

/// `u16le` name length, the UTF-8 name, then the bytes.
pub fn encode_file(name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let file = NamedFile::try_new(name, bytes)?;
    let mut out = Vec::with_capacity(2 + file.name.len() + file.bytes.len());
    out.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
    out.extend_from_slice(file.name.as_bytes());
    out.extend_from_slice(&file.bytes);
    Ok(out)
}

pub fn decode_file(payload: &[u8]) -> Result<NamedFile, String> {
    if payload.len() < 2 {
        return Err("file record is short".into());
    }
    let len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if payload.len() < 2 + len {
        return Err("file name is truncated".into());
    }
    let name = std::str::from_utf8(&payload[2..2 + len])
        .map_err(|_| "file name is not utf-8".to_string())?;
    if name.is_empty() {
        return Err("file name is empty".into());
    }
    Ok(NamedFile {
        name: name.to_string(),
        bytes: payload[2 + len..].to_vec(),
    })
}

/// Queue one file for the session. An empty name is not queued.
pub async fn submit_file(
    tx: &mpsc::Sender<NamedFile>,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let file = NamedFile::try_new(name, bytes)?;
    tx.send(file)
        .await
        .map_err(|_| "file channel closed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_name_is_refused_and_a_named_file_round_trips() {
        assert!(encode_file("", b"secret").is_err());
        let bytes = b"same-bytes".to_vec();
        let encoded = encode_file("notes.txt", &bytes).unwrap();
        let back = decode_file(&encoded).unwrap();
        assert_eq!(back.name, "notes.txt");
        assert_eq!(back.bytes, bytes);
    }
}
