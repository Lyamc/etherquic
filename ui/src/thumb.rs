//! Player side of the discovery extras: the JPEG XL thumbnail, and the
//! monitor and input lines for the host picker.

use etherquic::discover::{
    Capabilities, PadInfo, INPUT_KEYBOARD, INPUT_MOUSE, INPUT_PEN, INPUT_TOUCH, OS_LINUX,
    OS_MACOS, OS_WINDOWS, PAD_API_DINPUT, PAD_API_EVDEV, PAD_API_WGI, PAD_API_XINPUT,
    PAD_KIND_STICK, PAD_KIND_WHEEL, VPAD_UINPUT, VPAD_VIGEM,
};

/// A decoded thumbnail, RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The whole codestream arrived. `false` is a progressive preview.
    pub complete: bool,
}

/// Largest side a discovery thumbnail may claim. Anything bigger is not ours.
const MAX_SIDE: u32 = 1024;

/// Decode a codestream that may be cut short. A progressive (responsive)
/// modular stream renders a coarser picture from a prefix. `None` when not
/// even the header and a first pass are there.
pub fn decode_thumbnail(bytes: &[u8]) -> Option<Thumb> {
    use jxl_oxide::{InitializeResult, JxlImage};
    let mut uninit = JxlImage::builder().build_uninit();
    uninit.feed_bytes(bytes).ok()?;
    let mut image = match uninit.try_init().ok()? {
        InitializeResult::Initialized(image) => image,
        InitializeResult::NeedMoreData(_) => return None,
    };
    if image.width() == 0 || image.height() == 0 || image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        return None;
    }
    let complete = image.is_loading_done();
    let render = if complete {
        image.render_frame(0).ok()?
    } else {
        image.render_loading_frame().ok()?
    };
    let mut stream = render.stream();
    let (width, height, channels) = (stream.width(), stream.height(), stream.channels() as usize);
    if width == 0 || height == 0 || channels == 0 {
        return None;
    }
    let mut samples = vec![0u8; width as usize * height as usize * channels];
    stream.write_to_buffer(&mut samples);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for px in samples.chunks_exact(channels) {
        match channels {
            1 => rgba.extend([px[0], px[0], px[0], 255]),
            2 => rgba.extend([px[0], px[0], px[0], px[1]]),
            3 => rgba.extend([px[0], px[1], px[2], 255]),
            _ => rgba.extend([px[0], px[1], px[2], px[3]]),
        }
    }
    Some(Thumb {
        width,
        height,
        rgba,
        complete,
    })
}

/// Short line for a picker row, such as `2 monitors · 🎮 2`.
pub fn caps_summary(caps: &Capabilities) -> String {
    let mut parts = Vec::new();
    match caps.monitors.len() {
        0 => {}
        1 => {
            let m = caps.monitors[0];
            parts.push(format!("{}×{}", m.width, m.height));
        }
        n => parts.push(format!("{n} monitors")),
    }
    if !caps.pads.is_empty() {
        parts.push(if caps.pads.len() == 1 {
            "🎮".to_string()
        } else {
            format!("🎮 {}", caps.pads.len())
        });
    }
    if caps.inputs & INPUT_TOUCH != 0 {
        parts.push("touch".into());
    }
    if caps.inputs & INPUT_PEN != 0 {
        parts.push("pen".into());
    }
    parts.join(" · ")
}

/// Full lines for the details section.
pub fn caps_details(caps: &Capabilities) -> Vec<String> {
    let mut lines = Vec::new();
    let os = match caps.os {
        OS_WINDOWS => "Windows",
        OS_LINUX => "Linux",
        OS_MACOS => "macOS",
        _ => "",
    };
    let label = if caps.os_label.is_empty() || caps.os_label == os {
        os.to_string()
    } else {
        caps.os_label.clone()
    };
    match (caps.hostname.is_empty(), label.is_empty()) {
        (false, false) => lines.push(format!("{} · {label}", caps.hostname)),
        (false, true) => lines.push(caps.hostname.clone()),
        (true, false) => lines.push(label),
        (true, true) => {}
    }
    for (index, m) in caps.monitors.iter().enumerate() {
        let hz = if m.refresh_centihz == 0 {
            String::new()
        } else {
            let hz = m.refresh_hz();
            if (hz - hz.round()).abs() < 0.05 {
                format!(" @ {:.0} Hz", hz)
            } else {
                format!(" @ {hz:.2} Hz")
            }
        };
        let primary = if m.primary { " · primary" } else { "" };
        lines.push(format!(
            "Monitor {}: {}×{}{hz} at {},{}{primary}",
            index + 1,
            m.width,
            m.height,
            m.x,
            m.y
        ));
    }
    let mut inputs = Vec::new();
    for (bit, name) in [
        (INPUT_KEYBOARD, "keyboard"),
        (INPUT_MOUSE, "mouse"),
        (INPUT_TOUCH, "touch"),
        (INPUT_PEN, "pen"),
    ] {
        if caps.inputs & bit != 0 {
            inputs.push(name);
        }
    }
    lines.push(if inputs.is_empty() {
        "Inputs: none reported".into()
    } else {
        format!("Inputs: {}", inputs.join(", "))
    });
    if caps.pads.is_empty() {
        lines.push("Controllers: none".into());
    } else {
        for pad in &caps.pads {
            lines.push(format!("Controller: {}", pad_line(pad)));
        }
    }
    lines.push(match caps.virtual_pad {
        VPAD_VIGEM => "Virtual gamepad: ViGEmBus".into(),
        VPAD_UINPUT => "Virtual gamepad: uinput".into(),
        _ => "Virtual gamepad: not available".into(),
    });
    if !caps.audio_outputs.is_empty() {
        lines.push(format!("Sound out: {}", caps.audio_outputs.join(", ")));
    }
    if !caps.audio_inputs.is_empty() {
        lines.push(format!("Sound in: {}", caps.audio_inputs.join(", ")));
    }
    lines
}

