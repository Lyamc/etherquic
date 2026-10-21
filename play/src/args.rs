//! `host` and `connect` share the same link flags as the other L2 tools.

use std::env;
use std::path::PathBuf;

use etherquic::parse_mac;
use crate::{
    CursorMode, Dither, FrameGen, Look, PadSlot, ScaleMode, SizeMatch, Upscale, ViewOptions,
    CLIENT_MAC, DEFAULT_MAX_WIDTH, HOST_MAC, PAD_SLOTS, SERVICE_ID,
};

pub struct Host {
    pub nic: Option<String>,
    pub local_mac: [u8; 6],
    pub service_id: u16,
    pub max_width: u32,
    pub fps: etherquic::negotiate::FpsWish,
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
    /// L2 Connect already holds a discovery offer. Carriers 3–5 use `rtp` only.
    pub l2_offer: bool,
    /// Host IP and RTP input port from that offer. Port 0 fails the view.
    pub rtp: Option<std::net::SocketAddr>,
    /// Ask the host to confirm stamped input.
    pub trace_input: bool,
    /// Scale, what to send, pad slots, and the speaker.
    pub view: ViewOptions,
    /// Stop when stdin closes. A parent process (the macOS app) uses this to end
    /// the session cleanly.
    pub parent_pipe: bool,
    /// Codec order and pins for the offer.
    pub codecs: crate::CodecChoice,
    /// `plain` or `gcm`. Absent leaves UDP/RTP on `gcm` and ST 2110 on `plain`.
    pub profile: Option<String>,
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
    let mut fps = etherquic::negotiate::FpsWish::Auto;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nic" => nic = Some(need(args, &mut i, "--nic")?),
            "--local-mac" => {
                local_mac = mac(need(args, &mut i, "--local-mac")?)?;
            }
            "--service-id" => service_id = number(need(args, &mut i, "--service-id")?)?,
            "--max-width" => max_width = number(need(args, &mut i, "--max-width")?)?,
            "--fps" => {
                fps = etherquic::negotiate::FpsWish::Fixed(number(need(args, &mut i, "--fps")?)?)
            }
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
    let mut l2_offer = false;
    let mut rtp = None;
    let mut trace_input = false;
    let mut view = ViewOptions::default();
    let mut parent_pipe = false;
    let mut codecs = crate::CodecChoice::default();
    let mut profile = None;
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
                view.scale =
                    ScaleMode::from_token(&token).ok_or_else(|| format!("bad --scale {token}"))?;
            }
            "--cursor" => {
                let token = need(args, &mut i, "--cursor")?;
                view.cursor = CursorMode::from_token(&token)
                    .ok_or_else(|| format!("bad --cursor {token}"))?;
            }
            "--look" => {
                let token = need(args, &mut i, "--look")?;
                view.look =
                    Look::from_token(&token).ok_or_else(|| format!("bad --look {token}"))?;
            }
            "--dither" => {
                let token = need(args, &mut i, "--dither")?;
                view.dither =
                    Dither::from_token(&token).ok_or_else(|| format!("bad --dither {token}"))?;
            }
            "--pixel-size" => {
                let value: u32 = number(need(args, &mut i, "--pixel-size")?)?;
                if !(1..=32).contains(&value) {
                    return Err(format!("--pixel-size {value} is not 1-32"));
                }
                view.pixel_size = value as u8;
            }
            "--dither-strength" => {
                let value: u32 = number(need(args, &mut i, "--dither-strength")?)?;
                if value > 100 {
                    return Err(format!("--dither-strength {value} is not 0-100"));
                }
                view.dither_strength = value as u8;
            }
            "--size-match" => {
                let token = need(args, &mut i, "--size-match")?;
                view.size_match = SizeMatch::from_token(&token)
                    .ok_or_else(|| format!("bad --size-match {token}"))?;
            }
            "--upscale" => {
                let token = need(args, &mut i, "--upscale")?;
                view.upscale = Upscale::from_token(&token)
                    .ok_or_else(|| format!("bad --upscale {token}"))?;
            }
            "--frame-gen" => {
                let token = need(args, &mut i, "--frame-gen")?;
                view.frame_gen = FrameGen::from_token(&token)
                    .ok_or_else(|| format!("bad --frame-gen {token}"))?;
            }
            "--output-device" => view.output_device = Some(need(args, &mut i, "--output-device")?),
            "--pad" => view.pad_slots.push(pad(need(args, &mut i, "--pad")?)?),
            "--peer-mac" => peer_mac = Some(mac(need(args, &mut i, "--peer-mac")?)?),
            "--service-id" => service_id = number(need(args, &mut i, "--service-id")?)?,
            "--udp" => udp = Some(number(need(args, &mut i, "--udp")?)?),
            "--l2-offer" => flag(&mut i, &mut l2_offer),
            "--rtp" => rtp = Some(number(need(args, &mut i, "--rtp")?)?),
            "--video" => {
                codecs.video =
                    crate::CodecChoice::parse_video(&need(args, &mut i, "--video")?)
            }
            "--audio" => {
                codecs.audio =
                    crate::CodecChoice::parse_audio(&need(args, &mut i, "--audio")?)
            }
            "--video-pin" => {
                let token = need(args, &mut i, "--video-pin")?;
                codecs.video_pin = Some(
                    crate::PicturePref::from_token(&token)
                        .ok_or_else(|| format!("bad --video-pin {token}"))?,
                );
            }
            "--audio-pin" => {
                let token = need(args, &mut i, "--audio-pin")?;
                codecs.audio_pin = Some(
                    crate::AudioId::from_token(&token)
                        .ok_or_else(|| format!("bad --audio-pin {token}"))?,
                );
            }
            "--max-width" => codecs.max_width = number(need(args, &mut i, "--max-width")?)?,
            "--max-height" => codecs.max_height = number(need(args, &mut i, "--max-height")?)?,
            "--bitrate" => codecs.bitrate_kbps = number(need(args, &mut i, "--bitrate")?)?,
            "--quality" => {
                let value: u32 = number(need(args, &mut i, "--quality")?)?;
                if !(1..=100).contains(&value) {
                    return Err(format!("--quality {value} is not 1-100"));
                }
                codecs.quality = value as u8;
                view.quality = codecs.quality;
            }
            "--dirty-rects" => {
                flag(&mut i, &mut codecs.dirty_rects);
                view.dirty_rects = true;
            }
            "--debug-dirty-rects" => flag(&mut i, &mut view.debug_dirty_rects),
            "--yuv" => {
                let token = need(args, &mut i, "--yuv")?;
                let yuv = etherquic::negotiate::YuvFormat::from_token(&token)
                    .ok_or_else(|| format!("bad --yuv {token}"))?;
                codecs.yuv = yuv;
                view.yuv = yuv;
            }
            "--fps" => {
                let token = need(args, &mut i, "--fps")?;
                codecs.fps = etherquic::negotiate::FpsWish::from_token(&token);
            }
            "--audio-bitrate" => {
                codecs.audio_bitrate_kbps = number(need(args, &mut i, "--audio-bitrate")?)?
            }
            "--audio-channels" => {
                let n: u32 = number(need(args, &mut i, "--audio-channels")?)?;
                if n > etherquic::negotiate::AUDIO_CHANNELS_MAX as u32 {
                    return Err(format!(
                        "--audio-channels {n} is above {}",
                        etherquic::negotiate::AUDIO_CHANNELS_MAX
                    ));
                }
                codecs.audio_channels = n as u16;
            }
            "--view-only" => flag(&mut i, &mut codecs.view_only),
            "--exclusive" => flag(&mut i, &mut codecs.exclusive),
            "--carrier" => {
                let value: u32 = number(need(args, &mut i, "--carrier")?)?;
                if !(1..=5).contains(&value) {
                    return Err(format!("--carrier {value} is not 1-5"));
                }
                codecs.carrier = Some(value as u8);
            }
            "--profile" => {
                let token = need(args, &mut i, "--profile")?;
                if token != "plain" && token != "gcm" {
                    return Err(format!("--profile {token} is not plain or gcm"));
                }
                profile = Some(token);
            }
            other => return Err(format!("unknown arg {other}\n{}", usage())),
        }
    }
    let peer_mac = match (peer_mac, udp) {
        (Some(mac), _) => mac,
        (None, Some(_)) => HOST_MAC,
        (None, None) => return Err(format!("missing --peer-mac or --udp\n{}", usage())),
    };
    if rtp.is_some() {
        l2_offer = true;
    }
    Ok(Command::Connect(Connect {
        nic,
        local_mac,
        peer_mac,
        service_id,
        udp,
        l2_offer,
        rtp,
        trace_input,
        view,
        parent_pipe,
        codecs,
        profile,
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
    let (slot, name) = text
        .split_once('=')
        .ok_or_else(|| format!("bad --pad {text}, want SLOT=NAME"))?;
    let slot: u8 = slot.parse().map_err(|_| format!("bad --pad slot {slot}"))?;
    if slot >= PAD_SLOTS || name.is_empty() {
        return Err(format!("bad --pad {text}"));
    }
    Ok(PadSlot {
        name: name.to_string(),
        slot,
    })
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
            let resources = dir.join("..").join("Resources");
            let cert = resources.join("cert.pem");
            let key = resources.join("key.pem");
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
etherquic-play host [--nic IFACE] [--local-mac MAC] [--service-id 4433] [--max-width 1280] [--fps 15]
etherquic-play connect --peer-mac MAC [--nic IFACE] [--local-mac MAC] [--service-id 4433]
etherquic-play connect --udp IP:PORT
  connect also takes: [--trace-input] [--scale fit|fill|stretch|native]
    [--cursor native|copy|remote] [--size-match independent|window-to-host]
    [--look off|blocks|poster|gameboy|pico8|…] [--dither bayer4|bluenoise|floyd-steinberg|…]
    [--pixel-size 1-32] [--dither-strength 0-100]
    [--upscale off|auto|fsr|dlss|xess] [--frame-gen off|auto|on]
    [--no-keyboard] [--no-mouse] [--no-pads]
    [--output-device NAME] [--pad SLOT=NAME]... [--parent-pipe]
    [--video av1-hw,h265-hw,h264-hw,pyrowave,jpegxs,openapv,mjpeg,h264-sw] [--audio opus,pcm]
    [--video-pin TOKEN] [--audio-pin TOKEN] [--max-width PX] [--max-height PX] [--bitrate KBPS]
    [--quality 1-100] [--dirty-rects] [--debug-dirty-rects] [--yuv 420p8|422p8|444p8|420p10|422p10|444p10]
    [--fps auto|match|N] [--audio-bitrate KBPS] [--audio-channels 0-8]
    [--view-only] [--exclusive] [--carrier 1-5] [--profile plain|gcm]
    [--l2-offer] [--rtp IP:PORT]

host shares this desktop, its audio, and takes keyboard, mouse, and gamepad from connected players.
connect opens the view. The player window has to be focused to send the keyboard and the mouse. Gamepads send while that view is open.
"
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_flags_reach_the_offer_choice() {
        let args = [
            "connect",
            "--udp",
            "1.2.3.4:5",
            "--video",
            "pyrowave,mjpeg,nope",
            "--audio",
            "opus,pcm",
            "--video-pin",
            "h264-sw",
            "--max-width",
            "1280",
            "--max-height",
            "720",
            "--bitrate",
            "9000",
            "--quality",
            "7",
            "--dirty-rects",
            "--fps",
            "60",
            "--audio-bitrate",
            "96",
            "--audio-channels",
            "6",
        ];
        let Ok(Command::Connect(c)) = parse(args.iter().map(|s| s.to_string())) else {
            panic!()
        };
        assert_eq!(
            c.codecs.video,
            vec![
                crate::PicturePref::Pyrowave,
                crate::PicturePref::Mjpeg
            ]
        );
        assert_eq!(
            c.codecs.audio,
            vec![crate::AudioId::Opus, crate::AudioId::Pcm]
        );
        assert_eq!(
            c.codecs.video_pin,
            Some(crate::PicturePref::H264Sw)
        );
        assert_eq!(c.codecs.max_width, 1280);
        assert_eq!(c.codecs.max_height, 720);
        assert_eq!(c.codecs.bitrate_kbps, 9000);
        assert_eq!(c.codecs.quality, 7);
        assert_eq!(c.codecs.fps, etherquic::negotiate::FpsWish::Fixed(60));
        assert_eq!(c.codecs.audio_bitrate_kbps, 96);
        assert_eq!(c.codecs.audio_channels, 6);
        assert_eq!(c.view.quality, 7);
        assert!(c.codecs.dirty_rects);
        assert!(c.view.dirty_rects);
        let back = c.codecs.args();
        assert!(back
            .windows(2)
            .any(|w| w[0] == "--video" && w[1] == "pyrowave,mjpeg"));
        assert!(back
            .windows(2)
            .any(|w| w[0] == "--video-pin" && w[1] == "h264-sw"));
    }

    fn connect(args: &[&str]) -> Connect {
        match parse(args.iter().map(|s| s.to_string())) {
            Ok(Command::Connect(connect)) => connect,
            Ok(_) => panic!("not connect"),
            Err(err) => panic!("{err}"),
        }
    }

    #[test]
    fn connect_takes_plain_or_gcm_and_rejects_other_tokens() {
        let plain = connect(&[
            "connect",
            "--udp",
            "127.0.0.1:9",
            "--profile",
            "plain",
        ]);
        assert_eq!(plain.profile.as_deref(), Some("plain"));
        let gcm = connect(&["connect", "--udp", "127.0.0.1:9", "--profile", "gcm"]);
        assert_eq!(gcm.profile.as_deref(), Some("gcm"));
        let absent = connect(&["connect", "--udp", "127.0.0.1:9"]);
        assert!(absent.profile.is_none());
        let err = parse(
            ["connect", "--udp", "127.0.0.1:9", "--profile", "none"]
                .into_iter()
                .map(str::to_string),
        );
        assert!(err.is_err());
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
            "connect",
            "--udp",
            "[::1]:4434",
            "--trace-input",
            "--scale",
            "native",
            "--cursor",
            "copy",
            "--look",
            "pico8",
            "--dither",
            "bluenoise",
            "--pixel-size",
            "8",
            "--dither-strength",
            "70",
            "--size-match",
            "window-to-host",
            "--upscale",
            "fsr",
            "--frame-gen",
            "on",
            "--no-keyboard",
            "--no-pads",
            "--output-device",
            "MacBook Air Speakers",
            "--pad",
            "1=Xbox Controller",
            "--parent-pipe",
        ]);
        assert!(c.trace_input && c.parent_pipe);
        assert_eq!(c.view.scale, ScaleMode::Native);
        assert_eq!(c.view.cursor, CursorMode::Copy);
        assert_eq!(c.view.look, Look::Pico8);
        assert_eq!(c.view.dither, Dither::BlueNoise);
        assert_eq!(c.view.pixel_size, 8);
        assert_eq!(c.view.dither_strength, 70);
        assert_eq!(c.view.size_match, SizeMatch::WindowToHost);
        assert_eq!(c.view.upscale, Upscale::Fsr);
        assert_eq!(c.view.frame_gen, FrameGen::On);
        assert!(!c.view.send_keyboard && c.view.send_mouse && !c.view.send_pads);
        assert_eq!(
            c.view.output_device.as_deref(),
            Some("MacBook Air Speakers")
        );
        assert_eq!(
            c.view.pad_slots,
            vec![PadSlot {
                name: "Xbox Controller".into(),
                slot: 1
            }]
        );
    }

    #[test]
    fn bad_view_settings_are_errors() {
        for args in [
            &["connect", "--udp", "1.2.3.4:1", "--scale", "zoom"][..],
            &["connect", "--udp", "1.2.3.4:1", "--cursor", "zoom"][..],
            &["connect", "--udp", "1.2.3.4:1", "--look", "zoom"][..],
            &["connect", "--udp", "1.2.3.4:1", "--upscale", "nis"][..],
            &["connect", "--udp", "1.2.3.4:1", "--frame-gen", "nis"][..],
            &["connect", "--udp", "1.2.3.4:1", "--dither", "zoom"][..],
            &["connect", "--udp", "1.2.3.4:1", "--pixel-size", "0"][..],
            &["connect", "--udp", "1.2.3.4:1", "--pad", "4=X"][..],
            &["connect", "--udp", "1.2.3.4:1", "--pad", "nameonly"][..],
        ] {
            assert!(parse(args.iter().map(|s| s.to_string())).is_err());
        }
    }
}
