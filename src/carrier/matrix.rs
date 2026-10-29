//! Supported Connect cells: protocol, layer, and profile.
//!
//! The rules are the ones the session already enforces. UDP/RTP has no L2
//! dial. UDP/RTP and ST 2110 have plain and gcm. QUIC and AVB do not.
//! Each cell is crossed with mjpeg, pyrowave, and h264.

use super::{AVB, QUIC_DATAGRAMS, QUIC_STREAMS, ST2110, UDP_RTP};

/// How Connect reaches the host for one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixLayer {
    L2,
    Ipv4,
    Ipv6,
}

/// Plain and gcm exist only on UDP/RTP and ST 2110.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixProfile {
    /// QUIC and AVB have no plain/gcm switch.
    Unset,
    Plain,
    Gcm,
}

/// One supported protocol, layer, and profile. Codecs are a separate axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatrixCell {
    pub carrier: u8,
    pub layer: MatrixLayer,
    pub profile: MatrixProfile,
}

/// One cell plus the picture codec the player asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatrixSession {
    pub cell: MatrixCell,
    pub codec: &'static str,
}

/// Picture codecs the matrix runs. Family names, not encoder rows.
pub const MATRIX_CODECS: [&str; 3] = ["mjpeg", "pyrowave", "h264"];

/// UDP/RTP is an IP carrier. L2 has no UDP socket to dial.
pub fn matrix_layer_enabled(layer: MatrixLayer, carrier: u8) -> bool {
    !(layer == MatrixLayer::L2 && carrier == UDP_RTP)
}

fn profiles_for(carrier: u8) -> &'static [MatrixProfile] {
    match carrier {
        UDP_RTP | ST2110 => &[MatrixProfile::Plain, MatrixProfile::Gcm],
        _ => &[MatrixProfile::Unset],
    }
}

/// Every supported protocol × layer × profile, in carrier order.
pub fn matrix_cells() -> Vec<MatrixCell> {
    let carriers = [QUIC_STREAMS, QUIC_DATAGRAMS, UDP_RTP, AVB, ST2110];
    let layers = [MatrixLayer::L2, MatrixLayer::Ipv4, MatrixLayer::Ipv6];
    let mut cells = Vec::new();
    for carrier in carriers {
        for layer in layers {
            if !matrix_layer_enabled(layer, carrier) {
                continue;
            }
            for profile in profiles_for(carrier) {
                cells.push(MatrixCell {
                    carrier,
                    layer,
                    profile: *profile,
                });
            }
        }
    }
    cells
}

/// [`matrix_cells`] crossed with [`MATRIX_CODECS`]. This is the list a live
/// matrix run executes.
pub fn matrix_sessions() -> Vec<MatrixSession> {
    let mut sessions = Vec::new();
    for cell in matrix_cells() {
        for codec in MATRIX_CODECS {
            sessions.push(MatrixSession { cell, codec });
        }
    }
    sessions
}

/// `quic-streams/l2/mjpeg`, `udp-rtp/ipv4/plain/h264`, `st2110/ipv6/gcm/pyrowave`.
pub fn matrix_label(session: &MatrixSession) -> String {
    let carrier = match session.cell.carrier {
        QUIC_STREAMS => "quic-streams",
        QUIC_DATAGRAMS => "quic-datagrams",
        UDP_RTP => "udp-rtp",
        AVB => "avb",
        ST2110 => "st2110",
        _ => "carrier",
    };
    let layer = match session.cell.layer {
        MatrixLayer::L2 => "l2",
        MatrixLayer::Ipv4 => "ipv4",
        MatrixLayer::Ipv6 => "ipv6",
    };
    match session.cell.profile {
        MatrixProfile::Unset => format!("{carrier}/{layer}/{}", session.codec),
        MatrixProfile::Plain => format!("{carrier}/{layer}/plain/{}", session.codec),
        MatrixProfile::Gcm => format!("{carrier}/{layer}/gcm/{}", session.codec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_follow_the_connect_rules_and_three_codecs() {
        let cells = matrix_cells();
        assert_eq!(cells.len(), 19, "{cells:?}");
        let sessions = matrix_sessions();
        assert_eq!(sessions.len(), 57);
        let labels: Vec<String> = sessions.iter().map(matrix_label).collect();
        assert_eq!(labels.len(), labels.iter().collect::<std::collections::BTreeSet<_>>().len());

        for carrier in [QUIC_STREAMS, QUIC_DATAGRAMS, AVB] {
            let rows: Vec<_> = cells.iter().filter(|cell| cell.carrier == carrier).collect();
            assert_eq!(rows.len(), 3, "carrier {carrier}");
            assert!(rows.iter().all(|cell| cell.profile == MatrixProfile::Unset));
            assert!(rows.iter().any(|cell| cell.layer == MatrixLayer::L2));
            assert!(rows.iter().any(|cell| cell.layer == MatrixLayer::Ipv4));
            assert!(rows.iter().any(|cell| cell.layer == MatrixLayer::Ipv6));
        }
        for carrier in [UDP_RTP, ST2110] {
            for layer in [MatrixLayer::Ipv4, MatrixLayer::Ipv6] {
                let rows: Vec<_> = cells
                    .iter()
                    .filter(|cell| cell.carrier == carrier && cell.layer == layer)
                    .collect();
                assert!(rows.iter().any(|cell| cell.profile == MatrixProfile::Plain));
                assert!(rows.iter().any(|cell| cell.profile == MatrixProfile::Gcm));
            }
        }
        assert!(cells.iter().any(|cell| {
            cell.carrier == ST2110 && cell.layer == MatrixLayer::L2 && cell.profile == MatrixProfile::Plain
        }));
        assert!(cells.iter().any(|cell| {
            cell.carrier == ST2110 && cell.layer == MatrixLayer::L2 && cell.profile == MatrixProfile::Gcm
        }));
        assert!(!cells.iter().any(|cell| cell.carrier == UDP_RTP && cell.layer == MatrixLayer::L2));
        assert!(labels.iter().all(|label| {
            label.ends_with("/mjpeg") || label.ends_with("/pyrowave") || label.ends_with("/h264")
        }));
        assert!(labels.iter().any(|label| label == "quic-streams/l2/mjpeg"));
        assert!(labels.iter().any(|label| label == "quic-datagrams/ipv6/h264"));
        assert!(labels.iter().any(|label| label == "udp-rtp/ipv4/plain/pyrowave"));
        assert!(labels.iter().any(|label| label == "udp-rtp/ipv6/gcm/h264"));
        assert!(labels.iter().any(|label| label == "avb/l2/mjpeg"));
        assert!(labels.iter().any(|label| label == "st2110/l2/plain/mjpeg"));
        assert!(labels.iter().any(|label| label == "st2110/ipv6/gcm/pyrowave"));
        assert!(labels.iter().all(|label| !label.starts_with("udp-rtp/l2")));
        assert!(labels.iter().all(|label| {
            let quic_or_avb = label.starts_with("quic-") || label.starts_with("avb/");
            !quic_or_avb || (!label.contains("/plain/") && !label.contains("/gcm/"))
        }));
    }
}
