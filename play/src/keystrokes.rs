//! Keystroke strings on the live session.
//!
//! A string becomes one key-down and one key-up per character, as EQIN key
//! events. That path does not look at window focus. The picture window still
//! drops keys while it is unfocused.

use etherquic_input::{Body, Event, KeySource};
use tokio::sync::mpsc;

/// True when a focused picture window should forward a physical key.
///
/// Repeats and an unfocused window do not send. A keystroke string does not
/// use this gate.
pub fn focus_sends_keyboard(focused: bool, repeat: bool, send_keyboard: bool) -> bool {
    send_keyboard && focused && !repeat
}

/// X11 keysym for one character. A newline is Return. A tab is Tab.
/// Other Latin-1 keysyms are the code point.
fn keysym_for(ch: char) -> u32 {
    match ch {
        '\n' => 0xff0d,
        '\t' => 0xff09,
        other => u32::from(other),
    }
}

/// Characters to type. A Windows `\r\n` is one Return, not two.
fn paste_chars(text: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(text.chars().count());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Ctrl+V on Windows and Linux. Command+V or Ctrl+V on macOS.
/// Alt is left alone so AltGr can still type a character.
pub fn paste_chord(mac: bool, control: bool, alt: bool, super_key: bool, key_is_v: bool) -> bool {
    if !key_is_v || alt {
        return false;
    }
    if mac {
        super_key || control
    } else {
        control && !super_key
    }
}

/// Down, then up, for each character, in order. Focus is not an argument.
pub fn keystroke_events(text: &str) -> Vec<Event> {
    let chars = paste_chars(text);
    let mut events = Vec::with_capacity(chars.len() * 2);
    for ch in chars {
        let code = keysym_for(ch);
        events.push(Event {
            time_us: 0,
            body: Body::Key {
                down: true,
                source: KeySource::Keysym,
                code,
            },
        });
        events.push(Event {
            time_us: 0,
            body: Body::Key {
                down: false,
                source: KeySource::Keysym,
                code,
            },
        });
    }
    events
}

/// Queue the down/up sequence on the session input channel.
pub async fn submit_keystrokes(tx: &mpsc::Sender<Vec<Event>>, text: &str) -> Result<(), String> {
    let events = keystroke_events(text);
    if events.is_empty() {
        return Ok(());
    }
    tx.send(events)
        .await
        .map_err(|_| "input channel closed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_is_down_then_up_and_focus_still_gates_the_window() {
        assert!(!focus_sends_keyboard(false, false, true));
        assert!(!focus_sends_keyboard(true, true, true));
        assert!(!focus_sends_keyboard(true, false, false));
        assert!(focus_sends_keyboard(true, false, true));

        let events = keystroke_events("ab");
        assert_eq!(events.len(), 4);
        let codes: Vec<(bool, u32)> = events
            .iter()
            .map(|event| match event.body {
                Body::Key { down, code, source } => {
                    assert_eq!(source, KeySource::Keysym);
                    (down, code)
                }
                _ => panic!("keystroke string produced a non-key event"),
            })
            .collect();
        assert_eq!(
            codes,
            vec![
                (true, u32::from('a')),
                (false, u32::from('a')),
                (true, u32::from('b')),
                (false, u32::from('b')),
            ]
        );
    }

    #[test]
    fn a_paste_newline_is_one_return() {
        let codes: Vec<u32> = keystroke_events("a\r\nb")
            .into_iter()
            .filter_map(|event| match event.body {
                Body::Key {
                    down: true, code, ..
                } => Some(code),
                _ => None,
            })
            .collect();
        assert_eq!(codes, vec![u32::from('a'), 0xff0d, u32::from('b')]);
        assert_eq!(keystroke_events("\n").len(), keystroke_events("\r\n").len());
        assert_eq!(keystroke_events("\t").len(), 2);
    }

    #[test]
    fn paste_chord_is_ctrl_v_on_windows_and_command_v_on_mac() {
        assert!(paste_chord(false, true, false, false, true));
        assert!(!paste_chord(false, true, false, true, true));
        assert!(!paste_chord(false, true, true, false, true));
        assert!(!paste_chord(false, false, false, false, true));
        assert!(!paste_chord(false, true, false, false, false));
        assert!(paste_chord(true, false, false, true, true));
        assert!(paste_chord(true, true, false, false, true));
        assert!(!paste_chord(true, false, true, true, true));
    }
}
