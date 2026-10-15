//! `host` and `connect` share the same link flags as the other L2 tools.

use std::env;
use std::path::PathBuf;

use etherquic::parse_mac;
use etherquic_play::{
    PadSlot, ScaleMode, SizeMatch, ViewOptions, CLIENT_MAC, DEFAULT_FPS, DEFAULT_MAX_WIDTH,
    HOST_MAC, PAD_SLOTS, SERVICE_ID,
};

pub struct Host {
    pub nic: Option<String>,
    pub local_mac: [u8; 6],
    pub service_id: u16,
    pub max_width: u32,
    pub fps: u32,
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub struct Connect {
    pub nic: Option<String>,
    pub local_mac: [u8; 6],
    pub peer_mac: [u8; 6],
    pub service_id: u16,
    /// Dial the host over UDP instead of raw Ethernet.
    pub udp: Option<std::net::SocketAddr>,
    /// Ask the host to confirm stamped input.
    pub trace_input: bool,
    /// Scale, what to send, pad slots, and the speaker.
    pub view: ViewOptions,
    /// Stop when stdin closes. A parent process (the macOS app) uses this to end
    /// the session cleanly.
    pub parent_pipe: bool,
}

pub enum Command {
    Host(Host),
    Connect(Connect),
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let args: Vec<String> = args.into_iter().collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    match cmd {
        "host" => parse_host(&args[1..]),
        "connect" => parse_connect(&args[1..]),
        "--help" | "-h" | "help" => Err(usage()),
        other => Err(format!("unknown command {other}\n{}", usage())),
    }
}

fn parse_host(args: &[String]) -> Result<Command, String> {
    let mut nic = None;
    let mut local_mac = HOST_MAC;
    let mut service_id = SERVICE_ID;
    let mut max_width = DEFAULT_MAX_WIDTH;
    let mut fps = DEFAULT_FPS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nic" => nic = Some(need(args, &mut i, "--nic")?),
            "--local-mac" => {
                local_mac = mac(need(args, &mut i, "--local-mac")?)?;
            }
            "--service-id" => service_id = number(need(args, &mut i, "--service-id")?)?,
            "--max-width" => max_width = number(need(args, &mut i, "--max-width")?)?,
            "--fps" => fps = number(need(args, &mut i, "--fps")?)?,
            other => return Err(format!("unknown arg {other}\n{}", usage())),
        }
    }
    let (cert, key) = cert_paths();
    Ok(Command::Host(Host {
        nic,
        local_mac,
        service_id,
        max_width,
        fps,
        cert,
        key,
    }))
}

fn parse_connect(args: &[String]) -> Result<Command, String> {
    let mut nic = None;
    let mut local_mac = CLIENT_MAC;
    let mut peer_mac = None;
    let mut service_id = SERVICE_ID;
    let mut udp = None;
    let mut trace_input = false;
    let mut view = ViewOptions::default();
    let mut parent_pipe = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nic" => nic = Some(need(args, &mut i, "--nic")?),
            "--local-mac" => {
                local_mac = mac(need(args, &mut i, "--local-mac")?)?;
            }
            "--trace-input" => flag(&mut i, &mut trace_input),
            "--no-keyboard" => off(&mut i, &mut view.send_keyboard),
            "--no-mouse" => off(&mut i, &mut view.send_mouse),
            "--no-pads" => off(&mut i, &mut view.send_pads),
            "--parent-pipe" => flag(&mut i, &mut parent_pipe),
            "--scale" => {
                let token = need(args, &mut i, "--scale")?;
                view.scale = ScaleMode::from_token(&token).ok_or_else(|| format!("bad --scale {token}"))?;
            }
            "--size-match" => {
                let token = need(args, &mut i, "--size-match")?;
                view.size_match =
                    SizeMatch::from_token(&token).ok_or_else(|| format!("bad --size-match {token}"))?;
            }
            "--output-device" => view.output_device = Some(need(args, &mut i, "--output-device")?),
            "--pad" => view.pad_slots.push(pad(need(args, &mut i, "--pad")?)?),
            "--peer-mac" => peer_mac = Some(mac(need(args, &mut i, "--peer-mac")?)?),
            "--service-id" => service_id = number(need(args, &mut i, "--service-id")?)?,
            "--udp" => udp = Some(number(need(args, &mut i, "--udp")?)?),
            other => return Err(format!("unknown arg {other}\n{}", usage())),
        }
    }
    let peer_mac = match (peer_mac, udp) {
        (Some(mac), _) => mac,
        (None, Some(_)) => HOST_MAC,
        (None, None) => return Err(format!("missing --peer-mac or --udp\n{}", usage())),
    };
    Ok(Command::Connect(Connect {
        nic,
        local_mac,
        peer_mac,
        service_id,
        udp,
        trace_input,
        view,
        parent_pipe,
    }))
}

