//! Settings that are not codec preferences: how the picture is shown, what is
//! sent, which sound devices are used, and which backend runs each part.
//!
//! They live in `settings.txt` next to `preferences.txt`, one `key=value` per
//! line. A key this build does not know is kept out of the way, never an error.

use std::path::{Path, PathBuf};

use etherquic_play::{PadSlot, ScaleMode, SizeMatch, ViewOptions, PAD_SLOTS};

/// A heading in the Settings panel. Each one resets on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Appearance,
    Capture,
    Sound,
    Codecs,
    Input,
    Network,
    Language,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Self::Appearance,
        Self::Capture,
        Self::Sound,
        Self::Codecs,
        Self::Input,
        Self::Network,
        Self::Language,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Capture => "Display Capture",
            Self::Sound => "Sound",
            Self::Codecs => "Codecs",
            Self::Input => "Input",
            Self::Network => "Network",
            Self::Language => "Language",
        }
    }
}

/// Network driver choice. Membrane and Npcap exist on Windows only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkDriver {
    #[default]
    Automatic,
    Membrane,
    Npcap,
}

impl NetworkDriver {
    pub fn token(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Membrane => "membrane",
            Self::Npcap => "npcap",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Automatic, Self::Membrane, Self::Npcap]
            .into_iter()
            .find(|driver| driver.token() == token)
    }

    /// The value [`etherquic::set_l2_preference`] takes.
    pub fn l2_id(self) -> u8 {
        match self {
            Self::Automatic => 0,
            Self::Membrane => etherquic::discover::L2_MEMBRANE,
            Self::Npcap => etherquic::discover::L2_NPCAP,
        }
    }
}

/// One value in Settings. "Make default" and the "default" hint work per field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Scale,
    SizeMatch,
    SendKeyboard,
    SendMouse,
    SendPads,
    PadSlots,
    OutputDevice,
    ShareDevice,
    Network,
    Language,
}

impl Field {
    pub const ALL: [Field; 10] = [
        Self::Scale,
        Self::SizeMatch,
        Self::SendKeyboard,
        Self::SendMouse,
        Self::SendPads,
        Self::PadSlots,
        Self::OutputDevice,
        Self::ShareDevice,
        Self::Network,
        Self::Language,
    ];

