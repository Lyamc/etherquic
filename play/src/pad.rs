//! Gamepad reports shared by the player and the host.
//!
//! EQIN sticks use the full `i16` range, positive X to the right and positive
//! Y down. Triggers use `0..=32767`. The player API this maps from (gilrs)
//! uses positive Y up and triggers in `0.0..=1.0`, so [`set_pad_axis`] flips
//! Y and scales triggers into the EQIN range. [`pad_report`] is what the host
//! writes to a virtual controller.

use etherquic_input::{
    Axes, Event, PAD_BACK, PAD_DOWN, PAD_EAST, PAD_GUIDE, PAD_LB, PAD_LEFT, PAD_LSTICK, PAD_NORTH,
    PAD_RB, PAD_RIGHT, PAD_RSTICK, PAD_SOUTH, PAD_START, PAD_UP, PAD_WEST,
};

/// One button in player order. Matches the EQIN pad bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadButton {
    South,
    East,
    West,
    North,
    Lb,
    Rb,
    Back,
    Start,
    Guide,
    LStick,
    RStick,
    Up,
    Down,
    Left,
    Right,
}

/// One axis from the player device. Stick Y is positive up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}

/// Virtual-controller state. Stick Y stays EQIN-native: positive is down.
/// Triggers are `0..=255`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadReport {
    pub south: bool,
    pub east: bool,
    pub west: bool,
    pub north: bool,
    pub lb: bool,
    pub rb: bool,
    pub back: bool,
    pub start: bool,
    pub guide: bool,
    pub lstick: bool,
    pub rstick: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u8,
    pub rt: u8,
}

impl Default for PadReport {
    fn default() -> Self {
        Self {
            south: false,
            east: false,
            west: false,
            north: false,
            lb: false,
            rb: false,
            back: false,
            start: false,
            guide: false,
            lstick: false,
            rstick: false,
            up: false,
            down: false,
            left: false,
            right: false,
            lx: 0,
            ly: 0,
            rx: 0,
            ry: 0,
            lt: 0,
            rt: 0,
        }
    }
}

pub fn gamepad_event(index: u8, buttons: u32, axes: Axes) -> Event {
    Event {
        time_us: 0,
        body: etherquic_input::Body::Gamepad {
            index,
            buttons,
            axes,
        },
    }
}

pub fn set_pad_button(buttons: u32, button: PadButton, down: bool) -> u32 {
    let bit = pad_bit(button);
    if down {
        buttons | bit
    } else {
        buttons & !bit
    }
}

pub fn set_pad_axis(mut axes: Axes, axis: PadAxis, value: f32) -> Axes {
    match axis {
        PadAxis::LeftX => axes.lx = stick(value),
        PadAxis::LeftY => axes.ly = stick(-value),
        PadAxis::RightX => axes.rx = stick(value),
        PadAxis::RightY => axes.ry = stick(-value),
        PadAxis::LeftTrigger => axes.lt = trigger(value),
        PadAxis::RightTrigger => axes.rt = trigger(value),
    }
    axes
}

/// D-pad from a stick-style axis pair. Positive Y is up, matching the player API.
pub fn set_dpad(buttons: u32, x: f32, y: f32) -> u32 {
    let mut buttons = buttons & !(PAD_UP | PAD_DOWN | PAD_LEFT | PAD_RIGHT);
    if y > 0.5 {
        buttons |= PAD_UP;
    } else if y < -0.5 {
        buttons |= PAD_DOWN;
    }
    if x > 0.5 {
        buttons |= PAD_RIGHT;
    } else if x < -0.5 {
        buttons |= PAD_LEFT;
    }
    buttons
}

/// XInput button word. Y-up stick values are [`xinput_stick_y`].
pub fn xinput_buttons(report: &PadReport) -> u16 {
    let mut bits = 0u16;
    if report.up {
        bits |= 0x0001;
    }
    if report.down {
        bits |= 0x0002;
    }
    if report.left {
        bits |= 0x0004;
    }
    if report.right {
        bits |= 0x0008;
    }
    if report.start {
        bits |= 0x0010;
    }
    if report.back {
        bits |= 0x0020;
    }
    if report.lstick {
        bits |= 0x0040;
    }
    if report.rstick {
        bits |= 0x0080;
    }
    if report.lb {
        bits |= 0x0100;
    }
    if report.rb {
        bits |= 0x0200;
    }
    if report.guide {
        bits |= 0x0400;
    }
    if report.south {
        bits |= 0x1000;
    }
    if report.east {
        bits |= 0x2000;
    }
    if report.west {
        bits |= 0x4000;
    }
    if report.north {
        bits |= 0x8000;
    }
    bits
}

/// EQIN positive-down Y becomes XInput positive-up Y.
pub fn xinput_stick_y(ly_down: i16) -> i16 {
    ly_down.saturating_neg()
}

