//! Host configuration. Paths are absolute: a Windows service starts in System32.
//!
//! The MAC derivation matches `setup/src/plan.rs`. Reinstalls keep a valid MAC.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

pub const DEFAULT_SERVICE: u16 = 4433;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
    pub mac: [u8; 6],
    pub nic: String,
    pub service: u16,
    /// When false, a player waits for `etherquic-host --approve`. Unattended accepts immediately.
    pub unattended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPaths {
    pub config: PathBuf,
    pub log: PathBuf,
    pub address: PathBuf,
}

impl HostPaths {
    /// `ETHERQUIC_CONFIG` selects the file. The log and address sit beside it.
    /// Otherwise everything is under `%ProgramData%\etherquic`.
    pub fn from_env() -> Self {
        if let Some(config) = std::env::var_os("ETHERQUIC_CONFIG") {
            let config = PathBuf::from(config);
            let dir = config
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            return Self {
                log: dir.join("host.log"),
                address: dir.join("address.txt"),
                config,
            };
        }
        let dir = program_data_dir();
        Self {
            config: dir.join("host.conf"),
            log: dir.join("host.log"),
            address: dir.join("address.txt"),
        }
    }

    /// Directory of `host.conf`. Approve and deny files live in `control/` beside it.
    pub fn dir(&self) -> PathBuf {
        self.config
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn control_dir(&self) -> PathBuf {
        self.dir().join("control")
    }
}

pub fn program_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("etherquic")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/etherquic")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/var/lib/etherquic")
    }
    #[cfg(not(any(windows, unix)))]
    {
        PathBuf::from("etherquic")
    }
}

/// Desktop-duplication subscription for the desk helper.
///
/// Read from `desk.conf` beside `host.conf`. A missing file, a missing
/// `duplication` key, or a value that is not yes/no means on.
/// `duplication=no` turns the subscription off. This file is not `host.conf`.
pub fn desk_duplication() -> bool {
    let path = program_data_dir().join("desk.conf");
    match std::fs::read_to_string(path) {
        Ok(text) => parse_duplication(&text),
        Err(_) => true,
    }
}

fn parse_duplication(text: &str) -> bool {
    let mut enabled = true;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("duplication") {
            enabled = parse_yes_no(value.trim()).unwrap_or(true);
        }
    }
    enabled
}

/// Fill a missing or `auto` MAC from the machine id. Keep a MAC that already parses.
pub fn resolve(existing: Option<&str>, machine_id: &str) -> Result<HostConfig, String> {
    let partial = match existing {
        Some(text) if !text.trim().is_empty() => parse_partial(text)?,
        _ => Partial::default(),
    };
    Ok(HostConfig {
        mac: partial
            .mac
            .unwrap_or_else(|| mac_from_machine_id(machine_id)),
        nic: partial.nic.unwrap_or_default(),
        service: partial.service.unwrap_or(DEFAULT_SERVICE),
        unattended: partial.unattended.unwrap_or(false),
    })
}

/// True when the file already records this MAC, adapter, and service.
pub fn already_written(text: &str, cfg: &HostConfig) -> bool {
    match parse_partial(text) {
        Ok(partial) => {
            partial.mac == Some(cfg.mac)
                && partial.nic.unwrap_or_default() == cfg.nic
                && partial.service.unwrap_or(DEFAULT_SERVICE) == cfg.service
                && partial.unattended.unwrap_or(false) == cfg.unattended
        }
        Err(_) => false,
    }
}

pub fn render(cfg: &HostConfig) -> String {
    format!(
        "mac={}\nnic={}\nservice={}\nunattended={}\n",
        format_mac(cfg.mac),
        cfg.nic,
        cfg.service,
        if cfg.unattended { "yes" } else { "no" }
    )
}

/// Local MAC for `--client`. Different from the host MAC so both can run on one PC.
pub fn client_mac(machine_id: &str) -> [u8; 6] {
    let id = normalize_machine_id(machine_id);
    mac_from_machine_id(&format!("{id}\nclient"))
}

pub fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Locally administered unicast MAC. Same FNV-1a derivation as the installer.
pub fn mac_from_machine_id(id: &str) -> [u8; 6] {
    let id = normalize_machine_id(id);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let bytes = hash.to_le_bytes();
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&bytes[..6]);
    mac[0] = (mac[0] & 0xfc) | 0x02;
    mac
}

