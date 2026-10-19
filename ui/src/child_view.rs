//! The picture window in a child `etherquic-play connect` process.
//!
//! macOS only runs a winit loop on the main thread, and GPUI already owns it,
//! so the app cannot open the picture window itself. It starts the bundled
//! `etherquic-play` with the chosen address, transport and view settings, turns
//! the child's stderr into session status, and its exit into an error. Other
//! platforms keep the in-process window.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use etherquic_play::ViewOptions;

const PLAY_EXE: &str = if cfg!(windows) {
    "etherquic-play.exe"
} else {
    "etherquic-play"
};

/// Where to dial and how the window behaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildConnect {
    pub udp: Option<SocketAddr>,
    pub nic: Option<String>,
    pub peer_mac: [u8; 6],
    pub local_mac: [u8; 6],
    pub service_id: u16,
    pub trace_input: bool,
    pub view: ViewOptions,
    /// Codec order and pins, so the child offers what the Settings say.
    pub codecs: etherquic_play::CodecChoice,
}

/// `etherquic-play` arguments for one session. The child stops when its stdin closes.
pub fn connect_args(c: &ChildConnect) -> Vec<String> {
    let mut args = vec!["connect".to_string()];
    match c.udp {
        Some(addr) => args.extend(["--udp".into(), addr.to_string()]),
        None => {
            args.extend(["--peer-mac".into(), crate::format_mac(c.peer_mac)]);
            if let Some(nic) = &c.nic {
                args.extend(["--nic".into(), nic.clone()]);
            }
            args.extend(["--service-id".into(), c.service_id.to_string()]);
        }
    }
    args.extend(["--local-mac".into(), crate::format_mac(c.local_mac)]);
    if c.trace_input {
        args.push("--trace-input".into());
    }
    let v = &c.view;
    args.extend(["--scale".into(), v.scale.token().into()]);
    args.extend(["--cursor".into(), v.cursor.token().into()]);
    args.extend(["--look".into(), v.look.token().into()]);
    args.extend(["--dither".into(), v.dither.token().into()]);
    args.extend(["--pixel-size".into(), v.pixel_size.to_string()]);
    args.extend(["--dither-strength".into(), v.dither_strength.to_string()]);
    args.extend(["--quality".into(), v.quality.to_string()]);
    if v.dirty_rects {
        args.push("--dirty-rects".into());
    }
    if v.debug_dirty_rects {
        args.push("--debug-dirty-rects".into());
    }
    args.extend(["--yuv".into(), v.yuv.token().into()]);
    if v.size_match.available() {
        args.extend(["--size-match".into(), v.size_match.token().into()]);
    }
    for (on, flag) in [
        (v.send_keyboard, "--no-keyboard"),
        (v.send_mouse, "--no-mouse"),
        (v.send_pads, "--no-pads"),
    ] {
        if !on {
            args.push(flag.into());
        }
    }
    if let Some(device) = &v.output_device {
        args.extend(["--output-device".into(), device.clone()]);
    }
    for pad in &v.pad_slots {
        args.extend(["--pad".into(), format!("{}={}", pad.slot, pad.name)]);
    }
    args.extend(c.codecs.args());
    args.push("--parent-pipe".into());
    args
}

/// `ETHERQUIC_PLAY`, then next to this app (inside the .app bundle that is
/// `Contents/MacOS`), then `PATH`, then the cargo target dirs of a checkout.
pub fn play_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ETHERQUIC_PLAY") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(PLAY_EXE)));
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut dev = Vec::new();
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        dev.push(PathBuf::from(dir));
    }
    dev.push(checkout.join("target"));
    dev.push(checkout.join("play").join("target"));
    dev.push(checkout.join("ui").join("target"));
    find_play(beside, &path_dirs, &dev)
}