    pub fn section(self) -> Section {
        match self {
            Self::Scale | Self::SizeMatch => Section::Appearance,
            Self::SendKeyboard | Self::SendMouse | Self::SendPads | Self::PadSlots => Section::Input,
            Self::OutputDevice | Self::ShareDevice => Section::Sound,
            Self::Network => Section::Network,
            Self::Language => Section::Language,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub scale: ScaleMode,
    pub size_match: SizeMatch,
    pub send_keyboard: bool,
    pub send_mouse: bool,
    pub send_pads: bool,
    pub pad_slots: Vec<PadSlot>,
    /// Where remote sound plays. `None` is the system default.
    pub output_device: Option<String>,
    /// What this computer shares when it hosts. `None` picks desktop audio.
    pub share_device: Option<String>,
    pub network: NetworkDriver,
    /// Only `en` exists.
    pub language: String,
}

impl Default for Settings {
    fn default() -> Self {
        let view = ViewOptions::default();
        Self {
            scale: view.scale,
            size_match: view.size_match,
            send_keyboard: view.send_keyboard,
            send_mouse: view.send_mouse,
            send_pads: view.send_pads,
            pad_slots: view.pad_slots,
            output_device: view.output_device,
            share_device: None,
            network: NetworkDriver::Automatic,
            language: "en".into(),
        }
    }
}

impl Settings {
    /// What the picture window reads.
    pub fn view(&self) -> ViewOptions {
        ViewOptions {
            scale: self.scale,
            size_match: self.size_match,
            send_keyboard: self.send_keyboard,
            send_mouse: self.send_mouse,
            send_pads: self.send_pads,
            pad_slots: self.pad_slots.clone(),
            output_device: self.output_device.clone(),
        }
    }

    /// Put one section back to `base`: the user's defaults, or
    /// `Settings::default()` for factory values. Codecs and capture rate live
    /// in the preferences file and are reset there.
    pub fn reset(&mut self, section: Section, base: &Settings) {
        for field in Field::ALL {
            if field.section() == section {
                self.take(base, field);
            }
        }
    }

    /// Whether `field` has the same value in both.
    pub fn same(&self, other: &Settings, field: Field) -> bool {
        match field {
            Field::Scale => self.scale == other.scale,
            Field::SizeMatch => self.size_match == other.size_match,
            Field::SendKeyboard => self.send_keyboard == other.send_keyboard,
            Field::SendMouse => self.send_mouse == other.send_mouse,
            Field::SendPads => self.send_pads == other.send_pads,
            Field::PadSlots => self.pad_slots == other.pad_slots,
            Field::OutputDevice => self.output_device == other.output_device,
            Field::ShareDevice => self.share_device == other.share_device,
            Field::Network => self.network == other.network,
            Field::Language => self.language == other.language,
        }
    }

    /// Copy one field from `from`.
    pub fn take(&mut self, from: &Settings, field: Field) {
        match field {
            Field::Scale => self.scale = from.scale,
            Field::SizeMatch => self.size_match = from.size_match,
            Field::SendKeyboard => self.send_keyboard = from.send_keyboard,
            Field::SendMouse => self.send_mouse = from.send_mouse,
            Field::SendPads => self.send_pads = from.send_pads,
            Field::PadSlots => self.pad_slots = from.pad_slots.clone(),
            Field::OutputDevice => self.output_device = from.output_device.clone(),
            Field::ShareDevice => self.share_device = from.share_device.clone(),
            Field::Network => self.network = from.network,
            Field::Language => self.language = from.language.clone(),
        }
    }

    /// Pin `name` to `slot`, or unpin it with `None`. One controller per slot.
    pub fn pin_pad(&mut self, name: &str, slot: Option<u8>) {
        self.pad_slots.retain(|pin| pin.name != name);
        if let Some(slot) = slot.filter(|slot| *slot < PAD_SLOTS) {
            self.pad_slots.retain(|pin| pin.slot != slot);
            self.pad_slots.push(PadSlot {
                name: name.to_string(),
                slot,
            });
            self.pad_slots.sort_by_key(|pin| pin.slot);
        }
    }

    pub fn pinned_slot(&self, name: &str) -> Option<u8> {
        self.pad_slots
            .iter()
            .find(|pin| pin.name == name)
            .map(|pin| pin.slot)
    }
}

pub fn settings_path() -> Option<PathBuf> {
    crate::prefs::preferences_path().map(|path| path.with_file_name("settings.txt"))
}

pub fn load_settings() -> Settings {
    settings_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_settings(&text))
        .unwrap_or_default()
}

pub fn store_settings(settings: &Settings) -> Result<(), String> {
    let path = settings_path().ok_or("no home directory for settings")?;
    store_settings_at(&path, settings)
}

/// The user's own defaults, set with "Make default". Same format as
/// `settings.txt`. Reset goes back to these; a missing file is factory values.
pub fn defaults_path() -> Option<PathBuf> {
    crate::prefs::preferences_path().map(|path| path.with_file_name("defaults.txt"))
}

pub fn load_defaults() -> Settings {
    defaults_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_settings(&text))
        .unwrap_or_default()
}

/// Factory values remove the file, so a later build's factory values apply.
pub fn store_defaults(defaults: &Settings) -> Result<(), String> {
    let path = defaults_path().ok_or("no home directory for settings")?;
    if *defaults == Settings::default() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("remove {}: {err}", path.display())),
        };
    }
    store_settings_at(&path, defaults)
}

