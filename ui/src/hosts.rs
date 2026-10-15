//! Saved and recent hosts, and the discovered roster.
//!
//! Nothing here touches the window, so the list rules are unit tested.
//! Saved and Recent live in `hosts.txt` next to `preferences.txt`. Each line is
//! `kind<TAB>unix-seconds<TAB>address<TAB>name`, where kind is `saved` or `recent`.
//! A line this build cannot read is skipped, not fatal.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use crate::session::format_mac;
use etherquic::discover::Capabilities;

/// Recent keeps at most this many hosts, newest first.
pub const RECENT_CAP: usize = 10;
/// Longest name kept, in characters.
pub const NAME_MAX: usize = 64;

/// Where a host is reached. A MAC dials over Ethernet (L2); an IP dials UDP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAddr {
    Mac([u8; 6]),
    /// `port` `0` means the play service port.
    Ip { ip: IpAddr, port: u16 },
}

impl HostAddr {
    /// Accepts `aa:bb:cc:dd:ee:ff`, `aa-bb-…`, `aabbccddeeff`, `192.168.0.5`,
    /// `192.168.0.5:4433`, `fe80::1`, and `[fe80::1]:4433`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Enter a MAC address or an IP address.".into());
        }
        if !text.contains("::") {
            if let Some(mac) = parse_mac(text) {
                return Ok(Self::Mac(mac));
            }
        }
        if let Ok(addr) = text.parse::<SocketAddr>() {
            return Ok(Self::Ip {
                ip: addr.ip(),
                port: addr.port(),
            });
        }
        if let Ok(ip) = text.parse::<IpAddr>() {
            return Ok(Self::Ip { ip, port: 0 });
        }
        if let Some(inner) = text.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            if let Ok(ip) = inner.parse::<Ipv6Addr>() {
                return Ok(Self::Ip {
                    ip: IpAddr::V6(ip),
                    port: 0,
                });
            }
        }
        Err(format!(
            "\u{201c}{text}\u{201d} is not a MAC address (aa:bb:cc:dd:ee:ff) or an IP address (192.168.0.5:4433)."
        ))
    }

    pub fn display(&self) -> String {
        match *self {
            Self::Mac(mac) => format_mac(mac),
            Self::Ip { ip, port: 0 } => ip.to_string(),
            Self::Ip {
                ip: IpAddr::V4(ip),
                port,
            } => format!("{ip}:{port}"),
            Self::Ip {
                ip: IpAddr::V6(ip),
                port,
            } => format!("[{ip}]:{port}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Mac(_) => "Ethernet",
            Self::Ip { ip: IpAddr::V4(_), .. } => "IPv4",
            Self::Ip { ip: IpAddr::V6(_), .. } => "IPv6",
        }
    }

    /// Two addresses name the same host. A port of `0` matches any port.
    pub fn same_host(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::Mac(a), Self::Mac(b)) => a == b,
            (Self::Ip { ip: a, port: pa }, Self::Ip { ip: b, port: pb }) => {
                a == b && (pa == pb || pa == 0 || pb == 0)
            }
            _ => false,
        }
    }
}

