//! Raw L2 I/O via Npcap (Windows) or libpcap (Linux), same shape as borderless-link.
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
}

pub type Result<T> = std::result::Result<T, LinkError>;

#[derive(Clone, Debug)]
pub struct Nic {
    pub name: String,
    pub description: String,
}

/// Adapter chosen for an announcement: hardware MAC when sysfs has it, plus its addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    pub mac: Option<Mac>,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
}

/// The named adapter, or the first real one that has a global IPv4 address.
pub fn interface_info(want: Option<&str>) -> Result<InterfaceInfo> {
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

/// Every usable address Npcap or libpcap lists, on every adapter.
/// Empty when the devices cannot be listed.
pub fn local_ips() -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
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

fn cable_nic(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    const SKIP: &[&str] = &[
        "any",
        "lo",
        "nflog",
        "nfqueue",
        "dbus",
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
    ];
    !SKIP
        .iter()
        .any(|skip| lower == *skip || lower.contains(skip))
}

fn usable_v4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast()
}

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
    let devs = pcap::Device::list().map_err(map_pcap)?;
    Ok(devs
        .into_iter()
        .map(|d| Nic {
            name: d.name,
            description: d.desc.unwrap_or_default(),
        })
        .collect())
}

/// Prefer a named device; otherwise the first non-loopback NIC.
pub fn open(name: Option<&str>, ethertype: u16) -> Result<Link> {
    let nics = list_nics()?;
    let nic = if let Some(want) = name {
        nics.into_iter()
            .find(|n| n.name == want || n.name.contains(want) || n.description.contains(want))
            .ok_or_else(|| LinkError::NoDevice(want.into()))?
    } else {
        nics.into_iter()
            .find(|n| {
                let l = n.name.to_ascii_lowercase();
                !l.contains("loopback") && !l.contains("dbus") && !l.contains("nflog")
            })
            .ok_or(LinkError::NoAdapter)?
    };
    open_nic(nic, ethertype)
}

pub struct Link {
    cap: pcap::Capture<pcap::Active>,
    pub nic: Nic,
    pub ethertype: u16,
    /// The adapter's own MAC, when the OS reports it.
    pub hw_mac: Option<Mac>,
}

impl Link {
    pub fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.cap.sendpacket(frame).map_err(map_pcap)?;
        crate::capture::note_ether(crate::capture::DIR_SEND, frame);
        Ok(())
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
}