fn find_play(
    beside: Option<PathBuf>,
    path_dirs: &[PathBuf],
    target_dirs: &[PathBuf],
) -> Option<PathBuf> {
    beside
        .into_iter()
        .chain(path_dirs.iter().map(|dir| dir.join(PLAY_EXE)))
        .chain(
            target_dirs.iter().flat_map(|dir| {
                ["release", "debug"].map(|profile| dir.join(profile).join(PLAY_EXE))
            }),
        )
        .find(|path| path.is_file())
}

/// A stderr line from the child.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// Session status such as "Dialing …" or "Viewing …".
    Status(String),
    /// Media trace from the player. Not shown.
    Trace,
    /// `etherquic-play: …`, kept as the reason when the child fails.
    Problem(String),
}

pub fn classify(line: &str) -> Line {
    let line = line.trim_end();
    match line.strip_prefix("etherquic-play: ") {
        Some(rest) => {
            const TRACE: [&str; 9] = [
                "frame ",
                "audio ",
                "playing ",
                "picture ",
                "session ",
                "stats ",
                "offer ",
                "decoder ",
                "the host does not negotiate",
            ];
            if TRACE.iter().any(|t| rest.starts_with(t)) {
                Line::Trace
            } else {
                Line::Problem(rest.to_string())
            }
        }
        None if line.is_empty() => Line::Trace,
        None => Line::Status(line.to_string()),
    }
}

/// What a finished child means for the status line.
pub fn exit_error(code: Option<i32>, problem: Option<&str>) -> Option<String> {
    match code {
        Some(0) => None,
        Some(code) => Some(match problem {
            Some(problem) => problem.to_string(),
            None => format!("etherquic-play exited with code {code}"),
        }),
        None => Some(
            problem
                .unwrap_or("etherquic-play was stopped by a signal")
                .to_string(),
        ),
    }
}

