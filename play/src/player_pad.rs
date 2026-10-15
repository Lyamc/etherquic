//! Read local gamepads and send them as EQIN on the play session.
//!
//! gilrs reports stick Y positive up and triggers as `0.0..=1.0`. The pad
//! helpers flip Y and scale triggers into the EQIN ranges.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use etherquic_input::Axes;

use crate::outbox::Gate;
use crate::{gamepad_event, set_dpad, set_pad_axis, set_pad_button, PadAxis, PadButton};
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};

struct Slot {
    buttons: u32,
    axes: Axes,
    dpad_x: f32,
    dpad_y: f32,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            buttons: 0,
            axes: Axes::default(),
            dpad_x: 0.0,
            dpad_y: 0.0,
        }
    }
}

/// Send this computer's controllers. Each is sent as the slot pinned to its
/// name in `view`, else the lowest free slot, so "controller 2" stays the
/// same pad whichever is plugged in first. Pins and the on/off switch are read
/// on every event, so a change in the console applies straight away.
pub fn run(input: Arc<Gate>, stop: Arc<AtomicBool>, view: crate::LiveView) {
    let mut gilrs = match Gilrs::new() {
        Ok(gilrs) => gilrs,
        Err(error) => {
            eprintln!("etherquic-play: gamepad: {error}");
            return;
        }
    };
    let mut slots: HashMap<GamepadId, Slot> = HashMap::new();
    let mut sent_as: HashMap<GamepadId, u8> = HashMap::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut idle = true;
        while let Some(gilrs::Event { id, event, .. }) = gilrs.next_event() {
            idle = false;
            let options = crate::snapshot(&view);
            let name = gilrs.gamepad(id).name().to_string();
            let index = slot_of(&mut sent_as, id, &name, &options.pad_slots, &input);
            if matches!(event, EventType::Disconnected) {
                slots.remove(&id);
                sent_as.remove(&id);
                input.push(gamepad_event(index, 0, Axes::default()));
                continue;
            }
            let slot = slots.entry(id).or_default();
            if apply(slot, &event) && options.send_pads {
                input.push(gamepad_event(index, slot.buttons, slot.axes));
            }
        }
        if idle {
            thread::sleep(Duration::from_millis(4));
        }
    }
}

/// The slot `id` is sent as. When its pin moved, the old slot is released first.
fn slot_of(
    sent_as: &mut HashMap<GamepadId, u8>,
    id: GamepadId,
    name: &str,
    pins: &[crate::PadSlot],
    input: &Gate,
) -> u8 {
    let pinned = pins.iter().find(|pin| pin.name == name).map(|pin| pin.slot);
    if let Some(&now) = sent_as.get(&id) {
        match pinned {
            Some(pin) if pin != now => {
                input.push(gamepad_event(now, 0, Axes::default()));
                sent_as.insert(id, pin);
                return pin;
            }
            _ => return now,
        }
    }
    let taken: Vec<u8> = sent_as.values().copied().collect();
    let slot = crate::slot_for(pins, name, &taken);
    sent_as.insert(id, slot);
    slot
}

fn apply(slot: &mut Slot, event: &EventType) -> bool {
    match *event {
        EventType::ButtonPressed(button, _) => on_button(slot, button, true, 1.0),
        EventType::ButtonReleased(button, _) => on_button(slot, button, false, 0.0),
        EventType::ButtonChanged(button, value, _) => on_button(slot, button, value > 0.5, value),
        EventType::AxisChanged(axis, value, _) => on_axis(slot, axis, value),
        _ => false,
    }
}

