//! Notes the desktop session submits, and the notes it has received.
//!
//! `host_desktop` and `connect_view` own the pipes. Callers queue through
//! these functions; the session tasks call `submit_file`, `submit_clipboard`,
//! and `submit_chat`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tokio::sync::mpsc;

use crate::chat::{submit_chat, ChatMessage, Participant, Routed};
use crate::clipboard::{submit_clipboard, text_paste, TextPaste};
use crate::picture_hub::shown_picture;
use crate::pipes::Pipes;
use crate::transfer::{submit_file, NamedFile};

/// Which desktop end queues a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteSide {
    Host,
    Player,
}

/// Files, clipboard strings, and routed chat one end has received.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeskMail {
    pub files: Vec<NamedFile>,
    pub clipboard: Vec<String>,
    pub chat: Vec<Routed>,
}

pub(crate) enum Cmd {
    File(NamedFile),
    Clip(String),
    Chat(ChatMessage),
}

struct MailSlots {
    host: DeskMail,
    player: DeskMail,
}

fn mail() -> &'static Mutex<MailSlots> {
    static MAIL: OnceLock<Mutex<MailSlots>> = OnceLock::new();
    MAIL.get_or_init(|| {
        Mutex::new(MailSlots {
            host: DeskMail::default(),
            player: DeskMail::default(),
        })
    })
}

fn host_cmd() -> &'static Mutex<Option<mpsc::Sender<Cmd>>> {
    static HOST: OnceLock<Mutex<Option<mpsc::Sender<Cmd>>>> = OnceLock::new();
    HOST.get_or_init(|| Mutex::new(None))
}

fn player_cmds() -> &'static Mutex<HashMap<u64, mpsc::Sender<Cmd>>> {
    static PLAYERS: OnceLock<Mutex<HashMap<u64, mpsc::Sender<Cmd>>>> = OnceLock::new();
    PLAYERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn keys() -> &'static Mutex<HashMap<u64, mpsc::UnboundedSender<String>>> {
    static KEYS: OnceLock<Mutex<HashMap<u64, mpsc::UnboundedSender<String>>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn roster_slot() -> &'static Mutex<Vec<Participant>> {
    static ROSTER: OnceLock<Mutex<Vec<Participant>>> = OnceLock::new();
    ROSTER.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Roster both desktop ends route chat with. An empty roster is one local player.
pub fn set_desk_roster(roster: Vec<Participant>) {
    *lock(roster_slot()) = roster;
}

pub fn desk_roster() -> Vec<Participant> {
    let roster = lock(roster_slot()).clone();
    if roster.is_empty() {
        vec![Participant {
            id: "player".into(),
            team: "all".into(),
        }]
    } else {
        roster
    }
}

pub(crate) struct OutDrive {
    cmds: mpsc::Receiver<Cmd>,
    files: mpsc::Sender<NamedFile>,
    clipboard: mpsc::Sender<String>,
    chat: mpsc::Sender<ChatMessage>,
}

pub(crate) struct InDrive {
    files: mpsc::Receiver<NamedFile>,
    clipboard: mpsc::Receiver<String>,
    chat: mpsc::Receiver<Routed>,
}

pub(crate) fn open_notes(
    roster: Vec<Participant>,
) -> (mpsc::Sender<Cmd>, Pipes, OutDrive, InDrive) {
    let (files_tx, files_out) = mpsc::channel(8);
    let (files_in, files_rx) = mpsc::channel(8);
    let (clip_tx, clip_out) = mpsc::channel(8);
    let (clip_in, clip_rx) = mpsc::channel(8);
    let (chat_tx, chat_out) = mpsc::channel(8);
    let (chat_in, chat_rx) = mpsc::channel(8);
    let (cmd_tx, cmds) = mpsc::channel(32);
    (
        cmd_tx,
        Pipes {
            files_out,
            files_in,
            clipboard_out: clip_out,
            clipboard_in: clip_in,
            chat_out,
            chat_in,
            roster,
        },
        OutDrive {
            cmds,
            files: files_tx,
            clipboard: clip_tx,
            chat: chat_tx,
        },
        InDrive {
            files: files_rx,
            clipboard: clip_rx,
            chat: chat_rx,
        },
    )
}

pub(crate) fn attach_host(tx: mpsc::Sender<Cmd>) {
    *lock(host_cmd()) = Some(tx);
}

pub(crate) fn detach_host() {
    *lock(host_cmd()) = None;
}

pub(crate) fn attach_player(id: u64, tx: mpsc::Sender<Cmd>) {
    lock(player_cmds()).insert(id, tx);
}

pub(crate) fn detach_player(id: u64) {
    lock(player_cmds()).remove(&id);
}

pub(crate) fn attach_keys(id: u64, tx: mpsc::UnboundedSender<String>) {
    lock(keys()).insert(id, tx);
}

pub(crate) fn detach_keys(id: u64) {
    lock(keys()).remove(&id);
}