pub fn pad_report(buttons: u32, axes: Axes) -> PadReport {
    PadReport {
        south: buttons & PAD_SOUTH != 0,
        east: buttons & PAD_EAST != 0,
        west: buttons & PAD_WEST != 0,
        north: buttons & PAD_NORTH != 0,
        lb: buttons & PAD_LB != 0,
        rb: buttons & PAD_RB != 0,
        back: buttons & PAD_BACK != 0,
        start: buttons & PAD_START != 0,
        guide: buttons & PAD_GUIDE != 0,
        lstick: buttons & PAD_LSTICK != 0,
        rstick: buttons & PAD_RSTICK != 0,
        up: buttons & PAD_UP != 0,
        down: buttons & PAD_DOWN != 0,
        left: buttons & PAD_LEFT != 0,
        right: buttons & PAD_RIGHT != 0,
        lx: axes.lx,
        ly: axes.ly,
        rx: axes.rx,
        ry: axes.ry,
        lt: trigger_byte(axes.lt),
        rt: trigger_byte(axes.rt),
    }
}

fn pad_bit(button: PadButton) -> u32 {
    match button {
        PadButton::South => PAD_SOUTH,
        PadButton::East => PAD_EAST,
        PadButton::West => PAD_WEST,
        PadButton::North => PAD_NORTH,
        PadButton::Lb => PAD_LB,
        PadButton::Rb => PAD_RB,
        PadButton::Back => PAD_BACK,
        PadButton::Start => PAD_START,
        PadButton::Guide => PAD_GUIDE,
        PadButton::LStick => PAD_LSTICK,
        PadButton::RStick => PAD_RSTICK,
        PadButton::Up => PAD_UP,
        PadButton::Down => PAD_DOWN,
        PadButton::Left => PAD_LEFT,
        PadButton::Right => PAD_RIGHT,
    }
}

fn stick(value: f32) -> i16 {
    let value = value.clamp(-1.0, 1.0);
    (value * i16::MAX as f32) as i16
}

fn trigger(value: f32) -> i16 {
    let value = value.clamp(0.0, 1.0);
    (value * 32767.0) as i16
}

fn trigger_byte(value: i16) -> u8 {
    let value = value.clamp(0, 32767) as u32;
    (value * 255 / 32767) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic_input::{PAD_SOUTH, PAD_UP};

    #[test]
    fn player_axes_flip_y_and_scale_a_full_trigger() {
        let axes = set_pad_axis(Axes::default(), PadAxis::LeftY, 1.0);
        let axes = set_pad_axis(axes, PadAxis::LeftTrigger, 1.0);
        let axes = set_pad_axis(axes, PadAxis::RightX, -1.0);
        assert_eq!(axes.ly, -32767);
        assert_eq!(axes.lt, 32767);
        assert_eq!(axes.rx, -32767);
    }

    #[test]
    fn dpad_up_and_right_set_those_bits_only() {
        let buttons = set_dpad(PAD_SOUTH, 1.0, 1.0);
        assert_eq!(buttons & PAD_SOUTH, PAD_SOUTH);
        assert_eq!(buttons & PAD_UP, PAD_UP);
        assert_eq!(buttons & PAD_RIGHT, etherquic_input::PAD_RIGHT);
        assert_eq!(buttons & etherquic_input::PAD_DOWN, 0);
        assert_eq!(buttons & etherquic_input::PAD_LEFT, 0);
    }

    #[test]
    fn host_report_keeps_south_up_and_the_stick() {
        let buttons = set_pad_button(0, PadButton::South, true);
        let buttons = set_pad_button(buttons, PadButton::Up, true);
        let buttons = set_pad_button(buttons, PadButton::East, true);
        let buttons = set_pad_button(buttons, PadButton::East, false);
        let axes = Axes {
            lx: 100,
            ly: 50,
            rx: -10,
            ry: 0,
            lt: 16383,
            rt: 0,
        };
        let report = pad_report(buttons, axes);
        assert!(report.south);
        assert!(report.up);
        assert!(!report.east);
        assert_eq!(report.lx, 100);
        assert_eq!(report.ly, 50);
        assert_eq!(report.rx, -10);
        assert_eq!(report.lt, 127);
        assert_eq!(report.rt, 0);
        assert_eq!(xinput_buttons(&report), 0x1001);
        assert_eq!(xinput_stick_y(report.ly), -50);
        let event = gamepad_event(2, buttons, axes);
        match event.body {
            etherquic_input::Body::Gamepad {
                index,
                buttons: got,
                axes: got_axes,
            } => {
                assert_eq!(index, 2);
                assert_eq!(got, buttons);
                assert_eq!(got_axes, axes);
            }
            other => panic!("expected a gamepad event, got {other:?}"),
        }
    }
}