/// `aa:bb:cc:dd:ee:ff`, `aa-bb-cc-dd-ee-ff`, `aabb.ccdd.eeff`, or bare
/// `aabbccddeeff`. The groups must have that shape, so an address such as
/// `192.168.0.5:4433`, which also has twelve digits, is not taken for a MAC.
fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let groups: Vec<&str> = if text.contains(':') {
        text.split(':').collect()
    } else if text.contains('-') {
        text.split('-').collect()
    } else if text.contains('.') {
        text.split('.').collect()
    } else {
        vec![text]
    };
    let width = match groups.len() {
        6 => 2,
        3 => 4,
        1 => 12,
        _ => return None,
    };
    if text.contains('.') != (groups.len() == 3) {
        return None;
    }
    if !groups
        .iter()
        .all(|group| group.len() == width && group.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return None;
    }
    let digits: String = groups.concat();
    let mut mac = [0u8; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(mac)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEntry {
    pub name: String,
    pub addr: HostAddr,
    /// Unix seconds: when it was saved, or the last successful connect for Recent.
    pub at: u64,
}

/// Tabs and newlines would break the file. Blank names fall back to the address.
pub fn clean_name(name: &str, addr: &HostAddr) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned: String = cleaned.chars().take(NAME_MAX).collect();
    if cleaned.is_empty() {
        addr.display()
    } else {
        cleaned
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostBook {
    pub saved: Vec<HostEntry>,
    /// Newest first, at most [`RECENT_CAP`].
    pub recent: Vec<HostEntry>,
}

impl HostBook {
    /// A successful connect. The host moves to the front; older duplicates go.
    pub fn record_recent(&mut self, name: &str, addr: HostAddr, now: u64) {
        let name = clean_name(name, &addr);
        self.recent.retain(|entry| !entry.addr.same_host(&addr));
        self.recent.insert(0, HostEntry { name, addr, at: now });
        self.recent.truncate(RECENT_CAP);
    }

    pub fn remove_recent(&mut self, index: usize) -> bool {
        if index < self.recent.len() {
            self.recent.remove(index);
            true
        } else {
            false
        }
    }

    pub fn saved_index(&self, addr: &HostAddr) -> Option<usize> {
        self.saved.iter().position(|entry| entry.addr.same_host(addr))
    }

    /// Adds to Saved and returns its index. An address that is already saved
    /// keeps its entry and its name.
    pub fn save(&mut self, name: &str, addr: HostAddr, now: u64) -> usize {
        if let Some(index) = self.saved_index(&addr) {
            return index;
        }
        let name = clean_name(name, &addr);
        self.saved.push(HostEntry { name, addr, at: now });
        self.saved.len() - 1
    }

    pub fn rename_saved(&mut self, index: usize, name: &str) -> bool {
        let Some(entry) = self.saved.get_mut(index) else {
            return false;
        };
        entry.name = clean_name(name, &entry.addr);
        true
    }

    pub fn delete_saved(&mut self, index: usize) -> bool {
        if index < self.saved.len() {
            self.saved.remove(index);
            true
        } else {
            false
        }
    }

    /// Saved in name order so the list does not move when one is renamed elsewhere.
    pub fn saved_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.saved.len()).collect();
        order.sort_by(|&a, &b| {
            let left = &self.saved[a];
            let right = &self.saved[b];
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.at.cmp(&right.at))
        });
        order
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# etherquic hosts: kind, unix seconds, address, name\n");
        for (kind, list) in [("saved", &self.saved), ("recent", &self.recent)] {
            for entry in list {
                out.push_str(&format!(
                    "{kind}\t{}\t{}\t{}\n",
                    entry.at,
                    entry.addr.display(),
                    clean_name(&entry.name, &entry.addr)
                ));
            }
        }
        out
    }

    pub fn parse(text: &str) -> Self {
        let mut book = Self::default();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(4, '\t');
            let (Some(kind), Some(at), Some(addr), name) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Ok(addr) = HostAddr::parse(addr) else {
                continue;
            };
            let at = at.trim().parse().unwrap_or(0);
            let name = clean_name(name.unwrap_or(""), &addr);
            match kind.trim() {
                "saved" if book.saved_index(&addr).is_none() => {
                    book.saved.push(HostEntry { name, addr, at });
                }
                "recent" if !book.recent.iter().any(|entry| entry.addr.same_host(&addr)) => {
                    book.recent.push(HostEntry { name, addr, at });
                }
                _ => {}
            }
        }
        book.recent.sort_by_key(|entry| std::cmp::Reverse(entry.at));
        book.recent.truncate(RECENT_CAP);
        book
    }

    pub fn load_at(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// Written to a temporary file and renamed, so a crash leaves the old list.
    pub fn store_at(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", parent.display()))?;
        }
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, self.render())
            .map_err(|err| format!("write {}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|err| format!("save {}: {err}", path.display()))?;
        Ok(())
    }
}

/// `hosts.txt` in the same folder as `preferences.txt`
/// (`$XDG_CONFIG_HOME/etherquic`, `~/.config/etherquic`, or `%APPDATA%\etherquic`).
pub fn hosts_path() -> Option<PathBuf> {
    crate::prefs::preferences_path().map(|path| path.with_file_name("hosts.txt"))
}

