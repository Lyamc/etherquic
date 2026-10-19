//! Text a phone reads when it scans the host card.
//!
//! The lines are the same facts a discovery offer carries: the name, the MAC
//! and service to dial, the port, and the adapter addresses. Android joins
//! over UDP using [`Share::udp_peer`]; it has no EtherType socket.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::pcap_link::parse_mac;
use crate::Mac;

/// Discovery offers clip the host name here. The QR uses the same cap.
pub const NAME_LIMIT: usize = 32;
/// Play service a blank card names. Matches the desktop host.
pub const DEFAULT_SERVICE: u16 = 4433;
/// Locally administered host MAC a blank card names.
pub const DEFAULT_MAC: Mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

/// What a phone needs in order to join this PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    /// MAC a player dials. This is the host identity, not the adapter's burned-in address.
    pub mac: Mac,
    pub service: u16,
    pub port: u16,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
}

/// One labeled fact about this PC, in the order the window shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareRow {
    pub label: &'static str,
    pub value: String,
}

impl Share {
    pub fn blank() -> Self {
        Self {
            name: String::new(),
            mac: DEFAULT_MAC,
            service: DEFAULT_SERVICE,
            port: DEFAULT_SERVICE,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        }
    }

    /// Hostname, the L2 address to dial, then IPv4 and IPv6 on their own rows.
    pub fn rows(&self) -> [ShareRow; 4] {
        let name = self.name.trim();
        let hostname = if name.is_empty() {
            "unknown".to_string()
        } else {
            name.to_string()
        };
        [
            ShareRow {
                label: "Hostname",
                value: hostname,
            },
            ShareRow {
                label: "L2",
                value: format!("{} · service {}", format_mac(self.mac), self.service),
            },
            ShareRow {
                label: "IPv4",
                value: l3_value(&self.ipv4, self.port),
            },
            ShareRow {
                label: "IPv6",
                value: l3_value(&self.ipv6, self.port),
            },
        ]
    }

    /// One field per line. A camera app shows this text as the scan result.
    pub fn text(&self) -> String {
        let mut out = String::from("etherquic\n");
        let name = offer_name(&self.name);
        if !name.is_empty() {
            out.push_str("name ");
            out.push_str(&name);
            out.push('\n');
        }
        out.push_str("mac ");
        out.push_str(&format_mac(self.mac));
        out.push('\n');
        out.push_str(&format!("service {}\nport {}\n", self.service, self.port));
        for ip in &self.ipv4 {
            out.push_str("ip ");
            out.push_str(&ip.to_string());
            out.push('\n');
        }
        for ip in &self.ipv6 {
            out.push_str("ip ");
            out.push_str(&ip.to_string());
            out.push('\n');
        }
        out
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        if lines.next()?.trim() != "etherquic" {
            return None;
        }
        let mut name = None;
        let mut mac = None;
        let mut service = None;
        let mut port = None;
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        for line in lines {
            let (key, value) = line.trim().split_once(' ')?;
            if value.is_empty() {
                return None;
            }
            match key {
                "name" => {
                    if name.is_some() || value.len() > NAME_LIMIT {
                        return None;
                    }
                    name = Some(value.to_string());
                }
                "mac" => {
                    if mac.is_some() {
                        return None;
                    }
                    mac = Some(parse_mac(value)?);
                }
                "service" => {
                    if service.is_some() {
                        return None;
                    }
                    service = Some(value.parse().ok()?);
                }
                "port" => {
                    if port.is_some() {
                        return None;
                    }
                    port = Some(value.parse().ok()?);
                }
                "ip" => match value.parse::<IpAddr>().ok()? {
                    IpAddr::V4(ip) => ipv4.push(ip),
                    IpAddr::V6(ip) => ipv6.push(ip),
                },
                _ => return None,
            }
        }
        Some(Self {
            name: name.unwrap_or_default(),
            mac: mac?,
            service: service?,
            port: port?,
            ipv4,
            ipv6,
        })
    }

    /// UDP address a phone dials from this card.
    ///
    /// Prefers a global IPv4 address, then any IPv4, then a non-link-local
    /// IPv6 address. Link-local IPv6 needs a scope id the card does not carry.
    pub fn udp_peer(&self) -> Option<SocketAddr> {
        let port = self.port;
        let v4 = self
            .ipv4
            .iter()
            .copied()
            .find(|ip| !ip.is_link_local())
            .or_else(|| self.ipv4.first().copied());
        if let Some(ip) = v4 {
            return Some(SocketAddr::from((ip, port)));
        }
        self.ipv6
            .iter()
            .copied()
            .find(|ip| !ip.is_unicast_link_local())
            .map(|ip| SocketAddr::from((ip, port)))
    }
}

fn l3_value<T: ToString>(addrs: &[T], port: u16) -> String {
    if addrs.is_empty() {
        return "no address".into();
    }
    let hosts = addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{hosts} · port {port}")
}

fn format_mac(mac: Mac) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Newlines would split a field. Spaces keep the name on one line, then the discovery cap applies.
pub fn offer_name(name: &str) -> String {
    let flat: String = name
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect();
    let flat = flat.trim();
    let mut end = flat.len().min(NAME_LIMIT);
    while end > 0 && !flat.is_char_boundary(end) {
        end -= 1;
    }
    flat[..end].to_string()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    use super::*;

    fn sample() -> Share {
        Share {
            name: "o11d".into(),
            mac: DEFAULT_MAC,
            service: DEFAULT_SERVICE,
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
            mac: DEFAULT_MAC,
            service: DEFAULT_SERVICE,
            port: DEFAULT_SERVICE,
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
        assert!(share.udp_peer().is_none());
    }

    #[test]
    fn a_newline_in_the_name_stays_one_field() {
        let share = Share {
            name: "a\nb and a very long name that exceeds thirty two bytes".into(),
            mac: DEFAULT_MAC,
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
    fn udp_peer_prefers_global_ipv4_and_skips_link_local_ipv6() {
        let share = sample();
        assert_eq!(
            share.udp_peer(),
            Some(SocketAddr::from((Ipv4Addr::new(192, 168, 0, 24), 4434)))
        );
        let v6_only = Share {
            name: String::new(),
            mac: DEFAULT_MAC,
            service: DEFAULT_SERVICE,
            port: 4434,
            ipv4: Vec::new(),
            ipv6: vec![
                "fe80::1".parse().unwrap(),
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
            ],
        };
        assert_eq!(
            v6_only.udp_peer(),
            Some(SocketAddr::from((
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
                4434
            )))
        );
        let link_v4 = Share {
            ipv4: vec![Ipv4Addr::new(169, 254, 1, 1)],
            ipv6: vec!["fe80::1".parse().unwrap()],
            ..Share::blank()
        };
        assert_eq!(
            link_v4.udp_peer(),
            Some(SocketAddr::from((
                Ipv4Addr::new(169, 254, 1, 1),
                DEFAULT_SERVICE
            )))
        );
    }
}