/// Run one session in the child and block until it ends or `stop` is set.
/// Status lines go to `report`. Returns the failure reason, if any.
pub fn run(
    connect: &ChildConnect,
    stop: &AtomicBool,
    report: &(dyn Fn(&str) + Send + Sync),
) -> Result<(), String> {
    let bin = play_binary().ok_or_else(|| {
        format!("{PLAY_EXE} was not found next to the app, on PATH, or in a cargo target dir")
    })?;
    let mut child = Command::new(&bin)
        .args(connect_args(connect))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not start {}: {err}", bin.display()))?;
    report("Connecting…");
    let (tx, rx) = mpsc::channel::<String>();
    let stderr = child.stderr.take();
    let reader = std::thread::Builder::new()
        .name("etherquic-play-stderr".into())
        .spawn(move || {
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        })
        .ok();
    let mut problem = None::<String>;
    let drain = |problem: &mut Option<String>| {
        while let Ok(line) = rx.try_recv() {
            match classify(&line) {
                Line::Status(text) => report(&text),
                Line::Problem(text) => *problem = Some(text),
                Line::Trace => {}
            }
        }
    };
    let status = loop {
        drain(&mut problem);
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(err) => break Err(err.to_string()),
        }
        if stop.load(Ordering::SeqCst) {
            stop_child(&mut child);
            if let Some(reader) = reader {
                let _ = reader.join();
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    drain(&mut problem);
    let status = status?;
    match exit_error(status.code(), problem.as_deref()) {
        None => Ok(()),
        Some(err) => Err(err),
    }
}

/// Close stdin so the child ends the session itself, then kill it if it does not.
fn stop_child(child: &mut Child) {
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic_play::{PadSlot, ScaleMode};

    fn udp() -> ChildConnect {
        ChildConnect {
            udp: Some("192.168.0.58:4434".parse().unwrap()),
            nic: None,
            peer_mac: [2, 0, 0, 0, 0, 1],
            local_mac: [2, 0, 0, 0, 0, 2],
            service_id: 4433,
            trace_input: false,
            view: ViewOptions::default(),
            codecs: etherquic_play::CodecChoice::default(),
        }
    }

    #[test]
    fn codec_choice_reaches_the_child() {
        let mut connect = udp();
        connect.codecs.video = vec![
            etherquic_play::PicturePref::Pyrowave,
            etherquic_play::PicturePref::Mjpeg,
        ];
        connect.codecs.video_pin = Some(etherquic_play::PicturePref::Pyrowave);
        let args = connect_args(&connect);
        let at = args.iter().position(|a| a == "--video").expect("--video");
        assert_eq!(args[at + 1], "pyrowave,mjpeg");
        let at = args
            .iter()
            .position(|a| a == "--video-pin")
            .expect("--video-pin");
        assert_eq!(args[at + 1], "pyrowave");
        assert!(args.ends_with(&["--parent-pipe".to_string()]));
        assert_eq!(
            classify(
                "etherquic-play: session video pyrowave encoder pyrowave 1280x720 30 fps 0 kbit/s"
            ),
            Line::Trace
        );
    }

    #[test]
    fn udp_session_args() {
        let args = connect_args(&udp());
        assert_eq!(&args[..3], ["connect", "--udp", "192.168.0.58:4434"]);
        assert!(!args.contains(&"--peer-mac".to_string()));
        assert!(args.ends_with(&["--parent-pipe".to_string()]));
        assert!(!args.iter().any(|a| a.starts_with("--no-")));
    }

    #[test]
    fn l2_session_args_carry_the_view_settings() {
        let mut c = udp();
        c.udp = None;
        c.nic = Some("en0".into());
        c.trace_input = true;
        c.view.scale = ScaleMode::Fill;
        c.view.dirty_rects = true;
        c.view.debug_dirty_rects = true;
        c.view.send_mouse = false;
        c.view.output_device = Some("Speakers".into());
        c.view.pad_slots = vec![PadSlot {
            name: "Pad".into(),
            slot: 2,
        }];
        let args = connect_args(&c).join(" ");
        assert!(
            args.starts_with("connect --peer-mac 02:00:00:00:00:01 --nic en0 --service-id 4433")
        );
        for part in [
            "--trace-input",
            "--scale fill",
            "--cursor native",
            "--look off",
            "--dirty-rects",
            "--debug-dirty-rects",
            "--no-mouse",
            "--output-device Speakers",
            "--pad 2=Pad",
        ] {
            assert!(args.contains(part), "{part} in {args}");
        }
    }

    #[test]
    fn stderr_lines_become_status_or_errors() {
        assert_eq!(
            classify("Dialing 192.168.0.58:4434."),
            Line::Status("Dialing 192.168.0.58:4434.".into())
        );
        assert_eq!(
            classify("etherquic-play: frame 1280x720 jpeg 15026 bytes"),
            Line::Trace
        );
        assert_eq!(
            classify("etherquic-play: timed out"),
            Line::Problem("timed out".into())
        );
        assert_eq!(classify("   "), Line::Trace);
    }

    #[test]
    fn exit_codes_become_errors() {
        assert_eq!(exit_error(Some(0), Some("ignored")), None);
        assert_eq!(
            exit_error(Some(1), Some("timed out")),
            Some("timed out".into())
        );
        assert_eq!(
            exit_error(Some(3), None),
            Some("etherquic-play exited with code 3".into())
        );
        assert!(exit_error(None, None).is_some());
    }

    #[test]
    fn finds_the_binary_beside_the_app_first() {
        let dir = std::env::temp_dir().join(format!("eq-child-view-{}", std::process::id()));
        let app = dir.join("Contents").join("MacOS");
        let target = dir.join("target").join("debug");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(PLAY_EXE), b"x").unwrap();
        let found = find_play(Some(app.join(PLAY_EXE)), &[], &[dir.join("target")]);
        assert_eq!(found, Some(target.join(PLAY_EXE)));
        std::fs::write(app.join(PLAY_EXE), b"x").unwrap();
        let found = find_play(Some(app.join(PLAY_EXE)), &[], &[dir.join("target")]);
        assert_eq!(found, Some(app.join(PLAY_EXE)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