pub fn load_hosts() -> HostBook {
    hosts_path()
        .map(|path| HostBook::load_at(&path))
        .unwrap_or_default()
}

pub fn store_hosts(book: &HostBook) -> Result<(), String> {
    let path = hosts_path().ok_or("no home directory for hosts")?;
    book.store_at(&path)
}

/// One host in one discovery burst.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sighting {
    /// Tells hosts apart. Every host announces the same Ethernet identity,
    /// so this is the adapter MAC when the host reports one.
    pub key: [u8; 6],
    pub mac: [u8; 6],
    pub caps: Option<Capabilities>,
    pub thumbnail: Option<Vec<u8>>,
    pub name: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterRow {
    pub key: [u8; 6],
    pub mac: [u8; 6],
    pub caps: Option<Capabilities>,
    /// The latest picture the host sent; kept when a burst brings none.
    pub thumbnail: Option<Vec<u8>>,
    pub name: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub port: u16,
    /// Answered the latest burst.
    pub online: bool,
    /// Unix seconds of the last burst it answered.
    pub last_seen: u64,
}

impl RosterRow {
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            format_mac(self.mac)
        } else {
            self.name.clone()
        }
    }

    pub fn address_line(&self) -> String {
        let mut parts = vec![format_mac(self.mac)];
        if let Some(ip) = self.ipv4.first() {
            parts.push(ip.to_string());
        } else if let Some(ip) = self.ipv6.first() {
            parts.push(ip.to_string());
        }
        parts.join(" · ")
    }

    pub fn answers(&self, addr: &HostAddr) -> bool {
        match *addr {
            HostAddr::Mac(mac) => mac == self.mac || mac == self.key,
            HostAddr::Ip { ip, port } => {
                let listed = match ip {
                    IpAddr::V4(ip) => self.ipv4.contains(&ip),
                    IpAddr::V6(ip) => self.ipv6.contains(&ip),
                };
                listed && (port == 0 || self.port == 0 || port == self.port)
            }
        }
    }
}

/// Every host seen this session. A host that misses a burst stays listed as
/// offline, so rows do not vanish and reappear while discovery runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    rows: Vec<RosterRow>,
}

