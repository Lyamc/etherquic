//! Write a [`PadReport`] to a virtual gamepad on the host.
//!
//! Linux uses uinput. Windows uses ViGEmBus. The device is created on the
//! first report for that pad index, so a missing driver does not block the
//! keyboard and the mouse.

use std::collections::HashMap;

use crate::PadReport;
#[cfg(windows)]
use crate::{xinput_buttons, xinput_stick_y};

pub struct Pads {
    slots: HashMap<u8, Slot>,
    unavailable: bool,
}

struct Slot {
    device: Device,
    last: PadReport,
}

impl Pads {
    pub fn open() -> Self {
        Self {
            slots: HashMap::new(),
            unavailable: false,
        }
    }

    pub fn apply(&mut self, index: u8, report: PadReport) -> Result<(), String> {
        if self.unavailable {
            return Ok(());
        }
        if !self.slots.contains_key(&index) {
            let device = match Device::open(index) {
                Ok(device) => device,
                Err(error) => {
                    self.unavailable = true;
                    return Err(error);
                }
            };
            self.slots.insert(
                index,
                Slot {
                    device,
                    last: PadReport::default(),
                },
            );
        }
        let slot = self.slots.get_mut(&index).expect("slot");
        if slot.last == report {
            return Ok(());
        }
        slot.device.write(report)?;
        slot.last = report;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
struct Device {
    inner: evdev::uinput::VirtualDevice,
}

#[cfg(target_os = "linux")]
impl Device {
    fn open(index: u8) -> Result<Self, String> {
        use evdev::uinput::VirtualDevice;
        use evdev::{AbsInfo, AbsoluteAxisCode, AttributeSet, KeyCode, UinputAbsSetup};

        let mut keys = AttributeSet::<KeyCode>::new();
        for key in [
            KeyCode::BTN_SOUTH,
            KeyCode::BTN_EAST,
            KeyCode::BTN_NORTH,
            KeyCode::BTN_WEST,
            KeyCode::BTN_TL,
            KeyCode::BTN_TR,
            KeyCode::BTN_SELECT,
            KeyCode::BTN_START,
            KeyCode::BTN_MODE,
            KeyCode::BTN_THUMBL,
            KeyCode::BTN_THUMBR,
            KeyCode::BTN_DPAD_UP,
            KeyCode::BTN_DPAD_DOWN,
            KeyCode::BTN_DPAD_LEFT,
            KeyCode::BTN_DPAD_RIGHT,
        ] {
            keys.insert(key);
        }
        let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
        let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
        let name = format!("etherquic-play pad {index}");
        let device = VirtualDevice::builder()
            .map_err(|error| error.to_string())?
            .name(name.as_bytes())
            .with_keys(&keys)
            .map_err(|error| error.to_string())?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, stick))
            .map_err(|error| error.to_string())?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, stick))
            .map_err(|error| error.to_string())?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RX, stick))
            .map_err(|error| error.to_string())?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RY, stick))
            .map_err(|error| error.to_string())?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Z, trigger))
            .map_err(|error| error.to_string())?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RZ, trigger))
            .map_err(|error| error.to_string())?
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { inner: device })
    }

    fn write(&mut self, report: PadReport) -> Result<(), String> {
        // Full current state, so a reader that missed an edge still sees
        // buttons that are down. `emit` appends the sync report.
        let events = linux_state(report);
        self.inner.emit(&events).map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "linux")]
