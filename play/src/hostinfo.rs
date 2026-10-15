//! What a host adds to a discovery offer: its displays and inputs, and a
//! small desktop picture that fits in what is left of the reply frame.
//!
//! Both are cached. A background thread refreshes them every
//! [`REFRESH`], so an announce never waits for a capture or an encode.
//!
//! The picture is JPEG XL, modular and progressive, made by libjxl's `cjxl`
//! tool (BSD-3-Clause). The tool is found through `ETHERQUIC_CJXL`, next to
//! this program, or on `PATH`. Without it the offer carries no picture.
//! `cjxl` has no target size, so [`fit_to_budget`] searches the distance,
//! then smaller sizes, for the best picture that fits.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use etherquic::discover::{
    Capabilities, OfferExtras, PadInfo, INPUT_KEYBOARD, INPUT_MOUSE, INPUT_PEN, INPUT_TOUCH,
    OS_LINUX, OS_MACOS, OS_WINDOWS, PAD_API_EVDEV, PAD_KIND_GAMEPAD, PAD_KIND_STICK,
    PAD_KIND_WHEEL,
};

/// How often the capabilities and the picture are refreshed.
pub const REFRESH: Duration = Duration::from_secs(30);
pub use etherquic::thumbnail::{
    cjxl_path, downscale, encode_cjxl, fit_to_budget, Rgb, DISTANCE_MAX, DISTANCE_MIN,
    THUMB_WIDTHS,
};

/// Keyboard, mouse, touch, pen, and joysticks from `/proc/bus/input/devices`.
pub fn parse_proc_input(text: &str) -> (u16, Vec<PadInfo>) {
    let mut inputs = 0u16;
    let mut pads = Vec::new();
    for block in text.split("\n\n") {
        let mut name = String::new();
        let mut handlers = String::new();
        let mut ev = 0u64;
        let mut key = Vec::new();
        let mut abs = Vec::new();
        let mut prop = 0u64;
        for line in block.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("N: Name=") {
                name = rest.trim_matches('"').to_string();
            } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
                handlers = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("B: EV=") {
                ev = u64::from_str_radix(rest.trim(), 16).unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("B: KEY=") {
                key = bitmap(rest);
            } else if let Some(rest) = line.strip_prefix("B: ABS=") {
                abs = bitmap(rest);
            } else if let Some(rest) = line.strip_prefix("B: PROP=") {
                prop = u64::from_str_radix(rest.trim(), 16).unwrap_or(0);
            }
        }
        if name.is_empty() && handlers.is_empty() {
            continue;
        }
        let words: Vec<&str> = handlers.split_whitespace().collect();
        const EV_REP: u64 = 1 << 0x14;
        const INPUT_PROP_DIRECT: u64 = 1 << 1;
        // KEY_Q (16) and KEY_A (30) tell a keyboard from a power button.
        if words.contains(&"kbd") && ev & EV_REP != 0 && has_bit(&key, 16) && has_bit(&key, 30) {
            inputs |= INPUT_KEYBOARD;
        }
        let pen = has_bit(&key, 0x140); // BTN_TOOL_PEN
        let touch = prop & INPUT_PROP_DIRECT != 0 && has_bit(&abs, 0x35); // ABS_MT_POSITION_X
        if pen {
            inputs |= INPUT_PEN;
        } else if touch {
            inputs |= INPUT_TOUCH;
        } else if words.iter().any(|w| w.starts_with("mouse")) {
            inputs |= INPUT_MOUSE;
        }
        if words.iter().any(|w| w.starts_with("js")) {
            pads.push(PadInfo {
                api: PAD_API_EVDEV,
                kind: pad_kind(&name),
                name: name.chars().take(24).collect(),
            });
        }
    }
    (inputs, pads)
}

/// A sysfs bitmap: space-separated hex words, most significant first.
fn bitmap(text: &str) -> Vec<u64> {
    let mut words: Vec<u64> = text
        .split_whitespace()
        .map(|word| u64::from_str_radix(word, 16).unwrap_or(0))
        .collect();
    words.reverse();
    words
}

fn has_bit(words: &[u64], bit: usize) -> bool {
    // The kernel prints `long`s; on a 64-bit kernel each word is 64 bits.
    words
        .get(bit / 64)
        .is_some_and(|word| word & (1u64 << (bit % 64)) != 0)
}