impl Roster {
    pub fn update(&mut self, sightings: &[Sighting], now: u64) {
        for row in &mut self.rows {
            row.online = false;
        }
        for seen in sightings {
            match self.rows.iter_mut().find(|row| row.key == seen.key) {
                Some(row) => {
                    row.mac = seen.mac;
                    if seen.caps.is_some() {
                        row.caps = seen.caps.clone();
                    }
                    if seen.thumbnail.is_some() {
                        row.thumbnail = seen.thumbnail.clone();
                    }
                    row.name = seen.name.clone();
                    row.ipv4 = seen.ipv4.clone();
                    row.ipv6 = seen.ipv6.clone();
                    row.port = seen.port;
                    row.online = true;
                    row.last_seen = now;
                }
                None => self.rows.push(RosterRow {
                    key: seen.key,
                    mac: seen.mac,
                    caps: seen.caps.clone(),
                    thumbnail: seen.thumbnail.clone(),
                    name: seen.name.clone(),
                    ipv4: seen.ipv4.clone(),
                    ipv6: seen.ipv6.clone(),
                    port: seen.port,
                    online: true,
                    last_seen: now,
                }),
            }
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn online_count(&self) -> usize {
        self.rows.iter().filter(|row| row.online).count()
    }

    /// Online first, by name, then the address, so a burst does not shuffle them.
    /// Offline after, most recently seen first.
    pub fn ordered(&self) -> Vec<&RosterRow> {
        let mut rows: Vec<&RosterRow> = self.rows.iter().collect();
        rows.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| {
                    if a.online {
                        std::cmp::Ordering::Equal
                    } else {
                        b.last_seen.cmp(&a.last_seen)
                    }
                })
                .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
                .then_with(|| a.key.cmp(&b.key))
        });
        rows
    }

    pub fn find(&self, addr: &HostAddr) -> Option<&RosterRow> {
        self.rows
            .iter()
            .filter(|row| row.answers(addr))
            .max_by_key(|row| (row.online, row.last_seen))
    }

    pub fn presence(&self, addr: &HostAddr) -> Presence {
        match self.find(addr) {
            Some(row) if row.online => Presence::Online,
            Some(row) => Presence::Seen(row.last_seen),
            None => Presence::Unseen,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// Answered the latest discovery burst.
    Online,
    /// Answered an earlier burst this session, at these unix seconds.
    Seen(u64),
    /// Not discovered this session.
    Unseen,
}

impl Presence {
    pub fn label(&self, now: u64) -> String {
        match *self {
            Self::Online => "Online".into(),
            Self::Seen(at) => format!("Seen {}", ago(now, at)),
            Self::Unseen => "Not discovered".into(),
        }
    }
}

/// `just now`, `5 min ago`, `3 h ago`, `2 d ago`.
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// Every whitespace-separated word of `filter` is in at least one field, ignoring case.
pub fn matches_filter(filter: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    filter.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        fields.iter().any(|field| field.contains(&word))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0a];
    const B: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0b];
    const C: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0c];

    fn seen(mac: [u8; 6], name: &str, ip: Option<&str>) -> Sighting {
        Sighting {
            key: mac,
            mac,
            caps: None,
            thumbnail: None,
            name: name.into(),
            ipv4: ip.map(|ip| vec![ip.parse().unwrap()]).unwrap_or_default(),
            ipv6: Vec::new(),
            port: 4433,
        }
    }

    #[test]
    fn addresses_parse_for_every_connect_path() {
        let mac = HostAddr::Mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(HostAddr::parse("aa:bb:cc:dd:ee:ff"), Ok(mac));
        assert_eq!(HostAddr::parse("AA-BB-CC-DD-EE-FF"), Ok(mac));
        assert_eq!(HostAddr::parse(" aabbccddeeff "), Ok(mac));
        assert_eq!(HostAddr::parse("aabb.ccdd.eeff"), Ok(mac));
        assert!(HostAddr::parse("aa:bb:cc:dd:ee").is_err());
        assert!(HostAddr::parse("aa:bb-cc:dd:ee:ff").is_err());
        assert_eq!(
            HostAddr::parse("192.168.0.5:4433"),
            Ok(HostAddr::Ip {
                ip: "192.168.0.5".parse().unwrap(),
                port: 4433
            })
        );
        assert_eq!(
            HostAddr::parse("192.168.0.5"),
            Ok(HostAddr::Ip {
                ip: "192.168.0.5".parse().unwrap(),
                port: 0
            })
        );
        assert_eq!(
            HostAddr::parse("[fe80::1]:4433").unwrap().display(),
            "[fe80::1]:4433"
        );
        assert_eq!(HostAddr::parse("fe80::1").unwrap().display(), "fe80::1");
        assert_eq!(HostAddr::parse("[fe80::1]").unwrap().display(), "fe80::1");
        assert!(HostAddr::parse("").is_err());
        assert!(HostAddr::parse("aa:bb:cc").is_err());
        assert!(HostAddr::parse("my-pc").is_err());
        for text in ["aa:bb:cc:dd:ee:ff", "10.0.0.2:9000", "[2604::5]:4433", "10.0.0.2"] {
            let addr = HostAddr::parse(text).unwrap();
            assert_eq!(HostAddr::parse(&addr.display()), Ok(addr));
        }
    }

    #[test]
    fn port_zero_matches_any_port() {
        let any = HostAddr::parse("10.0.0.2").unwrap();
        let fixed = HostAddr::parse("10.0.0.2:4433").unwrap();
        let other = HostAddr::parse("10.0.0.2:5000").unwrap();
        assert!(any.same_host(&fixed));
        assert!(!fixed.same_host(&other));
        assert!(!HostAddr::Mac(A).same_host(&any));
    }

    #[test]
    fn recent_is_newest_first_deduplicated_and_capped() {
        let mut book = HostBook::default();
        for n in 0..15u8 {
            book.record_recent(&format!("pc{n}"), HostAddr::Mac([2, 0, 0, 0, 0, n]), n as u64);
        }
        assert_eq!(book.recent.len(), RECENT_CAP);
        assert_eq!(book.recent[0].name, "pc14");
        assert_eq!(book.recent[9].name, "pc5");
        book.record_recent("again", HostAddr::Mac([2, 0, 0, 0, 0, 7]), 100);
        assert_eq!(book.recent.len(), RECENT_CAP);
        assert_eq!(book.recent[0].name, "again");
        assert_eq!(
            book.recent
                .iter()
                .filter(|entry| entry.addr == HostAddr::Mac([2, 0, 0, 0, 0, 7]))
                .count(),
            1
        );
        assert!(book.remove_recent(0));
        assert_eq!(book.recent[0].name, "pc14");
        assert!(!book.remove_recent(99));
    }

    #[test]
    fn saved_add_rename_delete() {
        let mut book = HostBook::default();
        let first = book.save("Desk", HostAddr::Mac(A), 1);
        assert_eq!(book.save("Other name", HostAddr::Mac(A), 2), first);
        assert_eq!(book.saved.len(), 1);
        assert_eq!(book.saved[0].name, "Desk");
        book.save("", HostAddr::parse("10.0.0.9:4433").unwrap(), 3);
        assert_eq!(book.saved[1].name, "10.0.0.9:4433");
        assert!(book.rename_saved(0, "  Living\troom\n PC "));
        assert_eq!(book.saved[0].name, "Living room PC");
        assert!(book.rename_saved(0, "   "));
        assert_eq!(book.saved[0].name, "02:00:00:00:00:0a");
        assert!(book.delete_saved(1));
        assert_eq!(book.saved.len(), 1);
        assert!(!book.delete_saved(5));
        assert!(!book.rename_saved(5, "x"));
        let long: String = "x".repeat(200);
        book.rename_saved(0, &long);
        assert_eq!(book.saved[0].name.chars().count(), NAME_MAX);
    }

    #[test]
    fn saved_order_is_by_name() {
        let mut book = HostBook::default();
        book.save("zeta", HostAddr::Mac(A), 1);
        book.save("Alpha", HostAddr::Mac(B), 2);
        book.save("beta", HostAddr::Mac(C), 3);
        let names: Vec<_> = book
            .saved_order()
            .into_iter()
            .map(|index| book.saved[index].name.as_str())
            .collect();
        assert_eq!(names, ["Alpha", "beta", "zeta"]);
    }

    #[test]
    fn book_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("eq-hosts-{}", std::process::id()));
        let path = dir.join("etherquic").join("hosts.txt");
        let mut book = HostBook::default();
        book.save("Desk PC", HostAddr::Mac(A), 10);
        book.save("Lab", HostAddr::parse("[fe80::2]:4433").unwrap(), 11);
        book.record_recent("old", HostAddr::Mac(B), 20);
        book.record_recent("new", HostAddr::parse("192.168.1.4:4433").unwrap(), 30);
        book.store_at(&path).unwrap();
        let loaded = HostBook::load_at(&path);
        assert_eq!(loaded, book);
        assert!(!path.with_extension("txt.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(HostBook::load_at(&path), HostBook::default());
    }

    #[test]
    fn unreadable_lines_are_skipped() {
        let text = "# header\n\
            saved\t5\taa:bb:cc:dd:ee:ff\tGood\n\
            saved\t5\tnot-an-address\tBad\n\
            mystery\t1\taa:bb:cc:dd:ee:01\tUnknown kind\n\
            saved\tx\t10.0.0.1\n\
            recent\t1\taa:bb:cc:dd:ee:02\tolder\r\n\
            recent\t9\taa:bb:cc:dd:ee:03\tnewer\n\
            recent\t3\taa:bb:cc:dd:ee:03\tduplicate\n\
            short line\n";
        let book = HostBook::parse(text);
        assert_eq!(book.saved.len(), 2);
        assert_eq!(book.saved[0].name, "Good");
        assert_eq!(book.saved[1].name, "10.0.0.1");
        assert_eq!(book.saved[1].at, 0);
        let names: Vec<_> = book.recent.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["newer", "older"]);
    }

    #[test]
    fn roster_keeps_missing_hosts_and_orders_stably() {
        let mut roster = Roster::default();
        roster.update(
            &[seen(B, "beta", None), seen(A, "alpha", None), seen(C, "gamma", None)],
            100,
        );
        let titles = |roster: &Roster| -> Vec<String> {
            roster.ordered().into_iter().map(|row| row.title()).collect()
        };
        assert_eq!(titles(&roster), ["alpha", "beta", "gamma"]);
        // A different answer order does not move anything.
        roster.update(
            &[seen(C, "gamma", None), seen(A, "alpha", None), seen(B, "beta", None)],
            110,
        );
        assert_eq!(titles(&roster), ["alpha", "beta", "gamma"]);
        // alpha misses a burst: it stays, offline, after the online hosts.
        roster.update(&[seen(C, "gamma", None), seen(B, "beta", None)], 120);
        assert_eq!(titles(&roster), ["beta", "gamma", "alpha"]);
        assert_eq!(roster.len(), 3);
        assert_eq!(roster.online_count(), 2);
        assert_eq!(roster.presence(&HostAddr::Mac(A)), Presence::Seen(110));
        // Later, gamma drops too. The most recently seen offline host comes first.
        roster.update(&[seen(B, "beta", None)], 130);
        assert_eq!(titles(&roster), ["beta", "gamma", "alpha"]);
        roster.update(&[], 140);
        assert_eq!(titles(&roster), ["beta", "gamma", "alpha"]);
        assert_eq!(roster.online_count(), 0);
    }

    #[test]
    fn presence_matches_mac_or_ip() {
        let mut roster = Roster::default();
        roster.update(&[seen(A, "alpha", Some("192.168.0.24"))], 50);
        assert_eq!(roster.presence(&HostAddr::Mac(A)), Presence::Online);
        assert_eq!(
            roster.presence(&HostAddr::parse("192.168.0.24").unwrap()),
            Presence::Online
        );
        assert_eq!(
            roster.presence(&HostAddr::parse("192.168.0.24:4433").unwrap()),
            Presence::Online
        );
        assert_eq!(
            roster.presence(&HostAddr::parse("192.168.0.24:9999").unwrap()),
            Presence::Unseen
        );
        assert_eq!(roster.presence(&HostAddr::Mac(B)), Presence::Unseen);
        assert_eq!(Presence::Seen(0).label(125), "Seen 2 min ago");
    }

    #[test]
    fn filter_matches_every_word_in_any_field() {
        assert!(matches_filter("", &["anything"]));
        assert!(matches_filter("desk", &["Desk PC", "aa:bb"]));
        assert!(matches_filter("DESK aa:b", &["Desk PC", "aa:bb"]));
        assert!(!matches_filter("desk lab", &["Desk PC", "aa:bb"]));
        assert!(matches_filter("192.168", &["x", "02:00 · 192.168.0.24"]));
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 200), "just now");
        assert_eq!(ago(4000, 0), "1 h ago");
        assert_eq!(ago(600, 0), "10 min ago");
        assert_eq!(ago(3 * 86_400, 0), "3 d ago");
    }

    #[test]
    fn hosts_sharing_an_identity_stay_apart() {
        let shared = [0x02, 0, 0, 0, 0, 1];
        let mut a = seen(shared, "hyte", Some("192.168.0.58"));
        a.key = [0x04, 0x7c, 0x16, 0, 0, 1];
        a.thumbnail = Some(vec![1, 2, 3]);
        let mut b = seen(shared, "an515", Some("192.168.0.77"));
        b.key = [0x9c, 0x2d, 0xcd, 0, 0, 2];
        let mut roster = Roster::default();
        roster.update(&[a.clone(), b.clone()], 100);
        assert_eq!(roster.len(), 2);
        // A later burst without a picture keeps the last one.
        a.thumbnail = None;
        roster.update(&[a.clone()], 200);
        let hyte = roster.find(&HostAddr::Mac(a.key)).unwrap();
        assert_eq!(hyte.thumbnail.as_deref(), Some(&[1u8, 2, 3][..]));
        assert!(hyte.online);
        assert!(!roster.find(&HostAddr::Mac(b.key)).unwrap().online);
    }
}
