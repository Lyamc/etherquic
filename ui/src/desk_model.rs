//! Desk behaviour the window paints: listen set, session list, live edits,
//! tab groups, and the host-service summary.
//!
//! The picture window and the settings page used to share one global view and
//! nothing else. This module is the shared decision for what a click, a
//! settings change, or a closed session does.

use std::collections::BTreeMap;
use std::sync::Mutex;

use etherquic::IfaceKind;

/// One adapter row. `listen` is the user's toggle. Default is on for real and
/// virtual links, off for loopback and capture devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfaceRow {
    pub name: String,
    pub friendly: String,
    pub mac: String,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub kind: IfaceKind,
    pub listen: bool,
}

impl IfaceRow {
    pub fn label(&self) -> String {
        let friendly = if self.friendly.trim().is_empty() || self.friendly == self.name {
            self.name.clone()
        } else {
            format!("{} ({})", self.friendly.trim(), self.name)
        };
        let mac = if self.mac.is_empty() {
            "no mac".to_string()
        } else {
            self.mac.clone()
        };
        let mut addrs = self.ipv4.clone();
        addrs.extend(self.ipv6.iter().take(1).cloned());
        let addrs = if addrs.is_empty() {
            "no address".to_string()
        } else {
            addrs.join(" ")
        };
        format!("{friendly} · {mac} · {addrs} · {}", self.kind.label())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenSet {
    rows: Vec<IfaceRow>,
}

impl ListenSet {
    pub fn from_parts(
        rows: impl IntoIterator<Item = (String, String, String, Vec<String>, Vec<String>, IfaceKind)>,
    ) -> Self {
        let rows = rows
            .into_iter()
            .map(|(name, friendly, mac, ipv4, ipv6, kind)| IfaceRow {
                listen: kind == IfaceKind::Physical || kind == IfaceKind::Virtual,
                name,
                friendly,
                mac,
                ipv4,
                ipv6,
                kind,
            })
            .collect();
        Self { rows }
    }

    pub fn rows(&self) -> &[IfaceRow] {
        &self.rows
    }

    pub fn is_all(&self) -> bool {
        self.rows
            .iter()
            .filter(|row| row.kind != IfaceKind::Capture)
            .all(|row| row.listen == default_listen(row.kind))
    }

    /// Names discovery and the host should open. Empty when nothing is selected.
    pub fn probe_names(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| row.listen)
            .map(|row| row.name.clone())
            .collect()
    }

    /// `Some` only when exactly one adapter is on. All and any wider set are
    /// Auto, which is `None`.
    pub fn dial_nic(&self) -> Option<String> {
        let on: Vec<&IfaceRow> = self.rows.iter().filter(|row| row.listen).collect();
        if on.len() == 1 {
            Some(on[0].name.clone())
        } else {
            None
        }
    }

    /// Turn one adapter on or off. The last listening adapter stays on.
    pub fn toggle(&mut self, name: &str) -> bool {
        let listening = self.rows.iter().filter(|row| row.listen).count();
        let Some(row) = self.rows.iter_mut().find(|row| row.name == name) else {
            return false;
        };
        if row.listen && listening <= 1 {
            return false;
        }
        row.listen = !row.listen;
        true
    }

    pub fn set_all(&mut self) {
        for row in &mut self.rows {
            row.listen = default_listen(row.kind);
        }
    }

    /// Listen on one adapter and turn the others off. Capture devices stay off.
    pub fn select_only(&mut self, name: &str) -> bool {
        let ok = self
            .rows
            .iter()
            .any(|row| row.name == name && row.kind != IfaceKind::Capture);
        if !ok {
            return false;
        }
        for row in &mut self.rows {
            row.listen = row.name == name;
        }
        true
    }
}

fn default_listen(kind: IfaceKind) -> bool {
    matches!(kind, IfaceKind::Physical | IfaceKind::Virtual)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Connected,
    Paused,
    Error(String),
}

pub fn phase_from_status(status: &str) -> Phase {
    let lower = status.to_ascii_lowercase();
    if lower.contains("fail")
        || lower.contains("error")
        || lower.contains("timed")
        || lower.contains("denied")
        || lower.contains("refused")
        || lower.contains("closed")
        || lower.contains("dropped")
    {
        let reason = status.trim();
        let reason = if reason.len() > 80 {
            format!("{}…", &reason[..80])
        } else {
            reason.to_string()
        };
        return Phase::Error(reason);
    }
    if lower.contains("paus") || lower.contains("idle") || lower.contains("stall") {
        return Phase::Paused;
    }
    Phase::Connected
}

pub fn guest_line(name: &str) -> String {
    format!("{name} · [connected]")
}

pub fn outbound_line(label: &str, phase: &Phase, group: u64) -> String {
    format!("{label}  {}  · window {group}", phase_badge(phase))
}

/// Interface and layer changes redial the peer this session already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redial {
    NotClient,
    Address(std::net::SocketAddr),
    Peer([u8; 6]),
}