fn pad_line(pad: &PadInfo) -> String {
    let kind = match pad.kind {
        PAD_KIND_WHEEL => "wheel",
        PAD_KIND_STICK => "joystick",
        _ => "gamepad",
    };
    let api = match pad.api {
        PAD_API_XINPUT => "XInput",
        PAD_API_DINPUT => "DirectInput",
        PAD_API_WGI => "Windows.Gaming.Input",
        PAD_API_EVDEV => "evdev",
        _ => "",
    };
    let name = if pad.name.is_empty() { kind } else { pad.name.as_str() };
    if api.is_empty() {
        format!("{name} ({kind})")
    } else {
        format!("{name} ({kind}, {api})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic::discover::{MonitorInfo, PAD_KIND_GAMEPAD};

    /// 96×54, `cjxl --modular=1 --progressive --distance=2`.
    const FIXTURE: &[u8] = include_bytes!("testdata/thumb.jxl");

    #[test]
    fn decodes_a_whole_thumbnail() {
        let thumb = decode_thumbnail(FIXTURE).expect("decodes");
        assert_eq!((thumb.width, thumb.height), (96, 54));
        assert_eq!(thumb.rgba.len(), 96 * 54 * 4);
        assert!(thumb.complete);
        // The fixture is red on the left and blue on the right.
        let left = &thumb.rgba[(27 * 96 + 4) * 4..][..3];
        let right = &thumb.rgba[(27 * 96 + 91) * 4..][..3];
        assert!(left[0] > 150 && left[2] < 100, "{left:?}");
        assert!(right[2] > 150 && right[0] < 100, "{right:?}");
    }

    #[test]
    fn decodes_a_truncated_thumbnail() {
        let whole = decode_thumbnail(FIXTURE).unwrap();
        let mut decoded_some_prefix = false;
        for percent in [90usize, 75, 60, 45] {
            let cut = &FIXTURE[..FIXTURE.len() * percent / 100];
            if let Some(thumb) = decode_thumbnail(cut) {
                assert_eq!((thumb.width, thumb.height), (whole.width, whole.height));
                assert_eq!(thumb.rgba.len(), whole.rgba.len());
                assert!(!thumb.complete);
                decoded_some_prefix = true;
            }
        }
        assert!(decoded_some_prefix, "no prefix decoded");
        // Never a panic on junk or a header alone.
        assert!(decode_thumbnail(&FIXTURE[..8]).is_none());
        assert!(decode_thumbnail(&[]).is_none());
        assert!(decode_thumbnail(b"not a jxl at all").is_none());
    }

    fn caps() -> Capabilities {
        Capabilities {
            os: OS_WINDOWS,
            os_label: "Windows".into(),
            hostname: "AN515".into(),
            nic_mac: None,
            monitors: vec![
                MonitorInfo {
                    width: 2560,
                    height: 1440,
                    refresh_centihz: 14400,
                    x: 0,
                    y: 0,
                    primary: true,
                },
                MonitorInfo {
                    width: 1920,
                    height: 1080,
                    refresh_centihz: 5994,
                    x: 2560,
                    y: 0,
                    primary: false,
                },
            ],
            inputs: INPUT_KEYBOARD | INPUT_MOUSE,
            virtual_pad: VPAD_VIGEM,
            pads: vec![PadInfo {
                api: PAD_API_WGI,
                kind: PAD_KIND_GAMEPAD,
                name: "Xbox Controller".into(),
            }],
            audio_outputs: Vec::new(),
            audio_inputs: Vec::new(),
        }
    }

    #[test]
    fn summary_is_short() {
        assert_eq!(caps_summary(&caps()), "2 monitors · 🎮");
        let mut one = caps();
        one.monitors.truncate(1);
        one.pads.clear();
        one.inputs |= INPUT_TOUCH;
        assert_eq!(caps_summary(&one), "2560×1440 · touch");
        assert_eq!(caps_summary(&Capabilities::default()), "");
    }

    #[test]
    fn details_name_everything() {
        let lines = caps_details(&caps());
        assert_eq!(lines[0], "AN515 · Windows");
        assert_eq!(lines[1], "Monitor 1: 2560×1440 @ 144 Hz at 0,0 · primary");
        assert_eq!(lines[2], "Monitor 2: 1920×1080 @ 59.94 Hz at 2560,0");
        assert_eq!(lines[3], "Inputs: keyboard, mouse");
        assert_eq!(lines[4], "Controller: Xbox Controller (gamepad, Windows.Gaming.Input)");
        assert_eq!(lines[5], "Virtual gamepad: ViGEmBus");
    }
}
