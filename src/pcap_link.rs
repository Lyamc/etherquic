//! Raw L2 I/O via Npcap (Windows), libpcap (Linux), or BPF + AF_NDRV (macOS).
//!
//! EtherType defaults to `0x88B6` so we do not collide with a live borderless
//! daemon on `0x88B5`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use thiserror::Error;

use crate::Mac;

/// Locally administered experimental EtherType (pair with borderless `0x88B5`).
pub const ETHERTYPE_QUIC: u16 = 0x88B6;

#[derive(Debug, Error)]
pub enum LinkError {
    #[error("pcap: {0}")]
    Pcap(String),
    #[error("interface not found: {0}")]
    NoDevice(String),
    #[error("no usable Ethernet adapter")]
    NoAdapter,
    #[error("permission denied — elevate / CAP_NET_RAW, and install Npcap on Windows")]
    Permission,
    #[error("Android needs root")]
    AndroidRoot,
}

pub type Result<T> = std::result::Result<T, LinkError>;

#[derive(Clone, Debug)]
pub struct Nic {
    pub name: String,
    pub description: String,
}

/// A physical cable or radio, a virtual adapter, or a capture pseudo-device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfaceKind {
    Physical,
    Virtual,
    Loopback,
    /// `any`, netfilter, and other devices that are not a link.
    Capture,
}

impl IfaceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Physical => "physical",
            Self::Virtual => "virtual",
            Self::Loopback => "loopback",
            Self::Capture => "capture",
        }
    }
}

/// One adapter the desk can listen on, with the addresses next to its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfaceFace {
    pub name: String,
    pub description: String,
    pub mac: Option<Mac>,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub kind: IfaceKind,
}

/// Classify a pcap or OS interface name. Virtual and loopback stay visible so
/// the desk can show them apart from a real link.
pub fn iface_kind(name: &str) -> IfaceKind {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return IfaceKind::Capture;
    }
    if lower == "lo"
        || lower.starts_with("loopback")
        || (lower.starts_with("lo")
            && lower.len() > 2
            && lower[2..].chars().all(|ch| ch.is_ascii_digit()))
    {
        return IfaceKind::Loopback;
    }
    const CAPTURE: &[&str] = &["any", "nflog", "nfqueue", "dbus", "pktap"];
    if CAPTURE.iter().any(|skip| lower == *skip) {
        return IfaceKind::Capture;
    }
    const VIRTUAL: &[&str] = &[
        "bluetooth",
        "docker",
        "veth",
        "virbr",
        "tun",
        "tap",
        "tailscale",
        "nordlynx",
        "br-",
        "wg",
        "awdl",
        "llw",
        "gif",
        "stf",
        "feth",
        "vmnet",
        "ap1",
        "bridge",
        "p2p",
        "anpi",
    ];
    if VIRTUAL
        .iter()
        .any(|skip| lower == *skip || lower.contains(skip))
    {
        IfaceKind::Virtual
    } else {
        IfaceKind::Physical
    }
}

/// Every adapter pcap lists, including virtual ones. Empty on Android.
pub fn list_faces() -> Result<Vec<IfaceFace>> {
    #[cfg(target_os = "android")]
    {
        Ok(Vec::new())
    }
    #[cfg(not(target_os = "android"))]
    {
        let devs = pcap::Device::list().map_err(map_pcap)?;
        Ok(devs
            .into_iter()
            .map(|dev| {
                let mut ipv4 = Vec::new();
                let mut ipv6 = Vec::new();
                for address in &dev.addresses {
                    match address.addr {
                        IpAddr::V4(ip) if usable_v4(ip) => ipv4.push(ip),
                        IpAddr::V6(ip) if usable_v6(ip) => ipv6.push(ip),
                        _ => {}
                    }
                }
                ipv4.sort_by_key(|ip| u8::from(ip.is_link_local()));
                ipv6.sort_by_key(|ip| u8::from(ip.is_unicast_link_local()));
                ipv4.truncate(4);
                ipv6.truncate(4);
                let name = dev.name;
                IfaceFace {
                    mac: read_hw_mac(&name),
                    description: dev.desc.unwrap_or_default(),
                    kind: iface_kind(&name),
                    name,
                    ipv4,
                    ipv6,
                }
            })
            .collect())
    }
}

/// Adapter chosen for an announcement: hardware MAC when sysfs has it, plus its addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    pub mac: Option<Mac>,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
}

/// Why this process cannot send raw Ethernet. `None` when L2 can be used.
pub fn l2_unavailable_reason() -> Option<&'static str> {
    reason_for(current_os(), packet_caps())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostOs {
    Android,
    Linux,
    Mac,
    Windows,
    Other,
}

