//! How close a picture bitrate sits to what this path can actually carry.
//!
//! Advertised link speed is not usable goodput. PHY and MAC top out around
//! 90% of the rated rate (1 Gbit/s → 900 Mbit/s). Errors climb before that
//! wall, so the comfortable ceiling is another 90% (~810 Mbit/s on a gigabit
//! NIC). QUIC headers, delayed ACKs, RTT, and jitter take another slice.

/// PHY/MAC cannot deliver the printed rate.
pub const PHY_EFFICIENCY: f64 = 0.90;
/// Errors climb before the PHY cap; stay under this fraction of PHY goodput.
pub const APPROACH_HEADROOM: f64 = 0.90;
/// Ethernet + QUIC headers on a 1500-byte frame.
const HEADER_OVERHEAD: f64 = 58.0 / 1500.0;
/// Delayed ACK (~80 bytes every two ~1442-byte payloads) at zero RTT.
const ACK_OVERHEAD: f64 = 80.0 / 1442.0 / 2.0;
/// Extra ACK/retransmit cost as RTT grows, up to this fraction.
const RTT_OVERHEAD_MAX: f64 = 0.10;
const RTT_SCALE_MS: f64 = 8.0;
/// Extra waste when jitter is a large fraction of RTT.
const JITTER_OVERHEAD_MAX: f64 = 0.15;
/// Utilization of [`Path::usable_mbps`] where the yellow band starts.
pub const YELLOW_AT: f64 = 0.85;
/// Utilization where the red band starts (past the comfortable ceiling).
pub const RED_AT: f64 = 1.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub nic: String,
    pub bandwidth_mbps: u32,
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub computer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarnLevel {
    Ok,
    Yellow,
    Red,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Load {
    pub level: WarnLevel,
    pub color: u32,
    pub error_rate: f64,
    pub tip: Option<String>,
}

impl Path {
    /// Comfortable goodput in Mbit/s after PHY, approach headroom, and protocol.
    pub fn usable_mbps(&self) -> f64 {
        let phy = f64::from(self.bandwidth_mbps) * PHY_EFFICIENCY;
        let approach = phy * APPROACH_HEADROOM;
        let latency = f64::from(self.latency_ms.max(0.0));
        let jitter = f64::from(self.jitter_ms.max(0.0));
        let rtt_factor = latency / (latency + RTT_SCALE_MS);
        let rtt_overhead = RTT_OVERHEAD_MAX * rtt_factor;
        let jitter_frac = jitter / latency.max(0.5);
        let jitter_overhead = JITTER_OVERHEAD_MAX * (jitter_frac / (1.0 + jitter_frac));
        let protocol = (HEADER_OVERHEAD + ACK_OVERHEAD + rtt_overhead + jitter_overhead)
            .clamp(0.0, 0.60);
        approach * (1.0 - protocol)
    }

    pub fn jitter_pct(&self) -> f64 {
        let latency = f64::from(self.latency_ms.max(0.0));
        if latency < 0.05 {
            0.0
        } else {
            (f64::from(self.jitter_ms.max(0.0)) / latency * 100.0).clamp(0.0, 100.0)
        }
    }

    /// Estimated transmission error rate for `kbps` on this path, 0..=1.
    pub fn error_rate(&self, kbps: u32) -> f64 {
        let usable = self.usable_mbps().max(0.001);
        let u = (f64::from(kbps) / 1000.0) / usable;
        (1.0 - (-0.15 * u.powf(4.5)).exp()).clamp(0.0, 0.99)
    }

    pub fn load(&self, kbps: u32) -> Load {
        let usable = self.usable_mbps().max(0.001);
        let u = (f64::from(kbps) / 1000.0) / usable;
        let error_rate = self.error_rate(kbps);
        let level = if u >= RED_AT {
            WarnLevel::Red
        } else if u >= YELLOW_AT {
            WarnLevel::Yellow
        } else {
            WarnLevel::Ok
        };
        Load {
            level,
            color: spectrum(u),
            error_rate,
            tip: match level {
                WarnLevel::Ok => None,
                WarnLevel::Yellow => Some(self.tip(false, error_rate)),
                WarnLevel::Red => Some(self.tip(true, error_rate)),
            },
        }
    }

    fn tip(&self, red: bool, error_rate: f64) -> String {
        let head = if red {
            "Experience may be seriously degraded."
        } else {
            "Experience may be degraded."
        };
        let rate = if red {
            format!(
                "the estimated transmission error rate is expected to be extremely high at {}",
                format_error(error_rate)
            )
        } else {
            format!(
                "the estimated transmission error rate reaches moderate levels of {}",
                format_error(error_rate)
            )
        };
        format!(
            "{head}\nBased on the maximum bandwidth {} and the {} ms {}% connection to {}, {rate}",
            format_link_mbps(self.bandwidth_mbps),
            format_ms(self.latency_ms),
            format_pct(self.jitter_pct()),
            self.computer,
        )
    }
}

const TEXT: u32 = 0xe7ecf3;
const YELLOW: u32 = 0xe6c14a;
const ORANGE: u32 = 0xff9f43;
const RED: u32 = 0xff6b6b;

fn spectrum(u: f64) -> u32 {
    if u <= 0.0 {
        return TEXT;
    }
    if u < YELLOW_AT {
        return lerp_rgb(TEXT, YELLOW, u / YELLOW_AT);
    }
    if u < RED_AT {
        let t = (u - YELLOW_AT) / (RED_AT - YELLOW_AT);
        return lerp_rgb(YELLOW, ORANGE, t);
    }
    let t = ((u - RED_AT) / 0.25).clamp(0.0, 1.0);
    lerp_rgb(ORANGE, RED, t)
}

fn lerp_rgb(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |shift: u32| {
        let x = ((a >> shift) & 0xff) as f64;
        let y = ((b >> shift) & 0xff) as f64;
        (x + (y - x) * t).round() as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

pub fn format_link_mbps(mbps: u32) -> String {
    if mbps >= 1000 && mbps % 1000 == 0 {
        format!("{} Gbit/s", mbps / 1000)
    } else if mbps >= 1000 {
        format!("{:.1} Gbit/s", f64::from(mbps) / 1000.0)
    } else {
        format!("{mbps} Mbit/s")
    }
}

fn format_ms(ms: f32) -> String {
    if ms >= 10.0 {
        format!("{:.0}", ms)
    } else {
        format!("{:.1}", ms)
    }
}

fn format_pct(pct: f64) -> String {
    if pct >= 10.0 {
        format!("{:.0}", pct)
    } else {
        format!("{:.1}", pct)
    }
}

pub fn format_error(rate: f64) -> String {
    let pct = (rate * 100.0).clamp(0.0, 99.0);
    if pct >= 10.0 {
        format!("{pct:.0}%")
    } else {
        format!("{pct:.1}%")
    }
}

/// `/sys/class/net/*/speed` is Mbps; `-1` means unknown.
pub fn parse_sysfs_speed(text: &str) -> Option<u32> {
    let n: i64 = text.trim().parse().ok()?;
    (n > 0 && n <= 1_000_000).then_some(n as u32)
}

/// BSD `ifconfig` media / link-rate lines.
pub fn parse_ifconfig_speed(text: &str) -> Option<u32> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.split("link rate:").nth(1) {
            let mut words = rest.split_whitespace();
            let value: f64 = words.next()?.parse().ok()?;
            let unit = words.next().unwrap_or("mbps");
            if unit.starts_with("gb") {
                let mbps = (value * 1000.0).round() as i64;
                if mbps > 0 {
                    return Some(mbps as u32);
                }
            } else {
                let mbps = value.round() as i64;
                if mbps > 0 {
                    return Some(mbps as u32);
                }
            }
        }
        if let Some(mbps) = media_speed(&lower) {
            return Some(mbps);
        }
    }
    None
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn media_speed(line: &str) -> Option<u32> {
    let start = line.find("media:")?;
    let body = &line[start..];
    if let Some(at) = body.find("gbase") {
        let prefix = body[..at].rsplit(|c: char| !c.is_ascii_digit() && c != '.').next()?;
        let g: f64 = prefix.parse().ok()?;
        let mbps = (g * 1000.0).round() as i64;
        return (mbps > 0).then_some(mbps as u32);
    }
    let at = body.find("base")?;
    let prefix = body[..at]
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?;
    let mbps: u32 = prefix.parse().ok()?;
    (mbps > 0).then_some(mbps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gigabit() -> Path {
        Path {
            nic: "eth0".into(),
            bandwidth_mbps: 1000,
            latency_ms: 1.0,
            jitter_ms: 0.1,
            computer: "AN515".into(),
        }
    }

    #[test]
    fn gigabit_has_headroom_below_the_printed_rate() {
        let usable = gigabit().usable_mbps();
        assert!(usable < 900.0, "usable {usable}");
        assert!(usable > 650.0, "usable {usable}");
        assert_eq!(format_link_mbps(1000), "1 Gbit/s");
        assert_eq!(format_link_mbps(100), "100 Mbit/s");
        assert_eq!(format_link_mbps(2500), "2.5 Gbit/s");
    }

    #[test]
    fn latency_and_jitter_cut_usable_goodput() {
        let mut path = gigabit();
        let low = path.usable_mbps();
        path.latency_ms = 40.0;
        path.jitter_ms = 12.0;
        let high_rtt = path.usable_mbps();
        assert!(high_rtt < low, "low-rtt {low} high-rtt {high_rtt}");
        assert!((path.jitter_pct() - 30.0).abs() < 0.1, "{}", path.jitter_pct());
    }

    #[test]
    fn yellow_then_red_as_bitrate_approaches_the_ceiling() {
        let path = gigabit();
        let usable_kbps = (path.usable_mbps() * 1000.0) as u32;
        let green = path.load(50_000);
        assert_eq!(green.level, WarnLevel::Ok);
        assert!(green.tip.is_none());

        let yellow = path.load((usable_kbps as f64 * 0.90) as u32);
        assert_eq!(yellow.level, WarnLevel::Yellow, "err {}", yellow.error_rate);
        let tip = yellow.tip.unwrap();
        assert!(tip.starts_with("Experience may be degraded."), "{tip}");
        assert!(tip.contains("1 Gbit/s"), "{tip}");
        assert!(tip.contains("AN515"), "{tip}");
        assert!(tip.contains("moderate levels of"), "{tip}");
        assert!(tip.contains('\n'), "{tip}");

        let red = path.load(usable_kbps.saturating_add(80_000));
        assert_eq!(red.level, WarnLevel::Red, "err {}", red.error_rate);
        let tip = red.tip.unwrap();
        assert!(tip.starts_with("Experience may be seriously degraded."), "{tip}");
        assert!(tip.contains("extremely high at"), "{tip}");
        assert!(red.error_rate > yellow.error_rate);
    }

    #[test]
    fn a_hundred_megabit_link_goes_red_sooner() {
        let path = Path {
            nic: "wlan0".into(),
            bandwidth_mbps: 100,
            latency_ms: 8.0,
            jitter_ms: 2.0,
            computer: "claire".into(),
        };
        let load = path.load(90_000);
        assert_eq!(load.level, WarnLevel::Red);
    }

    #[test]
    fn sysfs_and_ifconfig_speeds_parse() {
        assert_eq!(parse_sysfs_speed("1000\n"), Some(1000));
        assert_eq!(parse_sysfs_speed("-1"), None);
        assert_eq!(parse_sysfs_speed("0"), None);
        assert_eq!(
            parse_ifconfig_speed("media: autoselect (1000baseT <full-duplex>)\n"),
            Some(1000)
        );
        assert_eq!(
            parse_ifconfig_speed("media: autoselect (10Gbase-T <full-duplex>)\n"),
            Some(10_000)
        );
        assert_eq!(
            parse_ifconfig_speed("link rate: 866.67 Mbps\n"),
            Some(867)
        );
        assert_eq!(
            parse_ifconfig_speed("link rate: 1.00 Gbps\n"),
            Some(1000)
        );
    }

    #[test]
    fn spectrum_moves_from_text_through_yellow_to_red() {
        let path = gigabit();
        let g = path.load(10_000).color;
        let y = path.load((path.usable_mbps() * 900.0) as u32).color;
        let r = path.load(950_000).color;
        assert_ne!(g, y);
        assert_ne!(y, r);
    }
}