pub fn plan_redial(is_client: bool, peer: [u8; 6], udp: Option<std::net::SocketAddr>) -> Redial {
    if !is_client {
        return Redial::NotClient;
    }
    if peer == [0; 6] {
        if let Some(addr) = udp {
            return Redial::Address(addr);
        }
    }
    Redial::Peer(peer)
}

pub fn phase_badge(phase: &Phase) -> String {
    match phase {
        Phase::Connected => "[connected]".into(),
        Phase::Paused => "[paused/idle]".into(),
        Phase::Error(reason) => format!("[error: {reason}]"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Select,
    Focus,
}

pub fn click_action(click_count: usize) -> Click {
    if click_count >= 2 {
        Click::Focus
    } else {
        Click::Select
    }
}

/// What a double-click asks the picture window to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusRequest {
    pub unminimize: bool,
    pub other_desktop: bool,
}

impl FocusRequest {
    pub fn raise() -> Self {
        Self {
            unminimize: true,
            other_desktop: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outbound {
    pub id: u64,
    pub label: String,
    pub phase: Phase,
    pub group: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLayout {
    next_group: u64,
    /// session id -> window group
    seat: BTreeMap<u64, u64>,
}

impl Default for TabLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl TabLayout {
    pub fn new() -> Self {
        Self {
            next_group: 1,
            seat: BTreeMap::new(),
        }
    }

    pub fn group_of(&mut self, id: u64) -> u64 {
        if let Some(group) = self.seat.get(&id).copied() {
            return group;
        }
        let group = 1;
        self.next_group = self.next_group.max(2);
        self.seat.insert(id, group);
        group
    }

    pub fn detach(&mut self, id: u64) -> u64 {
        let group = self.next_group;
        self.next_group += 1;
        self.seat.insert(id, group);
        group
    }

    pub fn move_to(&mut self, id: u64, group: u64) {
        self.next_group = self.next_group.max(group.saturating_add(1));
        self.seat.insert(id, group);
    }

    pub fn forget(&mut self, id: u64) {
        self.seat.remove(&id);
    }

    pub fn groups(&self) -> Vec<(u64, Vec<u64>)> {
        let mut map: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        for (id, group) in &self.seat {
            map.entry(*group).or_default().push(*id);
        }
        map.into_iter().collect()
    }
}

/// Replace the outbound list with the sessions that are still open.
pub fn sync_outbound(open: &[(u64, String, String)], layout: &mut TabLayout) -> Vec<Outbound> {
    let live: Vec<u64> = open.iter().map(|(id, _, _)| *id).collect();
    let stale: Vec<u64> = layout
        .seat
        .keys()
        .copied()
        .filter(|id| !live.contains(id))
        .collect();
    for id in stale {
        layout.forget(id);
    }
    open.iter()
        .map(|(id, label, status)| Outbound {
            id: *id,
            label: label.clone(),
            phase: phase_from_status(status),
            group: layout.group_of(*id),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveKind {
    /// The open picture reads this every frame or on the next offer.
    Immediate,
    /// A new offer on the same connection: codec, rate, carrier.
    Renegotiate,
    /// The socket itself changes.
    Redial,
}

pub fn live_kind(field: &str) -> LiveKind {
    match field {
        "interface" | "nic" | "layer" | "l2" | "l3" => LiveKind::Redial,
        "video" | "audio" | "codec" | "fps" | "carrier" => LiveKind::Renegotiate,
        _ => LiveKind::Immediate,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceLog {
    pub on: bool,
    lines: Vec<String>,
}

impl TraceLog {
    pub fn toggle(&mut self) {
        self.on = !self.on;
    }

    pub fn observe(&mut self, line: &str) {
        if !self.on {
            return;
        }
        let lower = line.to_ascii_lowercase();
        let interesting = lower.contains("ms")
            || lower.contains("rtt")
            || lower.contains("loss")
            || lower.contains("latenc")
            || lower.contains("error")
            || lower.contains("drop")
            || lower.contains("retransmit")
            || lower.contains("timeout")
            || lower.contains("trace");
        if !interesting {
            return;
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if self.lines.last().map(String::as_str) == Some(line) {
            return;
        }
        self.lines.push(line.to_string());
        if self.lines.len() > 40 {
            let drop_n = self.lines.len() - 40;
            self.lines.drain(0..drop_n);
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

pub fn quick_hotkey(control: bool, alt: bool, shift: bool, key: &str) -> bool {
    control && alt && !shift && key.eq_ignore_ascii_case("s")
}

/// Lines a picture child applies on stdin. The parent keeps the same view.
pub fn child_commands(pairs: &[(&str, &str)]) -> Vec<String> {
    pairs
        .iter()
        .map(|(key, value)| format!("set {key}={value}"))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFace {
    pub process_up: bool,
    pub process_peers: usize,
    pub service_installed: bool,
    pub service_running: bool,
    pub service_hosting: bool,
    pub guests: Vec<String>,
}

/// Installed is the plist, a host.conf on disk, or a service that is already up.
pub fn service_is_installed(plist: bool, conf: bool, running: bool) -> bool {
    plist || conf || running
}

impl HostFace {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.service_installed && !self.service_running && !self.process_up {
            parts.push("Service installed, not running".to_string());
        } else if self.service_running {
            if self.service_hosting {
                parts.push("Service running · hosting".to_string());
            } else {
                parts.push("Service running · not hosting".to_string());
            }
        } else if !self.process_up {
            parts.push("Not hosting".to_string());
        }
        if self.process_up {
            let players = if self.process_peers == 1 {
                "1 player".to_string()
            } else {
                format!("{} players", self.process_peers)
            };
            parts.push(format!("This window is hosting · {players}"));
        }
        if self.guests.is_empty() {
            if self.service_hosting {
                parts.push("No players on the service".to_string());
            }
        } else {
            parts.push(format!(
                "{} connected to the service",
                self.guests.len()
            ));
        }
        parts.join(" · ")
    }
}

#[derive(Clone, Debug)]
pub struct TabSnap {
    pub id: u64,
    pub label: String,
    pub badge: String,
    pub current: bool,
    pub group: u64,
}

#[derive(Clone, Debug, Default)]
struct Share {
    tabs: Vec<TabSnap>,
    quick: bool,
    commands: Vec<ShareCmd>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareCmd {
    Select(u64),
    Focus(u64),
    Close(u64),
    ToggleQuick,
}

fn share() -> &'static Mutex<Share> {
    static SHARE: Mutex<Share> = Mutex::new(Share {
        tabs: Vec::new(),
        quick: false,
        commands: Vec::new(),
    });
    &SHARE
}

fn lock() -> std::sync::MutexGuard<'static, Share> {
    share().lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn publish_tabs(tabs: Vec<TabSnap>, quick: bool) {
    let mut slot = lock();
    slot.tabs = tabs;
    slot.quick = quick;
}

pub fn published_tabs() -> (Vec<TabSnap>, bool) {
    let slot = lock();
    (slot.tabs.clone(), slot.quick)
}

pub fn push_cmd(cmd: ShareCmd) {
    lock().commands.push(cmd);
}

pub fn take_cmds() -> Vec<ShareCmd> {
    std::mem::take(&mut lock().commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_defaults_to_every_real_and_virtual_link() {
        let set = ListenSet::from_parts([
            (
                "enp5s0".into(),
                "Ethernet".into(),
                "aa:bb:cc:dd:ee:01".into(),
                vec!["192.168.1.20".into()],
                vec!["fe80::1".into()],
                IfaceKind::Physical,
            ),
            (
                "docker0".into(),
                "docker0".into(),
                "02:42:00:00:00:01".into(),
                vec!["172.17.0.1".into()],
                vec![],
                IfaceKind::Virtual,
            ),
            (
                "lo".into(),
                "lo".into(),
                "00:00:00:00:00:00".into(),
                vec![],
                vec![],
                IfaceKind::Loopback,
            ),
        ]);
        assert!(set.is_all());
        assert!(!set.rows()[2].listen);
        let mut set = set;
        assert!(set.toggle("lo"));
        assert!(!set.is_all());
        assert_eq!(set.dial_nic(), None);
        assert_eq!(
            set.probe_names(),
            vec![
                "enp5s0".to_string(),
                "docker0".to_string(),
                "lo".to_string()
            ]
        );
        set.set_all();
        assert!(!set.rows()[2].listen);
        assert!(set.is_all());
        assert_eq!(set.dial_nic(), None);
        assert_eq!(set.probe_names(), vec!["enp5s0".to_string(), "docker0".to_string()]);
        assert!(set.rows()[0].label().contains("Ethernet (enp5s0)"));
        assert!(set.rows()[0].label().contains("192.168.1.20"));
        assert!(set.rows()[0].label().contains("physical"));
        assert!(set.rows()[1].label().contains("virtual"));
    }

    #[test]
    fn turning_one_link_off_narrows_discovery_and_keeps_the_last() {
        let mut set = ListenSet::from_parts([
            (
                "enp5s0".into(),
                "Ethernet".into(),
                String::new(),
                vec![],
                vec![],
                IfaceKind::Physical,
            ),
            (
                "wlan0".into(),
                "Wi-Fi".into(),
                String::new(),
                vec![],
                vec![],
                IfaceKind::Physical,
            ),
        ]);
        assert!(set.toggle("wlan0"));
        assert!(!set.is_all());
        assert_eq!(set.probe_names(), vec!["enp5s0".to_string()]);
        assert_eq!(set.dial_nic().as_deref(), Some("enp5s0"));
        assert!(!set.toggle("enp5s0"));
        assert_eq!(set.probe_names(), vec!["enp5s0".to_string()]);
        set.set_all();
        assert!(set.is_all());
    }

    #[test]
    fn a_closed_session_leaves_the_active_list() {
        let mut layout = TabLayout::new();
        let open = sync_outbound(
            &[
                (1, "desk".into(), "Viewing desk".into()),
                (2, "laptop".into(), "Connect failed: timed out".into()),
            ],
            &mut layout,
        );
        assert_eq!(phase_badge(&open[0].phase), "[connected]");
        assert!(phase_badge(&open[1].phase).starts_with("[error:"));
        assert_eq!(open[0].group, open[1].group);
        let group = layout.detach(2);
        assert_ne!(group, layout.group_of(1));
        let left = sync_outbound(&[(1, "desk".into(), "idle".into())], &mut layout);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, 1);
        assert_eq!(phase_badge(&left[0].phase), "[paused/idle]");
        assert!(layout.groups().iter().all(|(_, ids)| !ids.contains(&2)));
    }

    #[test]
    fn double_click_focuses_and_a_single_click_selects() {
        assert_eq!(click_action(1), Click::Select);
        assert_eq!(click_action(2), Click::Focus);
        let focus = FocusRequest::raise();
        assert!(focus.unminimize && focus.other_desktop);
        assert_eq!(live_kind("scale"), LiveKind::Immediate);
        assert_eq!(live_kind("codec"), LiveKind::Renegotiate);
        assert_eq!(live_kind("interface"), LiveKind::Redial);
    }

    #[test]
    fn trace_keeps_latency_lines_and_the_hotkey_is_ctrl_alt_s() {
        let mut trace = TraceLog::default();
        trace.observe("rtt 400 us");
        assert!(trace.lines().is_empty());
        trace.toggle();
        trace.observe("rtt 400 us");
        trace.observe("rtt 400 us");
        trace.observe("picture");
        assert_eq!(trace.lines(), &["rtt 400 us".to_string()]);
        assert!(quick_hotkey(true, true, false, "s"));
        assert!(!quick_hotkey(true, false, false, "s"));
    }

    #[test]
    fn host_summary_names_the_service_and_this_window() {
        let face = HostFace {
            process_up: true,
            process_peers: 1,
            service_installed: true,
            service_running: true,
            service_hosting: true,
            guests: vec!["10.1.1.8:4433".into()],
        };
        let text = face.summary();
        assert!(text.contains("Service running · hosting"));
        assert!(text.contains("This window is hosting · 1 player"));
        assert!(text.contains("1 connected to the service"));
        let idle = HostFace {
            process_up: false,
            process_peers: 0,
            service_installed: true,
            service_running: false,
            service_hosting: false,
            guests: vec![],
        };
        assert_eq!(idle.summary(), "Service installed, not running");
        assert!(service_is_installed(false, true, false));
        assert!(service_is_installed(true, false, false));
        assert!(service_is_installed(false, false, true));
        assert!(!service_is_installed(false, false, false));
    }

    #[test]
    fn one_adapter_is_dialed_and_the_row_shows_its_addresses() {
        let mut set = ListenSet::from_parts([
            (
                "enp5s0".into(),
                "Ethernet".into(),
                "aa:bb:cc:dd:ee:01".into(),
                vec!["192.168.1.20".into()],
                vec!["fe80::1".into()],
                IfaceKind::Physical,
            ),
            (
                "wlan0".into(),
                "Wi-Fi".into(),
                "aa:bb:cc:dd:ee:02".into(),
                vec!["10.0.0.5".into()],
                vec!["fe80::2".into()],
                IfaceKind::Physical,
            ),
            (
                "lo".into(),
                "lo".into(),
                String::new(),
                vec![],
                vec![],
                IfaceKind::Loopback,
            ),
            (
                "any".into(),
                "any".into(),
                String::new(),
                vec![],
                vec![],
                IfaceKind::Capture,
            ),
        ]);
        assert!(set.rows()[0].label().contains("fe80::1"));
        assert!(set.rows()[0].label().contains("physical"));
        assert!(set.rows()[3].label().contains("capture"));
        assert!(set.select_only("wlan0"));
        assert_eq!(set.dial_nic().as_deref(), Some("wlan0"));
        assert!(!set.select_only("any"));
        set.set_all();
        assert!(set.is_all());
        assert_eq!(set.dial_nic(), None);
        assert!(!set.probe_names().iter().any(|name| name == "lo" || name == "any"));
    }

    #[test]
    fn one_window_holds_three_sessions_and_another_holds_two() {
        let mut layout = TabLayout::new();
        for id in 1..=3 {
            layout.move_to(id, 1);
        }
        for id in 4..=5 {
            layout.move_to(id, 2);
        }
        let groups = layout.groups();
        let three = groups.iter().find(|(group, _)| *group == 1).unwrap();
        let two = groups.iter().find(|(group, _)| *group == 2).unwrap();
        assert_eq!(three.1.len(), 3);
        assert_eq!(two.1.len(), 2);
        let open = sync_outbound(
            &[
                (1, "a".into(), "up".into()),
                (4, "b".into(), "idle".into()),
            ],
            &mut layout,
        );
        assert!(outbound_line(&open[0].label, &open[0].phase, open[0].group).contains("window 1"));
        assert!(outbound_line(&open[1].label, &open[1].phase, open[1].group).contains("[paused/idle]"));
        assert_eq!(guest_line("10.1.1.8"), "10.1.1.8 · [connected]");
    }

    #[test]
    fn redial_uses_the_same_peer_and_a_host_tab_does_not() {
        let peer = [2, 0, 0, 0, 0, 9];
        assert_eq!(plan_redial(false, peer, None), Redial::NotClient);
        assert_eq!(plan_redial(true, peer, None), Redial::Peer(peer));
        let addr = "192.168.1.9:4433".parse().unwrap();
        assert_eq!(plan_redial(true, [0; 6], Some(addr)), Redial::Address(addr));
    }

    #[test]
    fn shelf_commands_round_trip() {
        let _ = take_cmds();
        push_cmd(ShareCmd::Focus(4));
        assert_eq!(take_cmds(), vec![ShareCmd::Focus(4)]);
        assert!(take_cmds().is_empty());
    }
}