pub fn normalize_machine_id(id: &str) -> String {
    id.trim()
        .trim_matches(['{', '}'])
        .trim()
        .to_ascii_lowercase()
}

#[derive(Default)]
struct Partial {
    mac: Option<[u8; 6]>,
    nic: Option<String>,
    service: Option<u16>,
    unattended: Option<bool>,
}

fn parse_partial(text: &str) -> Result<Partial, String> {
    let mut partial = Partial::default();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "host.conf line {}: expected key=value",
                line_no + 1
            ));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "mac" => {
                partial.mac =
                    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(parse_mac(value).ok_or_else(|| {
                            format!("host.conf line {}: invalid mac", line_no + 1)
                        })?)
                    };
            }
            "nic" => partial.nic = Some(value.to_string()),
            "service" => {
                partial.service = Some(value.parse::<u16>().map_err(|_| {
                    format!("host.conf line {}: service is not a number", line_no + 1)
                })?);
            }
            "unattended" => {
                partial.unattended = Some(parse_yes_no(value).ok_or_else(|| {
                    format!(
                        "host.conf line {}: unattended is not yes or no",
                        line_no + 1
                    )
                })?);
            }
            _ => {}
        }
    }
    Ok(partial)
}

fn parse_yes_no(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Some(true),
        "no" | "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        if part.len() != 2 {
            return None;
        }
        mac[index] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE: &str = "00112233-4455-6677-8899-aabbccddeeff";
    const GENERATED: &str = "be:4e:c5:33:fb:a0";

    #[test]
    fn mac_matches_the_installer_vector() {
        assert_eq!(format_mac(mac_from_machine_id(MACHINE)), GENERATED);
        assert_eq!(format_mac(mac_from_machine_id("")), "26:23:22:84:e4:9c");
        let mac = mac_from_machine_id(MACHINE);
        assert_eq!(mac[0] & 0x01, 0);
        assert_eq!(mac[0] & 0x02, 0x02);
    }

    #[test]
    fn resolve_keeps_a_written_mac() {
        let text = "mac=02:11:22:33:44:55\nnic=Killer\nservice=4433\n";
        let cfg = resolve(Some(text), MACHINE).unwrap();
        assert_eq!(format_mac(cfg.mac), "02:11:22:33:44:55");
        assert_eq!(cfg.nic, "Killer");
        assert!(already_written(text, &cfg));
        assert_ne!(format_mac(cfg.mac), GENERATED);
    }

    #[test]
    fn resolve_generates_when_the_mac_is_missing() {
        let cfg = resolve(None, MACHINE).unwrap();
        assert_eq!(format_mac(cfg.mac), GENERATED);
        assert_eq!(cfg.service, DEFAULT_SERVICE);
        assert!(!cfg.unattended);
        assert!(!already_written("mac=auto\n", &cfg));
        assert_eq!(
            render(&cfg),
            format!("mac={GENERATED}\nnic=\nservice=4433\nunattended=no\n")
        );
    }

    #[test]
    fn unattended_yes_is_kept_and_client_mac_differs() {
        let text = "mac=02:11:22:33:44:55\nnic=\nservice=4433\nunattended=yes\n";
        let cfg = resolve(Some(text), MACHINE).unwrap();
        assert!(cfg.unattended);
        assert!(already_written(text, &cfg));
        let client = client_mac(MACHINE);
        assert_ne!(client, cfg.mac);
        assert_eq!(client[0] & 0x01, 0);
        assert_eq!(client[0] & 0x02, 0x02);
        assert_eq!(client_mac(&format!("{{{MACHINE}}}")), client);
    }

    #[test]
    fn duplication_stays_on_unless_desk_conf_says_no() {
        assert!(parse_duplication(""));
        assert!(parse_duplication("# duplication=no\n"));
        assert!(parse_duplication("other=no\n"));
        assert!(parse_duplication("duplication=maybe\n"));
        assert!(parse_duplication("duplication=yes\n"));
        assert!(parse_duplication("duplication=on\n"));
        assert!(!parse_duplication("duplication=no\n"));
        assert!(!parse_duplication("duplication=off\n"));
        assert!(!parse_duplication("  Duplication = FALSE \n"));
        assert!(parse_duplication("duplication=no\nduplication=maybe\n"));
        assert!(!parse_duplication("duplication=yes\nduplication=0\n"));
    }
}