pub fn linux_state(report: PadReport) -> Vec<evdev::InputEvent> {
    use evdev::{AbsoluteAxisCode, EventType, InputEvent, KeyCode};

    let mut out = Vec::new();
    let mut key = |down: bool, code: KeyCode| {
        out.push(InputEvent::new(EventType::KEY.0, code.0, i32::from(down)));
    };
    key(report.south, KeyCode::BTN_SOUTH);
    key(report.east, KeyCode::BTN_EAST);
    key(report.west, KeyCode::BTN_WEST);
    key(report.north, KeyCode::BTN_NORTH);
    key(report.lb, KeyCode::BTN_TL);
    key(report.rb, KeyCode::BTN_TR);
    key(report.back, KeyCode::BTN_SELECT);
    key(report.start, KeyCode::BTN_START);
    key(report.guide, KeyCode::BTN_MODE);
    key(report.lstick, KeyCode::BTN_THUMBL);
    key(report.rstick, KeyCode::BTN_THUMBR);
    key(report.up, KeyCode::BTN_DPAD_UP);
    key(report.down, KeyCode::BTN_DPAD_DOWN);
    key(report.left, KeyCode::BTN_DPAD_LEFT);
    key(report.right, KeyCode::BTN_DPAD_RIGHT);
    let mut axis = |value: i32, code: AbsoluteAxisCode| {
        out.push(InputEvent::new(EventType::ABSOLUTE.0, code.0, value));
    };
    axis(i32::from(report.lx), AbsoluteAxisCode::ABS_X);
    axis(i32::from(report.ly), AbsoluteAxisCode::ABS_Y);
    axis(i32::from(report.rx), AbsoluteAxisCode::ABS_RX);
    axis(i32::from(report.ry), AbsoluteAxisCode::ABS_RY);
    axis(i32::from(report.lt), AbsoluteAxisCode::ABS_Z);
    axis(i32::from(report.rt), AbsoluteAxisCode::ABS_RZ);
    out.push(InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0));
    out
}

#[cfg(windows)]
struct Device {
    target: vigem_client::Xbox360Wired<vigem_client::Client>,
}

#[cfg(windows)]
impl Device {
    fn open(_index: u8) -> Result<Self, String> {
        let client = vigem_client::Client::connect().map_err(|error| error.to_string())?;
        let mut target =
            vigem_client::Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
        target.plugin().map_err(|error| error.to_string())?;
        target.wait_ready().map_err(|error| error.to_string())?;
        Ok(Self { target })
    }

    fn write(&mut self, report: PadReport) -> Result<(), String> {
        let gamepad = xgamepad(report);
        self.target
            .update(&gamepad)
            .map_err(|error| error.to_string())
    }
}

#[cfg(windows)]
pub fn xgamepad(report: PadReport) -> vigem_client::XGamepad {
    let mut gamepad = vigem_client::XGamepad::default();
    gamepad.buttons = vigem_client::XButtons(xinput_buttons(&report));
    gamepad.left_trigger = report.lt;
    gamepad.right_trigger = report.rt;
    gamepad.thumb_lx = report.lx;
    gamepad.thumb_ly = xinput_stick_y(report.ly);
    gamepad.thumb_rx = report.rx;
    gamepad.thumb_ry = xinput_stick_y(report.ry);
    gamepad
}

#[cfg(not(any(target_os = "linux", windows)))]
struct Device;

#[cfg(not(any(target_os = "linux", windows)))]
impl Device {
    fn open(_index: u8) -> Result<Self, String> {
        Err("gamepad injection is built for linux and windows".into())
    }

    fn write(&mut self, _report: PadReport) -> Result<(), String> {
        Err("gamepad injection is built for linux and windows".into())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux_state;
    use crate::PadReport;
    use evdev::{AbsoluteAxisCode, EventType, KeyCode};

    #[test]
    fn south_and_stick_down_reach_the_virtual_pad_events() {
        let report = PadReport {
            south: true,
            ly: 200,
            lt: 255,
            ..PadReport::default()
        };
        let events = linux_state(report);
        assert!(events.iter().any(|event| {
            event.event_type() == EventType::KEY
                && event.code() == KeyCode::BTN_SOUTH.0
                && event.value() == 1
        }));
        assert!(events.iter().any(|event| {
            event.event_type() == EventType::KEY
                && event.code() == KeyCode::BTN_EAST.0
                && event.value() == 0
        }));
        assert!(events.iter().any(|event| {
            event.event_type() == EventType::ABSOLUTE
                && event.code() == AbsoluteAxisCode::ABS_Y.0
                && event.value() == 200
        }));
        assert!(events.iter().any(|event| {
            event.event_type() == EventType::ABSOLUTE
                && event.code() == AbsoluteAxisCode::ABS_Z.0
                && event.value() == 255
        }));
        assert!(events.iter().any(|event| {
            event.event_type() == EventType::SYNCHRONIZATION && event.code() == 0
        }));
    }
}
