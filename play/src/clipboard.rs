//! Clipboard text on the live session. The host receives the player's string.
//!
//! A paste of that text is one delivery. [`TextPaste::Clipboard`] sends the
//! string on the clipboard pipe. [`TextPaste::Keystrokes`] types it. A paste
//! does not do both.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tokio::sync::mpsc;

/// Which pipe a clipboard-text paste uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextPaste {
    /// The clipboard string, unchanged.
    Clipboard,
    /// The same characters as key events.
    Keystrokes,
}

/// `as_keystrokes` chooses the keystroke pipe. Otherwise the paste is clipboard text.
pub fn text_paste(as_keystrokes: bool) -> TextPaste {
    if as_keystrokes {
        TextPaste::Keystrokes
    } else {
        TextPaste::Clipboard
    }
}

static PASTE_WATCH: AtomicBool = AtomicBool::new(false);
static PASTE_REQUESTS: AtomicU32 = AtomicU32::new(0);

/// The window is watching for a paste from the picture. Ctrl+V can ask for one.
pub fn watch_text_paste() {
    PASTE_WATCH.store(true, Ordering::SeqCst);
}

/// True after [`watch_text_paste`]. The picture window forwards Ctrl+V until then.
pub fn text_paste_watched() -> bool {
    PASTE_WATCH.load(Ordering::SeqCst)
}

/// Ask the window to paste the local clipboard once.
pub fn request_text_paste() {
    PASTE_REQUESTS.fetch_add(1, Ordering::SeqCst);
}

/// How many pastes were requested since the last take.
pub fn take_text_paste_requests() -> u32 {
    PASTE_REQUESTS.swap(0, Ordering::SeqCst)
}

pub fn encode_clipboard(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

pub fn decode_clipboard(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| "clipboard is not utf-8".to_string())
}

/// Queue clipboard text. The session delivers this same string.
pub async fn submit_clipboard(tx: &mpsc::Sender<String>, text: &str) -> Result<(), String> {
    tx.send(text.to_string())
        .await
        .map_err(|_| "clipboard channel closed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_text_round_trips_unchanged() {
        let text = "line\nwith\tspaces and ☃";
        let back = decode_clipboard(&encode_clipboard(text)).unwrap();
        assert_eq!(back, text);
    }

    #[test]
    fn clipboard_paste_is_one_delivery() {
        assert_eq!(text_paste(false), TextPaste::Clipboard);
        assert_eq!(text_paste(true), TextPaste::Keystrokes);
    }
}