fn current_os() -> HostOs {
    if cfg!(target_os = "android") {
        HostOs::Android
    } else if cfg!(target_os = "linux") {
        HostOs::Linux
    } else if cfg!(target_os = "macos") {
        HostOs::Mac
    } else if cfg!(windows) {
        HostOs::Windows
    } else {
        HostOs::Other
    }
}

fn packet_caps() -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        capeff_has_packet(&std::fs::read_to_string("/proc/self/status").unwrap_or_default())
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos_link::bpf_available()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        true
    }
}

/// The named adapter, or the first real one that has a global IPv4 address.

/// `CAP_NET_ADMIN` (12) and `CAP_NET_RAW` (13) in `CapEff`.
fn capeff_has_packet(status: &str) -> bool {
    const CAP_NET_ADMIN: u64 = 12;
    const CAP_NET_RAW: u64 = 13;
    for line in status.lines() {
        let Some(hex) = line.strip_prefix("CapEff:").map(str::trim) else {
            continue;
        };
        let Ok(caps) = u64::from_str_radix(hex, 16) else {
            return false;
        };
        return caps & (1 << CAP_NET_ADMIN) != 0 && caps & (1 << CAP_NET_RAW) != 0;
    }
    false
}

fn reason_for(os: HostOs, packet_caps: bool) -> Option<&'static str> {
    match os {
        HostOs::Android => Some("Android needs root"),
        HostOs::Linux if !packet_caps => Some("needs CAP_NET_RAW (or root)"),
        HostOs::Mac if !packet_caps => Some("macOS BPF needs root"),
        _ => None,
    }
}

pub fn interface_info(want: Option<&str>) -> Result<InterfaceInfo> {
    #[cfg(target_os = "android")]
    {
        let _ = want;
        Err(LinkError::AndroidRoot)
    }
    #[cfg(not(target_os = "android"))]
    {
        let devs = pcap::Device::list().map_err(map_pcap)?;
        let dev = choose_device(devs, want)?;
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        for address in &dev.addresses {
            match address.addr {
                IpAddr::V4(ip) if usable_v4(ip) => ipv4.push(ip),
                IpAddr::V6(ip) if usable_v6(ip) => ipv6.push(ip),
                _ => {}
            }
        }
        ipv4.sort_by_key(|ip| u8::from(ip.is_link_local()));
        ipv6.sort_by_key(|ip| u8::from(ip.is_unicast_link_local()));
        ipv4.truncate(4);
        ipv6.truncate(4);
        Ok(InterfaceInfo {
            mac: read_hw_mac(&dev.name),
            name: dev.name,
            ipv4,
            ipv6,
        })
    }
}

/// Every usable address Npcap or libpcap lists, on every adapter.
/// Empty when the devices cannot be listed.
pub fn local_ips() -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    #[cfg(target_os = "android")]
    {
        (Vec::new(), Vec::new())
    }
    #[cfg(not(target_os = "android"))]
    {
        let Ok(devs) = pcap::Device::list() else {
            return (Vec::new(), Vec::new());
        };
        let mut ipv4 = Vec::new();
        let mut ipv6 = Vec::new();
        for dev in devs {
            for address in &dev.addresses {
                match address.addr {
                    IpAddr::V4(ip) if usable_v4(ip) && !ipv4.contains(&ip) => ipv4.push(ip),
                    IpAddr::V6(ip) if usable_v6(ip) && !ipv6.contains(&ip) => ipv6.push(ip),
                    _ => {}
                }
            }
        }
        (ipv4, ipv6)
    }
}

#[cfg(not(target_os = "android"))]
fn choose_device(devs: Vec<pcap::Device>, want: Option<&str>) -> Result<pcap::Device> {
    if let Some(want) = want {
        return devs
            .into_iter()
            .find(|dev| {
                dev.name == want
                    || dev.name.contains(want)
                    || dev.desc.as_deref().unwrap_or("").contains(want)
            })
            .ok_or_else(|| LinkError::NoDevice(want.into()));
    }
    let mut usable: Vec<_> = devs
        .into_iter()
        .filter(|dev| cable_nic(&dev.name))
        .collect();
    if usable.is_empty() {
        return Err(LinkError::NoAdapter);
    }
    usable.sort_by_key(|dev| {
        let global_v4 = dev.addresses.iter().any(|address| {
            matches!(address.addr, IpAddr::V4(ip) if usable_v4(ip) && !ip.is_link_local())
        });
        u8::from(!global_v4)
    });
    usable.into_iter().next().ok_or(LinkError::NoAdapter)
}

#[cfg(not(target_os = "android"))]
fn cable_nic(name: &str) -> bool {
    iface_kind(name) == IfaceKind::Physical
}

#[cfg(not(target_os = "android"))]
fn usable_v4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast()
}

#[cfg(not(target_os = "android"))]
fn usable_v6(ip: Ipv6Addr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast()
}

