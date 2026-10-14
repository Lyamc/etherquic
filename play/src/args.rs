//! `host` and `connect` share the same link flags as the other L2 tools.

use std::env;
use std::path::PathBuf;

use etherquic::parse_mac;
use etherquic_play::{CLIENT_MAC, DEFAULT_FPS, DEFAULT_MAX_WIDTH, HOST_MAC, SERVICE_ID};

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
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nic" => nic = Some(need(args, &mut i, "--nic")?),
            "--local-mac" => {
                local_mac = mac(need(args, &mut i, "--local-mac")?)?;
            }
            "--peer-mac" => peer_mac = Some(mac(need(args, &mut i, "--peer-mac")?)?),
            "--service-id" => service_id = number(need(args, &mut i, "--service-id")?)?,
            other => return Err(format!("unknown arg {other}\n{}", usage())),
        }
    }
    let peer_mac = peer_mac.ok_or_else(|| format!("missing --peer-mac\n{}", usage()))?;
    Ok(Command::Connect(Connect {
        nic,
        local_mac,
        peer_mac,
        service_id,
    }))
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

host shares this desktop, its audio, and takes keyboard, mouse, and gamepad from one player.
connect opens the view. The player window has to be focused to send the keyboard and the mouse. Gamepads send while that view is open.
"
    .to_string()
}
