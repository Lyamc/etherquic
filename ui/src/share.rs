//! Text a phone reads when it scans the host card.
//!
//! The lines are the same facts a discovery offer carries: the name, the MAC
//! and service to dial, the port, and the adapter addresses. The QR grid is
//! that text. A quiet zone is included so the dark window does not eat the edge.

use qrcode::types::Color;
use qrcode::QrCode;

use crate::session::{HOST_MAC, PLAY_SERVICE_ID};

pub use etherquic::share::{offer_name, Share, ShareRow, NAME_LIMIT};

/// Spec minimum. The white pad in the window is extra margin on top of this.
const QUIET: usize = 4;

/// Dark modules, row by row, including the quiet zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrGrid {
    pub modules: Vec<bool>,
    pub width: usize,
}

/// This PC, as the host card publishes it. A missing adapter still yields the MAC and service.
pub fn this_pc(nic: Option<&str>) -> Share {
    let (ipv4, ipv6) = match etherquic::pcap_link::interface_info(nic) {
        Ok(info) => (info.ipv4, info.ipv6),
        Err(_) => (Vec::new(), Vec::new()),
    };
    Share {
        name: machine_name(),
        mac: HOST_MAC,
        service: PLAY_SERVICE_ID,
        port: PLAY_SERVICE_ID,
        ipv4,
        ipv6,
    }
}

pub fn qr_grid(payload: &str) -> Result<QrGrid, String> {
    let code = QrCode::new(payload.as_bytes()).map_err(|err| err.to_string())?;
    let side = code.width();
    let width = side + QUIET * 2;
    let mut modules = vec![false; width * width];
    for y in 0..side {
        for x in 0..side {
            if code[(x, y)] == Color::Dark {
                modules[(y + QUIET) * width + (x + QUIET)] = true;
            }
        }
    }
    Ok(QrGrid { modules, width })
}

pub(crate) fn machine_name() -> String {
    #[cfg(windows)]
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    #[cfg(target_os = "android")]
    if let Some(name) = android_model() {
        return name;
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "etherquic".into())
}

#[cfg(target_os = "android")]
fn android_model() -> Option<String> {
    let text = std::fs::read_to_string("/system/build.prop").ok()?;
    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("ro.product.model=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn sample() -> Share {
        Share {
            name: "o11d".into(),
            mac: HOST_MAC,
            service: PLAY_SERVICE_ID,
            port: 4434,
            ipv4: vec![Ipv4Addr::new(192, 168, 0, 24)],
            ipv6: vec!["fe80::1".parse().unwrap()],
        }
    }

    #[test]
    fn share_text_round_trips() {
        let share = sample();
        let text = share.text();
        assert!(text.starts_with("etherquic\n"));
        assert!(text.contains("name o11d\n"));
        assert!(text.contains("mac 02:00:00:00:00:01\n"));
        assert!(text.contains("service 4433\n"));
        assert!(text.contains("port 4434\n"));
        assert!(text.contains("ip 192.168.0.24\n"));
        assert!(text.contains("ip fe80::1\n"));
        assert_eq!(Share::parse(&text), Some(share));
        let rows = sample().rows();
        assert_eq!(rows[0].value, "o11d");
        assert_eq!(rows[1].value, "02:00:00:00:00:01 · service 4433");
        assert_eq!(rows[2].label, "IPv4");
        assert_eq!(rows[2].value, "192.168.0.24 · port 4434");
        assert_eq!(rows[3].label, "IPv6");
        assert_eq!(rows[3].value, "fe80::1 · port 4434");
    }

    #[test]
    fn an_empty_adapter_still_names_the_service() {
        let share = Share {
            name: String::new(),
            mac: HOST_MAC,
            service: PLAY_SERVICE_ID,
            port: PLAY_SERVICE_ID,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        };
        let text = share.text();
        assert!(!text.contains("name "));
        assert_eq!(Share::parse(&text), Some(share.clone()));
        let rows = share.rows();
        assert_eq!(rows[0].value, "unknown");
        assert_eq!(rows[1].value, "02:00:00:00:00:01 · service 4433");
        assert_eq!(rows[2].value, "no address");
        assert_eq!(rows[3].value, "no address");
    }

    #[test]
    fn a_newline_in_the_name_stays_one_field() {
        let share = Share {
            name: "a\nb and a very long name that exceeds thirty two bytes".into(),
            mac: HOST_MAC,
            service: 1,
            port: 2,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        };
        let text = share.text();
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("name "))
                .count(),
            1
        );
        let parsed = Share::parse(&text).unwrap();
        assert_eq!(parsed.name, offer_name(&share.name));
        assert!(!parsed.name.contains('\n'));
        assert!(parsed.name.len() <= NAME_LIMIT);
        assert!(parsed.name.starts_with("a b and"));
        assert_eq!(parsed.service, 1);
        assert_eq!(parsed.port, 2);
    }

    #[test]
    fn parse_rejects_a_foreign_header() {
        assert!(Share::parse("hello\nmac 02:00:00:00:00:01\nservice 1\nport 1\n").is_none());
        assert!(Share::parse("etherquic\nmac 02:00\nservice 1\nport 1\n").is_none());
        assert!(Share::parse("etherquic\nservice 1\nport 1\n").is_none());
    }

    #[test]
    fn qr_matches_the_encoder_and_keeps_a_quiet_zone() {
        let payload = sample().text();
        let grid = qr_grid(&payload).unwrap();
        let code = QrCode::new(payload.as_bytes()).unwrap();
        let side = code.width();
        assert_eq!(grid.width, side + QUIET * 2);
        let drawn = code.to_debug_str('#', '.');
        let lines: Vec<&str> = drawn.lines().collect();
        assert_eq!(lines.len(), side);
        for (y, line) in lines.iter().enumerate() {
            assert_eq!(line.chars().count(), side);
            for (x, ch) in line.chars().enumerate() {
                let on = grid.modules[(y + QUIET) * grid.width + (x + QUIET)];
                assert_eq!(on, ch == '#');
            }
        }
        for i in 0..grid.width {
            assert!(!grid.modules[i]);
            assert!(!grid.modules[3 * grid.width + i]);
            assert!(!grid.modules[i * grid.width]);
        }
        let other = qr_grid("etherquic\nmac 02:00:00:00:00:02\nservice 1\nport 1\n").unwrap();
        assert_ne!(grid.modules, other.modules);
    }
}