/// The adapter's own MAC: sysfs on Linux, `ifconfig` on macOS.
pub fn read_hw_mac(name: &str) -> Option<Mac> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let text = std::fs::read_to_string(format!("/sys/class/net/{name}/address")).ok()?;
        parse_mac(text.trim())
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/sbin/ifconfig")
            .arg(name)
            .output()
            .ok()?;
        ifconfig_ether(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = name;
        None
    }
}

/// The `ether aa:bb:…` line of BSD `ifconfig` output.
pub fn ifconfig_ether(text: &str) -> Option<Mac> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some("ether"))
            .then(|| words.next().and_then(parse_mac))
            .flatten()
    })
}

pub fn list_nics() -> Result<Vec<Nic>> {
    #[cfg(target_os = "android")]
    {
        Ok(Vec::new())
    }
    #[cfg(not(target_os = "android"))]
    {
        let devs = pcap::Device::list().map_err(map_pcap)?;
        Ok(devs
            .into_iter()
            .map(|d| Nic {
                name: d.name,
                description: d.desc.unwrap_or_default(),
            })
            .collect())
    }
}

/// Prefer a named device; otherwise the first non-loopback NIC.
pub fn open(name: Option<&str>, ethertype: u16) -> Result<Link> {
    #[cfg(target_os = "android")]
    {
        let _ = (name, ethertype);
        Err(LinkError::AndroidRoot)
    }
    #[cfg(not(target_os = "android"))]
    {
        let info = interface_info(name)?;
        open_nic(
            Nic {
                name: info.name,
                description: String::new(),
            },
            ethertype,
        )
    }
}

#[cfg(target_os = "android")]
pub struct Link {
    pub nic: Nic,
    pub ethertype: u16,
    /// The adapter's own MAC, when the OS reports it.
    pub hw_mac: Option<Mac>,
}

#[cfg(target_os = "android")]
impl Link {
    pub fn send(&mut self, _frame: &[u8]) -> Result<()> {
        Err(LinkError::AndroidRoot)
    }

    pub fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        Err(LinkError::AndroidRoot)
    }
}

#[cfg(not(any(target_os = "android", target_os = "macos")))]
pub struct Link {
    cap: pcap::Capture<pcap::Active>,
    pub nic: Nic,
    pub ethertype: u16,
    /// The adapter's own MAC, when the OS reports it.
    pub hw_mac: Option<Mac>,
}

#[cfg(target_os = "macos")]
pub struct Link {
    inner: crate::macos_link::MacLink,
    pub nic: Nic,
    pub ethertype: u16,
    /// The adapter's own MAC, when the OS reports it.
    pub hw_mac: Option<Mac>,
}

#[cfg(not(any(target_os = "android", target_os = "macos")))]
impl Link {
    pub fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.cap.sendpacket(frame).map_err(map_pcap)?;
        crate::capture::note_ether(crate::capture::DIR_SEND, frame);
        Ok(())
    }

    /// pcap fd the Linux I/O thread polls together with the send-wake pipe.
    #[cfg(target_os = "linux")]
    pub fn poll_fd(&self) -> std::os::unix::io::RawFd {
        std::os::unix::io::AsRawFd::as_raw_fd(&self.cap)
    }

    /// Next matching frame, or `Ok(None)` on timeout.
    pub fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        match self.cap.next_packet() {
            Ok(pkt) => {
                let data = pkt.data.to_vec();
                crate::capture::note_ether(crate::capture::DIR_RECV, &data);
                Ok(Some(data))
            }
            Err(pcap::Error::TimeoutExpired) => Ok(None),
            Err(e) => Err(map_pcap(e)),
        }
    }
}

#[cfg(target_os = "macos")]
impl Link {
    pub fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.inner.send(frame)?;
        crate::capture::note_ether(crate::capture::DIR_SEND, frame);
        Ok(())
    }

    /// Next matching frame, or `Ok(None)` when the socket is empty.
    pub fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        match self.inner.recv()? {
            Some(data) => {
                crate::capture::note_ether(crate::capture::DIR_RECV, &data);
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "macos")))]
fn open_nic(nic: Nic, ethertype: u16) -> Result<Link> {
    // Immediate mode delivers a frame as soon as it arrives. Non-blocking reads
    // return TimeoutExpired when the socket is empty. A blocking read on Linux
    // can sit forever when the BPF filter matches nothing, which stalls the
    // send queue behind it.
    let mut cap = pcap::Capture::from_device(nic.name.as_str())
        .map_err(map_pcap)?
        .promisc(true)
        .snaplen(2048)
        .timeout(10)
        .immediate_mode(true)
        .open()
        .map_err(map_pcap)?;
    let filter = format!("ether proto 0x{ethertype:04x}");
    cap.filter(&filter, true).map_err(map_pcap)?;
    let cap = cap.setnonblock().map_err(map_pcap)?;
    let hw_mac = read_hw_mac(&nic.name);
    Ok(Link {
        cap,
        nic,
        ethertype,
        hw_mac,
    })
}