/// Wheel or stick from the device name. Anything else is a gamepad.
pub fn pad_kind(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if lower.contains("wheel") || lower.contains("racing") {
        PAD_KIND_WHEEL
    } else if ["joystick", "flight", "hotas", "throttle", "stick"]
        .iter()
        .any(|word| lower.contains(word))
    {
        PAD_KIND_STICK
    } else {
        PAD_KIND_GAMEPAD
    }
}

/// Host name, OS, monitors, and inputs, read now. Cheap: no capture.
#[cfg(feature = "desktop")]
pub fn gather_capabilities(pads: Vec<PadInfo>) -> Capabilities {
    let (os, os_label) = os_identity();
    let monitors = xcap::Monitor::all()
        .map(|all| {
            all.iter()
                .filter_map(|monitor| {
                    Some(etherquic::discover::MonitorInfo {
                        width: monitor.width().ok()?.min(u16::MAX as u32) as u16,
                        height: monitor.height().ok()?.min(u16::MAX as u32) as u16,
                        refresh_centihz: (monitor.frequency().unwrap_or(0.0) * 100.0)
                            .round()
                            .clamp(0.0, u16::MAX as f32)
                            as u16,
                        x: monitor.x().unwrap_or(0).clamp(i16::MIN as i32, i16::MAX as i32)
                            as i16,
                        y: monitor.y().unwrap_or(0).clamp(i16::MIN as i32, i16::MAX as i32)
                            as i16,
                        primary: monitor.is_primary().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let (audio_outputs, audio_inputs) = crate::audio_devices();
    let (inputs, mut found_pads) = input_devices();
    // gilrs is the cross-platform list. On Linux evdev joysticks are added
    // only when gilrs did not see them.
    if !pads.is_empty() {
        found_pads = pads;
    }
    Capabilities {
        os,
        os_label,
        hostname: hostname(),
        nic_mac: None,
        monitors,
        inputs,
        virtual_pad: virtual_pad(),
        pads: found_pads,
        audio_outputs,
        audio_inputs,
        codecs: etherquic_media::caps::caps_text_if_ready(),
    }
}

fn os_identity() -> (u8, String) {
    if cfg!(windows) {
        (OS_WINDOWS, "Windows".into())
    } else if cfg!(target_os = "macos") {
        (OS_MACOS, "macOS".into())
    } else if cfg!(target_os = "linux") {
        let pretty = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                    .map(|value| value.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Linux".into());
        (OS_LINUX, pretty)
    } else {
        (0, std::env::consts::OS.into())
    }
}

fn hostname() -> String {
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        return name;
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

fn input_devices() -> (u16, Vec<PadInfo>) {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/bus/input/devices")
            .map(|text| parse_proc_input(&text))
            .unwrap_or_default()
    }
    #[cfg(windows)]
    {
        (etherquic::hostinput::windows_input_flags(), Vec::new())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        (INPUT_KEYBOARD | INPUT_MOUSE, Vec::new())
    }
}

#[cfg(feature = "desktop")]
fn virtual_pad() -> u8 {
    #[cfg(target_os = "linux")]
    {
        let ok = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_ok();
        if ok {
            etherquic::discover::VPAD_UINPUT
        } else {
            etherquic::discover::VPAD_NONE
        }
    }
    #[cfg(windows)]
    {
        if vigem_client::Client::connect().is_ok() {
            etherquic::discover::VPAD_VIGEM
        } else {
            etherquic::discover::VPAD_NONE
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        etherquic::discover::VPAD_NONE
    }
}

#[derive(Default)]
struct Cache {
    caps: Option<Capabilities>,
    /// Codestream per budget it was made for.
    thumbs: HashMap<usize, Vec<u8>>,
    /// Budgets asked for that are not encoded yet.
    wanted: BTreeSet<usize>,
}

/// Cached extras for [`etherquic::discover::serve_with`] and `serve_udp`.
pub struct HostExtras {
    cache: Mutex<Cache>,
}

impl HostExtras {
    pub fn empty() -> Self {
        Self {
            cache: Mutex::new(Cache::default()),
        }
    }

    pub fn set_capabilities(&self, caps: Capabilities) {
        self.lock().caps = Some(caps);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        match self.cache.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn wanted(&self) -> Vec<usize> {
        let cache = self.lock();
        cache
            .wanted
            .iter()
            .chain(cache.thumbs.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

impl OfferExtras for HostExtras {
    fn capabilities(&self) -> Option<Capabilities> {
        self.lock().caps.clone()
    }

    /// The picture made for this budget, else the largest that still fits.
    /// A new budget is queued for the next encode.
    fn thumbnail(&self, budget: usize) -> Option<Vec<u8>> {
        let mut cache = self.lock();
        if let Some(bytes) = cache.thumbs.get(&budget) {
            return Some(bytes.clone());
        }
        if cache.wanted.len() < 8 {
            cache.wanted.insert(budget);
        }
        cache
            .thumbs
            .values()
            .filter(|bytes| bytes.len() <= budget)
            .max_by_key(|bytes| bytes.len())
            .cloned()
    }
}

/// Whether to capture the desktop for offers. `ETHERQUIC_DISCOVERY_THUMBNAIL`
/// `0` turns it off and `1` on. Under Wayland it is off unless asked for,
/// since a screenshot there can go through the portal and ask the user.
pub fn thumbnails_wanted(setting: Option<&str>, wayland: bool) -> bool {
    match setting.map(str::trim) {
        Some("0") => false,
        Some("1") => true,
        _ => !wayland,
    }
}

/// Start the refresher. It stops when `stop` is set.
/// See [`thumbnails_wanted`] for when it captures the desktop.
#[cfg(feature = "desktop")]
pub fn host_extras(stop: Arc<AtomicBool>) -> Arc<HostExtras> {
    let extras = Arc::new(HostExtras::empty());
    extras.set_capabilities(gather_capabilities(Vec::new()));
    let worker = Arc::clone(&extras);
    let _ = std::thread::Builder::new()
        .name("etherquic-host-extras".into())
        .spawn(move || refresh_loop(&worker, &stop));
    extras
}

#[cfg(feature = "desktop")]
fn refresh_loop(extras: &HostExtras, stop: &AtomicBool) {
    // The first refresh then carries the real encoders.
    let _ = etherquic_media::caps::probe();
    let pictures = thumbnails_wanted(
        std::env::var("ETHERQUIC_DISCOVERY_THUMBNAIL").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
    );
    let cjxl = if pictures { cjxl_path() } else { None };
    if pictures && cjxl.is_none() {
        eprintln!("discovery: no cjxl found; offers carry no thumbnail");
    }
    let mut gilrs = gilrs::Gilrs::new().ok();
    let mut last: Option<Instant> = None;
    let mut frame: Option<(Vec<u8>, u32, u32)> = None;
    while !stop.load(Ordering::SeqCst) {
        let due = last.is_none_or(|at| at.elapsed() >= REFRESH);
        let pending = !extras.lock().wanted.is_empty();
        if !due && !(pending && frame.is_some()) {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        if due {
            last = Some(Instant::now());
            let pads = gilrs.as_mut().map(gamepads).unwrap_or_default();
            extras.set_capabilities(gather_capabilities(pads));
            if cjxl.is_some() {
                frame = capture_primary();
            }
        }
        let (Some(cjxl), Some((rgba, w, h))) = (&cjxl, &frame) else {
            extras.lock().wanted.clear();
            continue;
        };
        let budgets = if due {
            extras.wanted()
        } else {
            extras.lock().wanted.iter().copied().collect()
        };
        let mut made = HashMap::new();
        for budget in budgets {
            let mut scaled: HashMap<u32, Rgb> = HashMap::new();
            let picture = fit_to_budget(budget, &THUMB_WIDTHS, |width, distance| {
                let rgb = scaled
                    .entry(width)
                    .or_insert_with(|| downscale(rgba, *w, *h, width));
                encode_cjxl(cjxl, rgb, distance)
            });
            if let Some(bytes) = picture {
                made.insert(budget, bytes);
            }
        }
        let mut cache = extras.lock();
        cache.wanted.clear();
        if due {
            cache.thumbs = made;
        } else {
            cache.thumbs.extend(made);
        }
    }
}

#[cfg(feature = "desktop")]
fn capture_primary() -> Option<(Vec<u8>, u32, u32)> {
    let monitors = xcap::Monitor::all().ok()?;
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())?;
    let image = monitor.capture_image().ok()?;
    let (w, h) = (image.width(), image.height());
    Some((image.into_raw(), w, h))
}

/// Names of the controllers plugged into this computer, as the player sends
/// them. A slot pin in Settings matches one of these names.
#[cfg(feature = "desktop")]
pub fn local_pads() -> Vec<String> {
    let Ok(mut gilrs) = gilrs::Gilrs::new() else {
        return Vec::new();
    };
    while gilrs.next_event().is_some() {}
    let mut names: Vec<String> = gilrs
        .gamepads()
        .filter(|(_, pad)| pad.is_connected())
        .map(|(_, pad)| pad.name().to_string())
        .collect();
    names.dedup();
    names
}

#[cfg(feature = "desktop")]
fn gamepads(gilrs: &mut gilrs::Gilrs) -> Vec<PadInfo> {
    while gilrs.next_event().is_some() {}
    let api = if cfg!(windows) {
        etherquic::discover::PAD_API_WGI
    } else {
        PAD_API_EVDEV
    };
    gilrs
        .gamepads()
        .filter(|(_, pad)| pad.is_connected())
        .take(8)
        .map(|(_, pad)| PadInfo {
            api,
            kind: pad_kind(pad.name()),
            name: pad.name().chars().take(24).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_keeps_aspect_and_averages() {
        let mut rgba = Vec::new();
        for y in 0..4u32 {
            for x in 0..8u32 {
                let v = if x < 4 { 0 } else { 200 };
                rgba.extend([v, (y * 10) as u8, 50, 255]);
            }
        }
        let small = downscale(&rgba, 8, 4, 2);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.data.len(), 6);
        assert_eq!(small.data[0], 0);
        assert_eq!(small.data[3], 200);
        assert_eq!(small.data[1], 15);
        // Never upscales.
        assert_eq!(downscale(&rgba, 8, 4, 64).width, 8);
    }

    /// A stand-in encoder: size grows with width and falls with distance.
    fn fake(width: u32, distance: f32) -> Option<Vec<u8>> {
        let size = (width as f32 * 40.0 / distance.max(0.1)) as usize + 20;
        Some(vec![0xAB; size])
    }

    #[test]
    fn fit_never_exceeds_the_budget() {
        for budget in [0usize, 30, 64, 100, 300, 900, 1200, 1420, 5000] {
            if let Some(bytes) = fit_to_budget(budget, &THUMB_WIDTHS, fake) {
                assert!(bytes.len() <= budget, "{} > {budget}", bytes.len());
            }
        }
        assert!(fit_to_budget(10, &THUMB_WIDTHS, fake).is_none());
    }

    #[test]
    fn fit_prefers_width_then_lowers_distance() {
        let mut calls = Vec::new();
        let bytes = fit_to_budget(1000, &[192, 96], |w, d| {
            calls.push((w, d));
            fake(w, d)
        })
        .unwrap();
        // 192 px fits at the worst distance, so 96 px is never tried.
        assert!(calls.iter().all(|(w, _)| *w == 192));
        assert!(bytes.len() <= 1000);
        // The search moved well below the worst distance.
        assert!(bytes.len() > 500, "{}", bytes.len());
        // Too small for 192 px even at the worst distance: drops to 96 px.
        let mut widths = Vec::new();
        fit_to_budget(300, &[192, 96], |w, d| {
            widths.push(w);
            fake(w, d)
        })
        .unwrap();
        assert!(widths.contains(&96));
    }

    #[test]
    fn failing_encoder_gives_no_picture() {
        assert!(fit_to_budget(1400, &THUMB_WIDTHS, |_, _| None).is_none());
    }

    const PROC: &str = "\
I: Bus=0011 Vendor=0001 Product=0001 Version=ab41
N: Name=\"AT Translated Set 2 keyboard\"
H: Handlers=sysrq kbd event3 leds
B: PROP=0
B: EV=120013
B: KEY=402000000 3803078f800d001 feffffdfffefffff fffffffffffffffe

I: Bus=0019 Vendor=0000 Product=0001 Version=0000
N: Name=\"Power Button\"
H: Handlers=kbd event0
B: PROP=0
B: EV=3
B: KEY=10000000000000 0

I: Bus=0003 Vendor=046d Product=c52b Version=0111
N: Name=\"Logitech USB Receiver Mouse\"
H: Handlers=mouse0 event5
B: PROP=0
B: EV=17
B: KEY=ffff0000 0 0 0 0

I: Bus=0003 Vendor=045e Product=028e Version=0114
N: Name=\"Microsoft X-Box 360 pad\"
H: Handlers=event20 js0
B: PROP=0
B: EV=20000b
B: KEY=7cdb000000000000 0 0 0 0
B: ABS=3003f

I: Bus=0003 Vendor=046d Product=c24f Version=0111
N: Name=\"Logitech G29 Driving Force Racing Wheel\"
H: Handlers=event21 js1
B: EV=1b
B: KEY=1fff 0 0 0 0 0 0 ffff00000000 0 0 0 0
B: ABS=30027

I: Bus=0018 Vendor=04f3 Product=2c82 Version=0100
N: Name=\"ELAN Touchscreen\"
H: Handlers=event7
B: PROP=2
B: EV=b
B: KEY=400 0 0 0 0 0
B: ABS=3273800000000003

I: Bus=0018 Vendor=04f3 Product=2c82 Version=0100
N: Name=\"ELAN Stylus\"
H: Handlers=event8
B: PROP=2
B: EV=b
B: KEY=c03 1 0 0 0 0
B: ABS=d000003
";

    #[test]
    fn proc_input_finds_every_kind() {
        let (inputs, pads) = parse_proc_input(PROC);
        assert_ne!(inputs & INPUT_KEYBOARD, 0);
        assert_ne!(inputs & INPUT_MOUSE, 0);
        assert_ne!(inputs & INPUT_TOUCH, 0);
        assert_ne!(inputs & INPUT_PEN, 0);
        assert_eq!(pads.len(), 2);
        assert_eq!(pads[0].kind, PAD_KIND_GAMEPAD);
        assert_eq!(pads[0].api, PAD_API_EVDEV);
        assert_eq!(pads[1].kind, PAD_KIND_WHEEL);
        assert!(pads[1].name.chars().count() <= 24);
        // A power button alone is not a keyboard.
        let (only_power, _) = parse_proc_input(PROC.split("\n\n").nth(1).unwrap());
        assert_eq!(only_power & INPUT_KEYBOARD, 0);
    }

    #[test]
    fn cached_thumbnail_is_never_too_big() {
        let extras = HostExtras::empty();
        assert!(extras.thumbnail(1000).is_none());
        {
            let mut cache = extras.lock();
            cache.thumbs.insert(1000, vec![1; 990]);
            cache.thumbs.insert(600, vec![2; 580]);
        }
        assert_eq!(extras.thumbnail(1000).unwrap().len(), 990);
        // An unseen budget gets the largest that fits and is queued.
        assert_eq!(extras.thumbnail(800).unwrap().len(), 580);
        assert!(extras.lock().wanted.contains(&800));
        assert!(extras.thumbnail(100).is_none());
    }

    /// Runs only where `cjxl` is installed (the nix shell has it).
    #[test]
    fn cjxl_output_fits_its_budget() {
        let Some(cjxl) = cjxl_path() else {
            eprintln!("skipped: no cjxl");
            return;
        };
        let mut rgba = Vec::new();
        for y in 0..216u32 {
            for x in 0..384u32 {
                rgba.extend([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255]);
            }
        }
        for budget in [300usize, 800, 1300] {
            let mut scaled: HashMap<u32, Rgb> = HashMap::new();
            let bytes = fit_to_budget(budget, &THUMB_WIDTHS, |width, distance| {
                let rgb = scaled
                    .entry(width)
                    .or_insert_with(|| downscale(&rgba, 384, 216, width));
                encode_cjxl(&cjxl, rgb, distance)
            })
            .expect("a picture fits");
            assert!(bytes.len() <= budget);
            assert_eq!(&bytes[..2], &[0xff, 0x0a], "bare codestream");
        }
    }

    #[test]
    fn thumbnails_are_opt_in_under_wayland() {
        assert!(thumbnails_wanted(None, false));
        assert!(!thumbnails_wanted(None, true));
        assert!(thumbnails_wanted(Some("1"), true));
        assert!(!thumbnails_wanted(Some("0"), false));
    }
}