fn flag(i: &mut usize, value: &mut bool) {
    *value = true;
    *i += 1;
}

fn off(i: &mut usize, value: &mut bool) {
    *value = false;
    *i += 1;
}

/// `SLOT=NAME`: send the local controller NAME as host slot SLOT (0 to 3).
fn pad(text: String) -> Result<PadSlot, String> {
    let (slot, name) = text.split_once('=').ok_or_else(|| format!("bad --pad {text}, want SLOT=NAME"))?;
    let slot: u8 = slot.parse().map_err(|_| format!("bad --pad slot {slot}"))?;
    if slot >= PAD_SLOTS || name.is_empty() {
        return Err(format!("bad --pad {text}"));
    }
    Ok(PadSlot { name: name.to_string(), slot })
}

fn need(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*i + 1)
        .cloned()
        .ok_or_else(|| format!("missing {flag}"))?;
    *i += 2;
    Ok(value)
}

fn mac(text: String) -> Result<[u8; 6], String> {
    parse_mac(&text).ok_or_else(|| format!("bad mac {text}"))
}

fn number<T: std::str::FromStr>(text: String) -> Result<T, String> {
    text.parse::<T>().map_err(|_| format!("bad number {text}"))
}

pub fn cert_paths() -> (PathBuf, PathBuf) {
    if let Ok(dir) = env::var("ETHERQUIC_CERTS") {
        let dir = PathBuf::from(dir);
        return (dir.join("cert.pem"), dir.join("key.pem"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cert = dir.join("cert.pem");
            let key = dir.join("key.pem");
            if cert.is_file() && key.is_file() {
                return (cert, key);
            }
        }
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs");
    (dir.join("cert.pem"), dir.join("key.pem"))
}

fn usage() -> String {
    "\
etherquic-play host [--nic IFACE] [--local-mac MAC] [--service-id 4433] [--max-width 1280] [--fps 10]
etherquic-play connect --peer-mac MAC [--nic IFACE] [--local-mac MAC] [--service-id 4433]
etherquic-play connect --udp IP:PORT
  connect also takes: [--trace-input] [--scale fit|fill|stretch|native]
    [--size-match independent|window-to-host] [--no-keyboard] [--no-mouse] [--no-pads]
    [--output-device NAME] [--pad SLOT=NAME]... [--parent-pipe]

host shares this desktop, its audio, and takes keyboard, mouse, and gamepad from one player.
connect opens the view. The player window has to be focused to send the keyboard and the mouse. Gamepads send while that view is open.
"
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(args: &[&str]) -> Connect {
        match parse(args.iter().map(|s| s.to_string())) {
            Ok(Command::Connect(connect)) => connect,
            Ok(_) => panic!("not connect"),
            Err(err) => panic!("{err}"),
        }
    }

    #[test]
    fn connect_defaults_match_the_window_defaults() {
        let c = connect(&["connect", "--udp", "192.168.0.58:4434"]);
        assert_eq!(c.udp, Some("192.168.0.58:4434".parse().unwrap()));
        assert_eq!(c.view, ViewOptions::default());
        assert!(!c.trace_input && !c.parent_pipe);
    }

    #[test]
    fn connect_takes_the_view_settings() {
        let c = connect(&[
            "connect", "--udp", "[::1]:4434", "--trace-input", "--scale", "native", "--size-match",
            "window-to-host", "--no-keyboard", "--no-pads", "--output-device", "MacBook Air Speakers",
            "--pad", "1=Xbox Controller", "--parent-pipe",
        ]);
        assert!(c.trace_input && c.parent_pipe);
        assert_eq!(c.view.scale, ScaleMode::Native);
        assert_eq!(c.view.size_match, SizeMatch::WindowToHost);
        assert!(!c.view.send_keyboard && c.view.send_mouse && !c.view.send_pads);
        assert_eq!(c.view.output_device.as_deref(), Some("MacBook Air Speakers"));
        assert_eq!(c.view.pad_slots, vec![PadSlot { name: "Xbox Controller".into(), slot: 1 }]);
    }

    #[test]
    fn bad_view_settings_are_errors() {
        for args in [
            &["connect", "--udp", "1.2.3.4:1", "--scale", "zoom"][..],
            &["connect", "--udp", "1.2.3.4:1", "--pad", "4=X"][..],
            &["connect", "--udp", "1.2.3.4:1", "--pad", "nameonly"][..],
        ] {
            assert!(parse(args.iter().map(|s| s.to_string())).is_err());
        }
    }
}
