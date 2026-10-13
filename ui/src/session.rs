//! Who is hosting, and the peer address the player types.

use etherquic::{PRIORITY_AUDIO, PRIORITY_INPUT, PRIORITY_VIDEO};

/// This PC while it is the host. A second process on the same machine uses [`CLIENT_MAC`].
pub const HOST_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
/// This PC while it is the player.
pub const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

/// Play session. The host listens here. Video uses the same service id.
pub const PLAY_SERVICE_ID: u16 = 4433;
/// Audio lane. Shown in the console; the host does not open a second listener.
pub const AUDIO_SERVICE_ID: u16 = 4434;
/// Input lane. Shown in the console; the host does not open a second listener.
pub const INPUT_SERVICE_ID: u16 = 4435;

/// Idle, hosting this PC, or connected as the player. Only one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Idle,
    Host,
    Client,
}

impl Role {
    /// The taskbar button belongs to the host. A player does not take a slot.
    pub fn shows_taskbar(self) -> bool {
        matches!(self, Role::Host)
    }
}

/// The button is shown only after the host endpoint is actually up.
pub fn taskbar_for(role: Role, link_up: bool) -> bool {
    role.shows_taskbar() && link_up
}

/// One media lane on a shared connection. Higher priority is sent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    pub name: &'static str,
    pub priority: i32,
    pub service: u16,
}

/// Input, then audio, then video. That is the order Quinn drains.
pub fn lanes() -> [Lane; 3] {
    [
        Lane {
            name: "Input",
            priority: PRIORITY_INPUT,
            service: INPUT_SERVICE_ID,
        },
        Lane {
            name: "Audio",
            priority: PRIORITY_AUDIO,
            service: AUDIO_SERVICE_ID,
        },
        Lane {
            name: "Video",
            priority: PRIORITY_VIDEO,
            service: PLAY_SERVICE_ID,
        },
    ]
}

pub fn format_mac(mac: [u8; 6]) -> String {
    HexMac::from_bytes(mac).display()
}

/// Up to 12 hex digits. Colons are display-only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexMac {
    digits: String,
}

impl Default for HexMac {
    fn default() -> Self {
        Self {
            digits: String::new(),
        }
    }
}

impl HexMac {
    pub fn from_bytes(mac: [u8; 6]) -> Self {
        let mut digits = String::with_capacity(12);
        for byte in mac {
            digits.push_str(&format!("{byte:02x}"));
        }
        Self { digits }
    }

    pub fn digits(&self) -> &str {
        &self.digits
    }

    pub fn display(&self) -> String {
        let mut out = String::with_capacity(17);
        for (i, ch) in self.digits.chars().enumerate() {
            if i > 0 && i % 2 == 0 {
                out.push(':');
            }
            out.push(ch);
        }
        out
    }

    /// Accept a typed nibble. Colons and dashes are ignored so a pasted-looking
    /// address can be typed with separators. Anything else is ignored.
    pub fn push(&mut self, ch: char) {
        if self.digits.len() >= 12 || ch == ':' || ch == '-' {
            return;
        }
        if ch.is_ascii_hexdigit() {
            self.digits.push(ch.to_ascii_lowercase());
        }
    }

    pub fn backspace(&mut self) {
        self.digits.pop();
    }

    pub fn clear(&mut self) {
        self.digits.clear();
    }

    /// A full address, or `None` while the field is still short.
    pub fn parse(&self) -> Option<[u8; 6]> {
        if self.digits.len() != 12 {
            return None;
        }
        let mut mac = [0u8; 6];
        for i in 0..6 {
            mac[i] = u8::from_str_radix(&self.digits[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(mac)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_live_host_takes_the_taskbar() {
        assert!(Role::Host.shows_taskbar());
        assert!(!Role::Client.shows_taskbar());
        assert!(!Role::Idle.shows_taskbar());
        assert!(taskbar_for(Role::Host, true));
        assert!(!taskbar_for(Role::Host, false));
        assert!(!taskbar_for(Role::Client, true));
        assert!(!taskbar_for(Role::Idle, true));
    }

    #[test]
    fn lanes_send_input_then_audio_then_video() {
        let lanes = lanes();
        assert_eq!(lanes[0].name, "Input");
        assert_eq!(lanes[1].name, "Audio");
        assert_eq!(lanes[2].name, "Video");
        assert!(lanes[0].priority > lanes[1].priority);
        assert!(lanes[1].priority > lanes[2].priority);
        assert_eq!(lanes[2].service, PLAY_SERVICE_ID);
        assert_eq!(lanes[0].priority, 20);
        assert_eq!(lanes[1].priority, 10);
        assert_eq!(lanes[2].priority, 0);
    }

    #[test]
    fn hex_mac_groups_backspaces_and_parses() {
        let host = HexMac::from_bytes(HOST_MAC);
        assert_eq!(host.display(), "02:00:00:00:00:01");
        assert_eq!(host.parse(), Some(HOST_MAC));
        assert_ne!(HOST_MAC, CLIENT_MAC);

        let mut mac = HexMac::default();
        mac.push('g');
        mac.push(':');
        assert!(mac.digits().is_empty());
        mac.push('A');
        mac.push('b');
        mac.push('C');
        assert_eq!(mac.display(), "ab:c");
        mac.backspace();
        assert_eq!(mac.display(), "ab");
        assert_eq!(mac.parse(), None);

        mac.clear();
        for ch in "0123456789abcdef".chars() {
            mac.push(ch);
        }
        assert_eq!(mac.digits().len(), 12);
        assert_eq!(mac.parse(), Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab]));
        assert_eq!(mac.display(), "01:23:45:67:89:ab");
    }
}
