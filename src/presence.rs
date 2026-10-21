//! What the desk reads from a host service: whether it is running, whether it
//! is hosting, and which players are connected.
//!
//! The service writes [`presence_path`]. The desk writes [`adjust_path`] when
//! hosting settings change. Neither file is `host.conf`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    pub running: bool,
    pub hosting: bool,
    pub guests: Vec<String>,
}

impl Default for Presence {
    fn default() -> Self {
        Self {
            running: false,
            hosting: false,
            guests: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostAdjust {
    pub dirty_rects: Option<bool>,
    pub capture_fps: Option<u32>,
    pub nic: Option<String>,
    pub audio_device: Option<String>,
}

pub fn program_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("etherquic")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/etherquic")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/var/lib/etherquic")
    }
    #[cfg(not(any(windows, unix)))]
    {
        PathBuf::from("etherquic")
    }
}

pub fn host_conf_path() -> PathBuf {
    if let Some(config) = std::env::var_os("ETHERQUIC_CONFIG") {
        return PathBuf::from(config);
    }
    program_data_dir().join("host.conf")
}

/// A host.conf on disk is an installed service, even when it is not running.
pub fn host_conf_installed() -> bool {
    std::fs::metadata(host_conf_path())
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

pub fn presence_path() -> PathBuf {
    config_dir().join("presence.txt")
}

pub fn adjust_path() -> PathBuf {
    config_dir().join("control").join("host-settings")
}

fn config_dir() -> PathBuf {
    if let Some(config) = std::env::var_os("ETHERQUIC_CONFIG") {
        let config = PathBuf::from(config);
        if let Some(dir) = config.parent() {
            if !dir.as_os_str().is_empty() {
                return dir.to_path_buf();
            }
        }
    }
    program_data_dir()
}

pub fn render(presence: &Presence) -> String {
    let mut out = format!(
        "running={}\nhosting={}\n",
        yes_no(presence.running),
        yes_no(presence.hosting)
    );
    for guest in &presence.guests {
        let guest = guest.trim();
        if !guest.is_empty() {
            out.push_str("guest=");
            out.push_str(guest);
            out.push('\n');
        }
    }
    out
}

pub fn parse(text: &str) -> Presence {
    let mut presence = Presence::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "running" => presence.running = parse_yes(value),
            "hosting" => presence.hosting = parse_yes(value),
            "guest" if !value.is_empty() => presence.guests.push(value.to_string()),
            _ => {}
        }
    }
    presence
}

pub fn load() -> Presence {
    match std::fs::read_to_string(presence_path()) {
        Ok(text) => parse(&text),
        Err(_) => Presence::default(),
    }
}

pub fn store(path: &Path, presence: &Presence) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, render(presence))?;
    std::fs::rename(&tmp, path)
}

/// What a host-settings file does to a host that is already up.
/// Dirty rectangles and capture rate apply now. Audio waits for the next
/// capture open. A different adapter rewrites the listener and does not stop
/// the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEffect {
    pub dirty_rects: Option<bool>,
    pub capture_fps: Option<u32>,
    pub audio_on_next_open: Option<String>,
    pub reopen_nic: Option<String>,
    pub stop: bool,
}

pub fn host_effect(current_nic: &str, adjust: &HostAdjust) -> HostEffect {
    HostEffect {
        dirty_rects: adjust.dirty_rects,
        capture_fps: adjust.capture_fps,
        audio_on_next_open: adjust.audio_device.clone(),
        // An empty name is Auto. It still reopens when the listener is on one adapter.
        reopen_nic: adjust.nic.clone().filter(|nic| nic != current_nic),
        stop: false,
    }
}

fn next_audio() -> &'static Mutex<Option<String>> {
    static NEXT: Mutex<Option<String>> = Mutex::new(None);
    &NEXT
}

/// Remember the share device until capture opens again.
pub fn stage_audio_device(name: Option<String>) {
    let name = name.filter(|name| !name.trim().is_empty());
    match next_audio().lock() {
        Ok(mut slot) => *slot = name,
        Err(poisoned) => *poisoned.into_inner() = name,
    }
}

pub fn staged_audio_device() -> Option<String> {
    match next_audio().lock() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// The listed device whose name matches the request. `None` is the default.
pub fn pick_audio_device<'a>(requested: Option<&str>, names: &'a [String]) -> Option<&'a str> {
    let requested = requested.map(str::trim).filter(|name| !name.is_empty())?;
    names
        .iter()
        .find(|name| name.eq_ignore_ascii_case(requested))
        .or_else(|| {
            let needle = requested.to_ascii_lowercase();
            names
                .iter()
                .find(|name| name.to_ascii_lowercase().contains(&needle))
        })
        .map(String::as_str)
}

/// Label for the device the next capture open should use.
pub fn audio_open_label(requested: Option<&str>, names: &[String]) -> String {
    if let Some(found) = pick_audio_device(requested, names) {
        return found.to_string();
    }
    match requested.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => name.to_string(),
        None => "default output".to_string(),
    }
}

/// Share-device name the service leaves for the capture helper.
///
/// The helper is another process, so the in-memory stage does not reach it.
/// The next time that helper opens capture it reads this file.
pub fn audio_next_path() -> PathBuf {
    config_dir().join("control").join("audio-next")
}

/// Capture rate the service leaves for the capture helper. `0` is not written.
pub fn capture_fps_path() -> PathBuf {
    config_dir().join("control").join("capture-fps")
}