fn store_settings_at(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, render_settings(settings))
        .map_err(|err| format!("write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("save {}: {err}", path.display()))
}

fn flag(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

/// Names go on one line each. A newline or tab in a device name is dropped.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

pub fn render_settings(settings: &Settings) -> String {
    let mut out = format!(
        "scale={}\nsize-match={}\nsend-keyboard={}\nsend-mouse={}\nsend-pads={}\noutput-device={}\nshare-device={}\nnetwork={}\nlanguage={}\n",
        settings.scale.token(),
        settings.size_match.token(),
        flag(settings.send_keyboard),
        flag(settings.send_mouse),
        flag(settings.send_pads),
        clean(settings.output_device.as_deref().unwrap_or("")),
        clean(settings.share_device.as_deref().unwrap_or("")),
        settings.network.token(),
        clean(&settings.language),
    );
    for pin in &settings.pad_slots {
        out.push_str(&format!("pad-slot={}={}\n", pin.slot, clean(&pin.name)));
    }
    out
}

pub fn parse_settings(text: &str) -> Settings {
    let mut settings = Settings::default();
    let mut pins = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim_end_matches('\r');
        let device = || (!value.is_empty()).then(|| value.to_string());
        match key.trim() {
            "scale" => settings.scale = ScaleMode::from_token(value).unwrap_or(settings.scale),
            "size-match" => {
                settings.size_match = SizeMatch::from_token(value)
                    .filter(|choice| choice.available())
                    .unwrap_or(settings.size_match)
            }
            "send-keyboard" => settings.send_keyboard = value != "0",
            "send-mouse" => settings.send_mouse = value != "0",
            "send-pads" => settings.send_pads = value != "0",
            "output-device" => settings.output_device = device(),
            "share-device" => settings.share_device = device(),
            "network" => {
                settings.network = NetworkDriver::from_token(value).unwrap_or(settings.network)
            }
            "language" if value == "en" => settings.language = value.to_string(),
            "pad-slot" => {
                if let Some((slot, name)) = value.split_once('=') {
                    if let Ok(slot) = slot.parse::<u8>() {
                        if !name.is_empty() {
                            pins.push((name.to_string(), slot));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (name, slot) in pins {
        settings.pin_pad(&name, Some(slot));
    }
    settings
}

/// Whether a backend can be picked here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendState {
    /// Runs now and there is nothing else to pick.
    Active,
    /// Can be picked in this build on this computer.
    Available,
    /// Shown greyed with the reason. Not pretending it works.
    Unavailable(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backend {
    pub label: &'static str,
    pub state: BackendState,
}

impl Backend {
    fn active(label: &'static str) -> Self {
        Self {
            label,
            state: BackendState::Active,
        }
    }
    fn available(label: &'static str) -> Self {
        Self {
            label,
            state: BackendState::Available,
        }
    }
    fn missing(label: &'static str, why: &'static str) -> Self {
        Self {
            label,
            state: BackendState::Unavailable(why),
        }
    }
}

/// The parts with a backend row. Media is the Opus encoder order and is
/// listed from the probe in the panel, so it is not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Network,
    Input,
    Capture,
    Render,
}

/// What this build can run for one part, for the OS it was built for.
/// `wayland` says the desktop session is Wayland.
pub fn backends(part: Part, wayland: bool) -> Vec<Backend> {
    let windows = cfg!(windows);
    if cfg!(target_os = "macos") {
        return match part {
            Part::Network => vec![
                Backend::active("libpcap (BPF)"),
                Backend::missing("Membrane", "Windows only"),
                Backend::missing("Npcap", "Windows only"),
            ],
            Part::Input => vec![
                Backend::active("IOKit"),
                Backend::missing("XInput", "Windows only"),
                Backend::missing("DirectInput", "Windows only"),
            ],
            Part::Capture => vec![
                Backend::active("CoreGraphics"),
                Backend::missing("ScreenCaptureKit", "not available"),
            ],
            Part::Render => vec![
                Backend::active("CoreGraphics (softbuffer)"),
                Backend::missing("Metal", "not available"),
                Backend::missing("SDL", "not available"),
            ],
        };
    }
    match part {
        Part::Network if windows => vec![
            Backend::available("Automatic"),
            Backend::available("Membrane"),
            Backend::available("Npcap"),
        ],
        Part::Network => vec![
            Backend::active("libpcap"),
            Backend::missing("Membrane", "Windows only"),
            Backend::missing("Npcap", "Windows only"),
        ],
        Part::Input if windows => vec![
            Backend::active("Windows.Gaming.Input"),
            Backend::missing("XInput", "not in this build"),
            Backend::missing("DirectInput", "not in this build"),
        ],
        Part::Input => vec![
            Backend::active("evdev"),
            Backend::missing("XInput", "Windows only"),
            Backend::missing("DirectInput", "Windows only"),
        ],
        Part::Capture if windows => vec![
            Backend::active("GDI"),
            Backend::missing("Desktop Duplication", "not available"),
            Backend::missing("DirectShow", "not available"),
        ],
        Part::Capture => vec![
            if wayland {
                Backend::active("Wayland")
            } else {
                Backend::active("X11")
            },
            Backend::missing("video4linux", "not available"),
        ],
        Part::Render if windows => vec![
            Backend::active("GDI (softbuffer)"),
            Backend::missing("DX12", "not available"),
            Backend::missing("Vulkan", "not available"),
            Backend::missing("SDL", "not available"),
        ],
        Part::Render => vec![
            if wayland {
                Backend::active("Wayland")
            } else {
                Backend::active("X11")
            },
            Backend::missing("Vulkan", "not available"),
            Backend::missing("SDL", "not available"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let mut settings = Settings {
            scale: ScaleMode::Fill,
            size_match: SizeMatch::WindowToHost,
            send_keyboard: false,
            send_mouse: true,
            send_pads: false,
            output_device: Some("Speakers (USB)".into()),
            share_device: Some("Line In".into()),
            network: NetworkDriver::Npcap,
            ..Settings::default()
        };
        settings.pin_pad("Xbox Wireless Controller", Some(1));
        settings.pin_pad("DualSense", Some(0));
        let parsed = parse_settings(&render_settings(&settings));
        assert_eq!(parsed, settings);
        assert_eq!(parsed.pad_slots[0].name, "DualSense");
    }

    #[test]
    fn unknown_or_bad_lines_keep_defaults() {
        let parsed = parse_settings(
            "scale=zoom\nsize-match=host-to-window\nfuture=1\nnetwork=wifi\nlanguage=fr\npad-slot=x\npad-slot=9=Pad\n",
        );
        assert_eq!(parsed, Settings::default());
    }

    #[test]
    fn one_controller_per_slot() {
        let mut settings = Settings::default();
        settings.pin_pad("A", Some(0));
        settings.pin_pad("B", Some(0));
        assert_eq!(settings.pinned_slot("A"), None);
        assert_eq!(settings.pinned_slot("B"), Some(0));
        settings.pin_pad("B", Some(2));
        settings.pin_pad("A", Some(0));
        assert_eq!(settings.pad_slots.len(), 2);
        settings.pin_pad("B", None);
        assert_eq!(settings.pinned_slot("B"), None);
    }

    #[test]
    fn reset_touches_one_section() {
        let mut settings = Settings {
            scale: ScaleMode::Native,
            send_mouse: false,
            output_device: Some("Headset".into()),
            ..Settings::default()
        };
        let factory = Settings::default();
        settings.reset(Section::Appearance, &factory);
        assert_eq!(settings.scale, ScaleMode::Fit);
        assert!(!settings.send_mouse);
        settings.reset(Section::Sound, &factory);
        assert_eq!(settings.output_device, None);
        settings.reset(Section::Input, &factory);
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn make_default_then_reset_returns_to_it() {
        let mut defaults = Settings::default();
        let mut settings = Settings {
            scale: ScaleMode::Native,
            send_mouse: false,
            output_device: Some("Headset".into()),
            ..Settings::default()
        };
        // "Make default" on the scale only.
        assert!(!settings.same(&defaults, Field::Scale));
        defaults.take(&settings, Field::Scale);
        assert!(settings.same(&defaults, Field::Scale));
        assert!(!settings.same(&defaults, Field::SendMouse));
        settings.scale = ScaleMode::Stretch;
        settings.reset(Section::Appearance, &defaults);
        assert_eq!(settings.scale, ScaleMode::Native);
        // Fields that were not made default still go back to factory values.
        settings.reset(Section::Input, &defaults);
        assert!(settings.send_mouse);
        assert_eq!(settings.output_device.as_deref(), Some("Headset"));
        // The defaults file uses the settings format.
        assert_eq!(parse_settings(&render_settings(&defaults)), defaults);
    }

    #[test]
    fn every_field_belongs_to_a_resettable_section() {
        let changed = Settings {
            scale: ScaleMode::Native,
            size_match: SizeMatch::WindowToHost,
            send_keyboard: false,
            send_mouse: false,
            send_pads: false,
            pad_slots: vec![PadSlot { name: "Pad".into(), slot: 1 }],
            output_device: Some("A".into()),
            share_device: Some("B".into()),
            network: NetworkDriver::Npcap,
            language: "en-x".into(),
        };
        let factory = Settings::default();
        for field in Field::ALL {
            assert!(!changed.same(&factory, field), "{field:?} unchanged in the test");
        }
        let mut settings = changed.clone();
        for section in Section::ALL {
            settings.reset(section, &factory);
        }
        assert_eq!(settings, factory);
    }

    #[test]
    fn store_and_load_from_a_file() {
        let dir = std::env::temp_dir().join(format!("eq-settings-{}", std::process::id()));
        let path = dir.join("etherquic").join("settings.txt");
        let settings = Settings {
            scale: ScaleMode::Stretch,
            ..Settings::default()
        };
        store_settings_at(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse_settings(&text), settings);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_part_names_what_runs() {
        for part in [Part::Network, Part::Input, Part::Capture, Part::Render] {
            for wayland in [false, true] {
                let list = backends(part, wayland);
                assert!(list.iter().any(|backend| matches!(
                    backend.state,
                    BackendState::Active | BackendState::Available
                )));
            }
        }
        // Nothing greyed claims to run.
        let render = backends(Part::Render, true);
        assert!(render
            .iter()
            .filter(|backend| backend.label == "SDL")
            .all(|backend| matches!(backend.state, BackendState::Unavailable(_))));
    }
}