fn on_button(slot: &mut Slot, button: Button, down: bool, value: f32) -> bool {
    match button {
        Button::LeftTrigger2 => {
            slot.axes = set_pad_axis(slot.axes, PadAxis::LeftTrigger, value);
            true
        }
        Button::RightTrigger2 => {
            slot.axes = set_pad_axis(slot.axes, PadAxis::RightTrigger, value);
            true
        }
        Button::DPadUp => dpad_button(slot, 0.0, 1.0, down),
        Button::DPadDown => dpad_button(slot, 0.0, -1.0, down),
        Button::DPadRight => dpad_button(slot, 1.0, 0.0, down),
        Button::DPadLeft => dpad_button(slot, -1.0, 0.0, down),
        other => {
            let Some(pad) = face(other) else {
                return false;
            };
            let next = set_pad_button(slot.buttons, pad, down);
            if next == slot.buttons {
                return false;
            }
            slot.buttons = next;
            true
        }
    }
}

fn dpad_button(slot: &mut Slot, x: f32, y: f32, down: bool) -> bool {
    if x != 0.0 {
        slot.dpad_x = if down { x } else { 0.0 };
    }
    if y != 0.0 {
        slot.dpad_y = if down { y } else { 0.0 };
    }
    let next = set_dpad(slot.buttons, slot.dpad_x, slot.dpad_y);
    if next == slot.buttons {
        return false;
    }
    slot.buttons = next;
    true
}

fn on_axis(slot: &mut Slot, axis: Axis, value: f32) -> bool {
    match axis {
        Axis::LeftStickX => slot.axes = set_pad_axis(slot.axes, PadAxis::LeftX, value),
        Axis::LeftStickY => slot.axes = set_pad_axis(slot.axes, PadAxis::LeftY, value),
        Axis::RightStickX => slot.axes = set_pad_axis(slot.axes, PadAxis::RightX, value),
        Axis::RightStickY => slot.axes = set_pad_axis(slot.axes, PadAxis::RightY, value),
        Axis::LeftZ => slot.axes = set_pad_axis(slot.axes, PadAxis::LeftTrigger, value),
        Axis::RightZ => slot.axes = set_pad_axis(slot.axes, PadAxis::RightTrigger, value),
        Axis::DPadX => {
            slot.dpad_x = value;
            slot.buttons = set_dpad(slot.buttons, slot.dpad_x, slot.dpad_y);
        }
        Axis::DPadY => {
            slot.dpad_y = value;
            slot.buttons = set_dpad(slot.buttons, slot.dpad_x, slot.dpad_y);
        }
        Axis::Unknown => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic_input::PAD_SOUTH;

    #[test]
    fn south_and_stick_up_become_an_eqin_pad() {
        let mut slot = Slot::default();
        assert!(on_button(&mut slot, Button::South, true, 1.0));
        assert!(on_axis(&mut slot, Axis::LeftStickY, 1.0));
        assert!(on_button(&mut slot, Button::LeftTrigger2, true, 1.0));
        assert_eq!(slot.buttons & PAD_SOUTH, PAD_SOUTH);
        assert_eq!(slot.axes.ly, -32767);
        assert_eq!(slot.axes.lt, 32767);
        let event = gamepad_event(0, slot.buttons, slot.axes);
        match event.body {
            etherquic_input::Body::Gamepad {
                index,
                buttons,
                axes,
            } => {
                assert_eq!(index, 0);
                assert_eq!(buttons, slot.buttons);
                assert_eq!(axes.ly, -32767);
                assert_eq!(axes.lt, 32767);
            }
            other => panic!("expected a gamepad event, got {other:?}"),
        }
    }
}

fn face(button: Button) -> Option<PadButton> {
    Some(match button {
        Button::South => PadButton::South,
        Button::East => PadButton::East,
        Button::West => PadButton::West,
        Button::North => PadButton::North,
        Button::LeftTrigger => PadButton::Lb,
        Button::RightTrigger => PadButton::Rb,
        Button::Select => PadButton::Back,
        Button::Start => PadButton::Start,
        Button::Mode => PadButton::Guide,
        Button::LeftThumb => PadButton::LStick,
        Button::RightThumb => PadButton::RStick,
        _ => return None,
    })
}