#[cfg(target_os = "macos")]
fn open_nic(nic: Nic, ethertype: u16) -> Result<Link> {
    let inner = crate::macos_link::MacLink::open(&nic.name, ethertype)?;
    let hw_mac = read_hw_mac(&nic.name);
    Ok(Link {
        inner,
        nic,
        ethertype,
        hw_mac,
    })
}

#[cfg(not(target_os = "android"))]
fn map_pcap(e: pcap::Error) -> LinkError {
    let s = e.to_string();
    let low = s.to_ascii_lowercase();
    if low.contains("permission") || low.contains("access") || low.contains("denied") {
        LinkError::Permission
    } else {
        LinkError::Pcap(s)
    }
}

/// Parse `aa:bb:cc:dd:ee:ff` into a MAC.
pub fn parse_mac(s: &str) -> Option<Mac> {
    let parts: Vec<_> = s.split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ifconfig_ether_line() {
        let text = "en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
                    \toptions=6460<TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINVERT_CSUM>\n\
                    \tether 3a:13:6f:4b:73:53\n\
                    \tinet 192.168.0.30 netmask 0xffffff00 broadcast 192.168.0.255\n";
        assert_eq!(
            ifconfig_ether(text),
            Some([0x3a, 0x13, 0x6f, 0x4b, 0x73, 0x53])
        );
        assert_eq!(
            ifconfig_ether("lo0: flags=8049<UP,LOOPBACK> mtu 16384\n"),
            None
        );
    }

    #[test]
    fn android_l2_names_root() {
        assert_eq!(
            reason_for(HostOs::Android, true),
            Some("Android needs root")
        );
        assert_eq!(
            reason_for(HostOs::Android, false),
            Some("Android needs root")
        );
    }

    #[test]
    fn linux_l2_names_the_missing_cap() {
        assert_eq!(
            reason_for(HostOs::Linux, false),
            Some("needs CAP_NET_RAW (or root)")
        );
        assert_eq!(reason_for(HostOs::Linux, true), None);
        assert_eq!(reason_for(HostOs::Windows, false), None);
        assert_eq!(reason_for(HostOs::Mac, true), None);
        assert_eq!(reason_for(HostOs::Mac, false), Some("macOS BPF needs root"));
    }

    #[test]
    fn iface_kind_separates_real_links_from_virtual_ones() {
        assert_eq!(iface_kind("enp5s0"), IfaceKind::Physical);
        assert_eq!(iface_kind("wlan0"), IfaceKind::Physical);
        assert_eq!(iface_kind("docker0"), IfaceKind::Virtual);
        assert_eq!(iface_kind("tailscale0"), IfaceKind::Virtual);
        assert_eq!(iface_kind("veth123"), IfaceKind::Virtual);
        assert_eq!(iface_kind("lo"), IfaceKind::Loopback);
        assert_eq!(iface_kind("lo0"), IfaceKind::Loopback);
        assert_eq!(iface_kind("any"), IfaceKind::Capture);
        assert_eq!(iface_kind("nflog"), IfaceKind::Capture);
    }

    #[test]
    fn cable_nic_skips_macos_pseudo_devices() {
        assert!(cable_nic("en0"));
        assert!(cable_nic("en8"));
        assert!(!cable_nic("any"));
        assert!(!cable_nic("lo0"));
        assert!(!cable_nic("awdl0"));
        assert!(!cable_nic("llw0"));
        assert!(!cable_nic("utun2"));
        assert!(!cable_nic("ap1"));
        assert!(!cable_nic("bridge0"));
        assert!(!cable_nic("feth0"));
    }

    #[test]
    fn capeff_line_needs_raw_and_admin() {
        assert!(!capeff_has_packet("Name:\tetherquic\n"));
        assert!(!capeff_has_packet("CapEff:\t0000000000000000\n"));
        // CAP_NET_ADMIN only.
        assert!(!capeff_has_packet("CapEff:\t0000000000001000\n"));
        // CAP_NET_RAW only.
        assert!(!capeff_has_packet("CapEff:\t0000000000002000\n"));
        assert!(capeff_has_packet("CapEff:\t0000000000003000\n"));
        assert!(capeff_has_packet("CapEff:\t0000001fffffffff\n"));
    }

    #[test]
    fn this_build_matches_the_os() {
        let why = l2_unavailable_reason();
        if cfg!(target_os = "android") {
            assert_eq!(why, Some("Android needs root"));
        } else {
            assert_eq!(why, reason_for(current_os(), packet_caps()));
        }
    }
}