/// Hand a keystroke string to the picture that is on screen.
pub(crate) fn send_keys(id: u64, text: &str) -> bool {
    let tx = lock(keys()).get(&id).cloned();
    match tx {
        Some(tx) => tx.send(text.to_string()).is_ok(),
        None => false,
    }
}

/// Queue one named file. An empty name is refused and nothing is queued.
pub fn queue_file(side: NoteSide, name: &str, bytes: &[u8]) -> Result<(), String> {
    let file = NamedFile::try_new(name, bytes)?;
    sender(side)?
        .try_send(Cmd::File(file))
        .map_err(|_| "session notes are closed".to_string())
}

pub fn queue_clipboard(side: NoteSide, text: &str) -> Result<(), String> {
    sender(side)?
        .try_send(Cmd::Clip(text.to_string()))
        .map_err(|_| "session notes are closed".to_string())
}

/// Send clipboard text on one pipe. Keystrokes and the clipboard string are not both used.
pub fn send_text_paste(text: &str, as_keystrokes: bool) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    match text_paste(as_keystrokes) {
        TextPaste::Keystrokes => {
            #[cfg(feature = "desktop")]
            {
                if !crate::queue_window_keystrokes(text) {
                    return Err("No view is open.".to_string());
                }
                Ok(())
            }
            #[cfg(not(feature = "desktop"))]
            {
                let _ = text;
                Err("keystroke paste needs a desktop session".to_string())
            }
        }
        TextPaste::Clipboard => queue_clipboard(NoteSide::Player, text),
    }
}

pub fn queue_chat(side: NoteSide, message: ChatMessage) -> Result<(), String> {
    sender(side)?
        .try_send(Cmd::Chat(message))
        .map_err(|_| "session notes are closed".to_string())
}

fn sender(side: NoteSide) -> Result<mpsc::Sender<Cmd>, String> {
    match side {
        NoteSide::Host => lock(host_cmd())
            .clone()
            .ok_or_else(|| "session notes are closed".to_string()),
        NoteSide::Player => {
            let map = lock(player_cmds());
            if let Some(id) = shown_picture() {
                if let Some(tx) = map.get(&id) {
                    return Ok(tx.clone());
                }
            }
            if map.len() == 1 {
                return Ok(map.values().next().expect("one player").clone());
            }
            Err("session notes are closed".to_string())
        }
    }
}

pub fn take_host_mail() -> DeskMail {
    std::mem::take(&mut lock(mail()).host)
}

pub fn take_player_mail() -> DeskMail {
    std::mem::take(&mut lock(mail()).player)
}

pub fn clear_mail() {
    let mut slot = lock(mail());
    slot.host = DeskMail::default();
    slot.player = DeskMail::default();
}

/// `submit_file`, `submit_clipboard`, and `submit_chat` for whatever was queued.
pub(crate) async fn pump_outbound(mut drive: OutDrive) {
    while let Some(cmd) = drive.cmds.recv().await {
        let result = match cmd {
            Cmd::File(file) => submit_file(&drive.files, &file.name, &file.bytes).await,
            Cmd::Clip(text) => submit_clipboard(&drive.clipboard, &text).await,
            Cmd::Chat(message) => submit_chat(&drive.chat, message).await,
        };
        if result.is_err() {
            break;
        }
    }
}

/// Record each note the peer delivered.
pub(crate) async fn pump_inbound(side: NoteSide, mut drive: InDrive) {
    let mut files_open = true;
    let mut clips_open = true;
    let mut chat_open = true;
    while files_open || clips_open || chat_open {
        tokio::select! {
            biased;
            file = drive.files.recv(), if files_open => {
                match file {
                    Some(file) => push_file(side, file),
                    None => files_open = false,
                }
            }
            text = drive.clipboard.recv(), if clips_open => {
                match text {
                    Some(text) => push_clip(side, text),
                    None => clips_open = false,
                }
            }
            routed = drive.chat.recv(), if chat_open => {
                match routed {
                    Some(routed) => push_chat(side, routed),
                    None => chat_open = false,
                }
            }
        }
    }
}

fn push_file(side: NoteSide, file: NamedFile) {
    let mut slot = lock(mail());
    let mail = match side {
        NoteSide::Host => &mut slot.host,
        NoteSide::Player => &mut slot.player,
    };
    if mail.files.len() == 32 {
        mail.files.remove(0);
    }
    mail.files.push(file);
}

fn push_clip(side: NoteSide, text: String) {
    let mut slot = lock(mail());
    let mail = match side {
        NoteSide::Host => &mut slot.host,
        NoteSide::Player => &mut slot.player,
    };
    if mail.clipboard.len() == 32 {
        mail.clipboard.remove(0);
    }
    mail.clipboard.push(text);
}

fn push_chat(side: NoteSide, routed: Routed) {
    let mut slot = lock(mail());
    let mail = match side {
        NoteSide::Host => &mut slot.host,
        NoteSide::Player => &mut slot.player,
    };
    if mail.chat.len() == 32 {
        mail.chat.remove(0);
    }
    mail.chat.push(routed);
}
