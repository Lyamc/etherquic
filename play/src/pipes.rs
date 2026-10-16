//! Side streams on a play session: files, clipboard text, and chat.
//!
//! Picture, audio, and input stay on their own streams. These open only when
//! both ends agreed on `etherquic/2`, so an older peer is left alone.

use quinn::{RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::chat::{decode_chat, encode_chat, route, ChatMessage, Participant, Routed};
use crate::clipboard::{decode_clipboard, encode_clipboard};
use crate::transfer::{decode_file, encode_file, NamedFile};
use crate::wire;
use crate::Error;

pub const FILE_MAGIC: &[u8; 4] = b"EFIL";
pub const CLIP_MAGIC: &[u8; 4] = b"ECLP";
pub const CHAT_MAGIC: &[u8; 4] = b"ECHT";

/// Between audio (10) and input (20), so a note is not stuck behind a frame.
pub const NOTE_PRIORITY: i32 = 15;

/// Channels for one direction of the side streams, plus the roster the
/// receiver uses to decide who a chat message is for.
pub struct Pipes {
    pub files_out: mpsc::Receiver<NamedFile>,
    pub files_in: mpsc::Sender<NamedFile>,
    pub clipboard_out: mpsc::Receiver<String>,
    pub clipboard_in: mpsc::Sender<String>,
    pub chat_out: mpsc::Receiver<ChatMessage>,
    pub chat_in: mpsc::Sender<Routed>,
    pub roster: Vec<Participant>,
}

impl Pipes {
    /// Nothing to send and nowhere to report. The streams still open and end.
    pub fn idle() -> Self {
        let (_files_tx, files_out) = mpsc::channel(1);
        let (files_in, _files_rx) = mpsc::channel(1);
        let (_clip_tx, clipboard_out) = mpsc::channel(1);
        let (clipboard_in, _clip_rx) = mpsc::channel(1);
        let (_chat_tx, chat_out) = mpsc::channel(1);
        let (chat_in, _chat_rx) = mpsc::channel(1);
        Self {
            files_out,
            files_in,
            clipboard_out,
            clipboard_in,
            chat_out,
            chat_in,
            roster: Vec::new(),
        }
    }
}

async fn finish(send: &mut SendStream) -> Result<(), Error> {
    send.finish()
        .map_err(|error| Error::Wire(error.to_string()))?;
    send.stopped()
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    Ok(())
}

pub async fn write_files(
    send: &mut SendStream,
    mut outbound: mpsc::Receiver<NamedFile>,
) -> Result<(), Error> {
    while let Some(file) = outbound.recv().await {
        if let Ok(payload) = encode_file(&file.name, &file.bytes) {
            wire::write_record(send, FILE_MAGIC, &payload).await?;
        }
    }
    finish(send).await
}

pub async fn read_files(
    mut recv: RecvStream,
    inbound: mpsc::Sender<NamedFile>,
) -> Result<(), Error> {
    while let Some(payload) = wire::read_record(&mut recv, FILE_MAGIC).await? {
        let file = decode_file(&payload).map_err(Error::Wire)?;
        if inbound.send(file).await.is_err() {
            break;
        }
    }
    Ok(())
}

pub async fn write_clipboard(
    send: &mut SendStream,
    mut outbound: mpsc::Receiver<String>,
) -> Result<(), Error> {
    while let Some(text) = outbound.recv().await {
        let payload = encode_clipboard(&text);
        wire::write_record(send, CLIP_MAGIC, &payload).await?;
    }
    finish(send).await
}

pub async fn read_clipboard(
    mut recv: RecvStream,
    inbound: mpsc::Sender<String>,
) -> Result<(), Error> {
    while let Some(payload) = wire::read_record(&mut recv, CLIP_MAGIC).await? {
        let text = decode_clipboard(&payload).map_err(Error::Wire)?;
        if inbound.send(text).await.is_err() {
            break;
        }
    }
    Ok(())
}

pub async fn write_chat(
    send: &mut SendStream,
    mut outbound: mpsc::Receiver<ChatMessage>,
) -> Result<(), Error> {
    while let Some(message) = outbound.recv().await {
        let payload = encode_chat(&message).map_err(Error::Wire)?;
        wire::write_record(send, CHAT_MAGIC, &payload).await?;
    }
    finish(send).await
}

pub async fn read_chat(
    mut recv: RecvStream,
    roster: Vec<Participant>,
    inbound: mpsc::Sender<Routed>,
) -> Result<(), Error> {
    while let Some(payload) = wire::read_record(&mut recv, CHAT_MAGIC).await? {
        let message = decode_chat(&payload).map_err(Error::Wire)?;
        let routed = route(&roster, message);
        if inbound.send(routed).await.is_err() {
            break;
        }
    }
    Ok(())
}
