//! Raw L2 I/O via Npcap (Windows) or libpcap (Linux), same shape as borderless-link.
//!
//! EtherType defaults to `0x88B6` so we do not collide with a live borderless
//! daemon on `0x88B5`.

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
}

impl Link {
    pub fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.cap.sendpacket(frame).map_err(map_pcap)
    }

    /// Next matching frame, or `Ok(None)` on timeout.
    pub fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        match self.cap.next_packet() {
            Ok(pkt) => Ok(Some(pkt.data.to_vec())),
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
    Ok(Link {
        cap,
        nic,
        ethertype,
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