pub fn write_audio_next(path: &Path, name: Option<&str>) -> std::io::Result<()> {
    match name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, name)
        }
        None => match std::fs::remove_file(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

pub fn read_audio_next(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub fn write_capture_fps(path: &Path, fps: u32) -> std::io::Result<()> {
    if fps == 0 {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, fps.to_string())
}

pub fn read_capture_fps(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let fps = text.trim().parse::<u32>().ok()?;
    (fps > 0).then_some(fps)
}

pub fn render_adjust(adjust: &HostAdjust) -> String {
    let mut out = String::new();
    if let Some(on) = adjust.dirty_rects {
        out.push_str(&format!("dirty-rects={}\n", yes_no(on)));
    }
    if let Some(fps) = adjust.capture_fps {
        out.push_str(&format!("capture-fps={fps}\n"));
    }
    if let Some(nic) = &adjust.nic {
        out.push_str("nic=");
        out.push_str(nic);
        out.push('\n');
    }
    if let Some(audio) = &adjust.audio_device {
        out.push_str("audio=");
        out.push_str(audio);
        out.push('\n');
    }
    out
}

pub fn parse_adjust(text: &str) -> HostAdjust {
    let mut adjust = HostAdjust::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "dirty-rects" => adjust.dirty_rects = Some(parse_yes(value)),
            "capture-fps" => adjust.capture_fps = value.parse().ok(),
            "nic" => adjust.nic = Some(value.to_string()),
            "audio" => adjust.audio_device = Some(value.to_string()),
            _ => {}
        }
    }
    adjust
}

/// Read and remove a settings file the desk left for the service.
pub fn take_adjust(path: &Path) -> Option<HostAdjust> {
    let text = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    let adjust = parse_adjust(&text);
    let empty = adjust.dirty_rects.is_none()
        && adjust.capture_fps.is_none()
        && adjust.nic.is_none()
        && adjust.audio_device.is_none();
    (!empty).then_some(adjust)
}

fn yes_no(on: bool) -> &'static str {
    if on {
        "yes"
    } else {
        "no"
    }
}

fn parse_yes(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "yes" | "true" | "1" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_round_trip_keeps_guests() {
        let presence = Presence {
            running: true,
            hosting: true,
            guests: vec!["10.0.0.8:4433".into(), "aa:bb:cc:dd:ee:ff".into()],
        };
        let parsed = parse(&render(&presence));
        assert_eq!(parsed, presence);
    }

    #[test]
    fn a_missing_file_is_not_hosting() {
        let parsed = parse("");
        assert!(!parsed.running);
        assert!(!parsed.hosting);
        assert!(parsed.guests.is_empty());
    }

    #[test]
    fn adjust_round_trip() {
        let adjust = HostAdjust {
            dirty_rects: Some(true),
            capture_fps: Some(60),
            nic: Some("enp5s0".into()),
            audio_device: Some("Analog".into()),
        };
        assert_eq!(parse_adjust(&render_adjust(&adjust)), adjust);
    }

    #[test]
    fn a_running_host_applies_picture_now_and_reopens_without_stopping() {
        let adjust = HostAdjust {
            dirty_rects: Some(true),
            capture_fps: Some(30),
            nic: Some("enp5s0".into()),
            audio_device: Some("Analog".into()),
        };
        let effect = host_effect("wlan0", &adjust);
        assert_eq!(effect.dirty_rects, Some(true));
        assert_eq!(effect.capture_fps, Some(30));
        assert_eq!(effect.audio_on_next_open.as_deref(), Some("Analog"));
        assert_eq!(effect.reopen_nic.as_deref(), Some("enp5s0"));
        assert!(!effect.stop);
        let same = host_effect("enp5s0", &adjust);
        assert_eq!(same.reopen_nic, None);
        assert!(!same.stop);
        let clear = HostAdjust {
            dirty_rects: None,
            capture_fps: None,
            nic: Some(String::new()),
            audio_device: None,
        };
        let written = render_adjust(&clear);
        assert_eq!(written, "nic=\n");
        assert_eq!(parse_adjust(&written).nic.as_deref(), Some(""));
        let back = host_effect("enp5s0", &parse_adjust(&written));
        assert_eq!(back.reopen_nic.as_deref(), Some(""));
        assert!(!back.stop);
        let already = host_effect("", &parse_adjust(&written));
        assert_eq!(already.reopen_nic, None);
        assert!(!already.stop);
        stage_audio_device(effect.audio_on_next_open);
        assert_eq!(staged_audio_device().as_deref(), Some("Analog"));
        let names = vec!["Default".into(), "Analog Stereo".into()];
        assert_eq!(
            pick_audio_device(Some("analog"), &names),
            Some("Analog Stereo")
        );
        assert_eq!(
            audio_open_label(Some("Analog Stereo"), &names),
            "Analog Stereo"
        );
        assert_eq!(audio_open_label(None, &names), "default output");
        stage_audio_device(None);
        let dir = std::env::temp_dir().join(format!(
            "etherquic-audio-next-{}-{}",
            std::process::id(),
            "host-effect"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let audio = dir.join("audio-next");
        let fps = dir.join("capture-fps");
        write_audio_next(&audio, Some("Analog Stereo")).unwrap();
        assert_eq!(read_audio_next(&audio).as_deref(), Some("Analog Stereo"));
        write_capture_fps(&fps, 30).unwrap();
        assert_eq!(read_capture_fps(&fps), Some(30));
        write_audio_next(&audio, None).unwrap();
        write_capture_fps(&fps, 0).unwrap();
        assert_eq!(read_audio_next(&audio), None);
        assert_eq!(read_capture_fps(&fps), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
