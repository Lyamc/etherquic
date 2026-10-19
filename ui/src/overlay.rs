//! On-screen remote-play controls for the Android player.
//!
//! Left and right hold a stick, a d-pad, or a keyboard cluster. Any column
//! can take extra buttons (keys, pad face buttons, mouse). The layout is
//! stored in `settings.txt` as `overlay-left=` / `overlay-mid=` / `overlay-right=`.

use etherquic_input::{Axes, Body, Buttons, Event, KeySource};
use etherquic_play::{
    gamepad_event, set_pad_axis, set_pad_button, PadAxis, PadButton,
};

const STICK_RADIUS: f32 = 48.0;
const PAD_INDEX: u8 = 0;

/// Which column a control sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Middle,
    Right,
}

impl Side {
    pub const ALL: [Side; 3] = [Self::Left, Self::Middle, Self::Right];

    pub fn token(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Middle => "mid",
            Self::Right => "right",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Middle => "Mid",
            Self::Right => "Right",
        }
    }
}

/// Four-way cluster direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// One control the user can drop onto a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayItem {
    StickLeft,
    StickRight,
    Dpad,
    Arrows,
    Wasd,
    Pad(PadKind),
    Key(KeyKind),
    Mouse(MouseKind),
}

/// Face / shoulder / menu buttons on the virtual pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadKind {
    South,
    East,
    West,
    North,
    Lb,
    Rb,
    Lt,
    Rt,
    Back,
    Start,
}

/// Keyboard keys offered in the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Space,
    Enter,
    Esc,
    Shift,
    Ctrl,
    Tab,
    Letter(char),
}

/// Mouse buttons sent as EQIN pointer buttons with no motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Left,
    Middle,
    Right,
}

impl OverlayItem {
    pub fn token(self) -> String {
        match self {
            Self::StickLeft => "stick-l".into(),
            Self::StickRight => "stick-r".into(),
            Self::Dpad => "dpad".into(),
            Self::Arrows => "arrows".into(),
            Self::Wasd => "wasd".into(),
            Self::Pad(kind) => format!("pad-{}", kind.token()),
            Self::Key(KeyKind::Letter(ch)) => format!("key-{ch}"),
            Self::Key(kind) => format!("key-{}", kind.token()),
            Self::Mouse(kind) => format!("mouse-{}", kind.token()),
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "stick-l" => Self::StickLeft,
            "stick-r" => Self::StickRight,
            "dpad" => Self::Dpad,
            "arrows" => Self::Arrows,
            "wasd" => Self::Wasd,
            "pad-a" | "pad-south" => Self::Pad(PadKind::South),
            "pad-b" | "pad-east" => Self::Pad(PadKind::East),
            "pad-x" | "pad-west" => Self::Pad(PadKind::West),
            "pad-y" | "pad-north" => Self::Pad(PadKind::North),
            "pad-lb" => Self::Pad(PadKind::Lb),
            "pad-rb" => Self::Pad(PadKind::Rb),
            "pad-lt" => Self::Pad(PadKind::Lt),
            "pad-rt" => Self::Pad(PadKind::Rt),
            "pad-back" => Self::Pad(PadKind::Back),
            "pad-start" => Self::Pad(PadKind::Start),
            "key-space" => Self::Key(KeyKind::Space),
            "key-enter" => Self::Key(KeyKind::Enter),
            "key-esc" => Self::Key(KeyKind::Esc),
            "key-shift" => Self::Key(KeyKind::Shift),
            "key-ctrl" => Self::Key(KeyKind::Ctrl),
            "key-tab" => Self::Key(KeyKind::Tab),
            "mouse-l" => Self::Mouse(MouseKind::Left),
            "mouse-m" => Self::Mouse(MouseKind::Middle),
            "mouse-r" => Self::Mouse(MouseKind::Right),
            other => {
                let rest = other.strip_prefix("key-")?;
                let mut chars = rest.chars();
                let ch = chars.next()?;
                if chars.next().is_some() || !ch.is_ascii_alphanumeric() {
                    return None;
                }
                Self::Key(KeyKind::Letter(ch.to_ascii_lowercase()))
            }
        })
    }

    pub fn label(self) -> String {
        match self {
            Self::StickLeft => "L stick".into(),
            Self::StickRight => "R stick".into(),
            Self::Dpad => "D-pad".into(),
            Self::Arrows => "Arrows".into(),
            Self::Wasd => "WASD".into(),
            Self::Pad(kind) => kind.label().into(),
            Self::Key(KeyKind::Letter(ch)) => ch.to_ascii_uppercase().to_string(),
            Self::Key(kind) => kind.label().into(),
            Self::Mouse(kind) => kind.label().into(),
        }
    }

    pub fn is_stick(self) -> bool {
        matches!(self, Self::StickLeft | Self::StickRight)
    }

    pub fn is_cluster(self) -> bool {
        matches!(self, Self::Dpad | Self::Arrows | Self::Wasd)
    }
}

impl PadKind {
    fn token(self) -> &'static str {
        match self {
            Self::South => "a",
            Self::East => "b",
            Self::West => "x",
            Self::North => "y",
            Self::Lb => "lb",
            Self::Rb => "rb",
            Self::Lt => "lt",
            Self::Rt => "rt",
            Self::Back => "back",
            Self::Start => "start",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::South => "A",
            Self::East => "B",
            Self::West => "X",
            Self::North => "Y",
            Self::Lb => "LB",
            Self::Rb => "RB",
            Self::Lt => "LT",
            Self::Rt => "RT",
            Self::Back => "Select",
            Self::Start => "Start",
        }
    }

    fn pad_button(self) -> Option<PadButton> {
        Some(match self {
            Self::South => PadButton::South,
            Self::East => PadButton::East,
            Self::West => PadButton::West,
            Self::North => PadButton::North,
            Self::Lb => PadButton::Lb,
            Self::Rb => PadButton::Rb,
            Self::Back => PadButton::Back,
            Self::Start => PadButton::Start,
            Self::Lt | Self::Rt => return None,
        })
    }
}

impl KeyKind {
    fn token(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Enter => "enter",
            Self::Esc => "esc",
            Self::Shift => "shift",
            Self::Ctrl => "ctrl",
            Self::Tab => "tab",
            Self::Letter(_) => "letter",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Space => "Space",
            Self::Enter => "Enter",
            Self::Esc => "Esc",
            Self::Shift => "Shift",
            Self::Ctrl => "Ctrl",
            Self::Tab => "Tab",
            Self::Letter(_) => "Key",
        }
    }

    fn keysym(self) -> u32 {
        match self {
            Self::Space => u32::from(' '),
            Self::Enter => 0xff0d,
            Self::Esc => 0xff1b,
            Self::Shift => 0xffe1,
            Self::Ctrl => 0xffe3,
            Self::Tab => 0xff09,
            Self::Letter(ch) => u32::from(ch),
        }
    }
}

impl MouseKind {
    fn token(self) -> &'static str {
        match self {
            Self::Left => "l",
            Self::Middle => "m",
            Self::Right => "r",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Left => "Mouse L",
            Self::Middle => "Mouse M",
            Self::Right => "Mouse R",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Self::Left => Buttons::LEFT,
            Self::Middle => Buttons::MIDDLE,
            Self::Right => Buttons::RIGHT,
        }
    }
}

/// Saved overlay. Empty until the player adds controls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverlayLayout {
    pub visible: bool,
    pub left: Vec<OverlayItem>,
    pub middle: Vec<OverlayItem>,
    pub right: Vec<OverlayItem>,
}

impl OverlayLayout {
    pub fn column(&self, side: Side) -> &[OverlayItem] {
        match side {
            Side::Left => &self.left,
            Side::Middle => &self.middle,
            Side::Right => &self.right,
        }
    }

    pub fn column_mut(&mut self, side: Side) -> &mut Vec<OverlayItem> {
        match side {
            Side::Left => &mut self.left,
            Side::Middle => &mut self.middle,
            Side::Right => &mut self.right,
        }
    }

    pub fn add(&mut self, side: Side, item: OverlayItem) {
        self.column_mut(side).push(item);
        self.visible = true;
    }

    pub fn remove(&mut self, side: Side, index: usize) {
        let col = self.column_mut(side);
        if index < col.len() {
            col.remove(index);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty() && self.middle.is_empty() && self.right.is_empty()
    }
}

/// Live pad / mouse / stick while fingers are down.
#[derive(Clone, Debug, Default)]
pub struct OverlayRuntime {
    pub edit: bool,
    pub add_side: Side,
    pub pad_buttons: u32,
    pub axes: Axes,
    pub mouse: u8,
    pub stick: Option<StickTouch>,
}

impl Default for Side {
    fn default() -> Self {
        Self::Left
    }
}

/// Finger tracking one analog stick.
#[derive(Clone, Copy, Debug)]
pub struct StickTouch {
    pub side: Side,
    pub index: usize,
    pub origin_x: f32,
    pub origin_y: f32,
    pub nx: f32,
    pub ny: f32,
}

/// Picker rows: directional widgets, then pad, mouse, keys.
pub fn catalog() -> &'static [OverlayItem] {
    use OverlayItem::*;
    &[
        StickLeft,
        StickRight,
        Dpad,
        Arrows,
        Wasd,
        Pad(PadKind::South),
        Pad(PadKind::East),
        Pad(PadKind::West),
        Pad(PadKind::North),
        Pad(PadKind::Lb),
        Pad(PadKind::Rb),
        Pad(PadKind::Lt),
        Pad(PadKind::Rt),
        Pad(PadKind::Start),
        Pad(PadKind::Back),
        Mouse(MouseKind::Left),
        Mouse(MouseKind::Middle),
        Mouse(MouseKind::Right),
        Key(KeyKind::Space),
        Key(KeyKind::Enter),
        Key(KeyKind::Esc),
        Key(KeyKind::Shift),
        Key(KeyKind::Ctrl),
        Key(KeyKind::Tab),
        Key(KeyKind::Letter('e')),
        Key(KeyKind::Letter('f')),
        Key(KeyKind::Letter('r')),
        Key(KeyKind::Letter('q')),
        Key(KeyKind::Letter('c')),
    ]
}

/// Left stick, face buttons in the middle, d-pad on the right.
pub fn preset_game() -> OverlayLayout {
    OverlayLayout {
        visible: true,
        left: vec![OverlayItem::StickLeft],
        middle: vec![
            OverlayItem::Pad(PadKind::South),
            OverlayItem::Pad(PadKind::East),
            OverlayItem::Pad(PadKind::West),
            OverlayItem::Pad(PadKind::North),
            OverlayItem::Pad(PadKind::Start),
        ],
        right: vec![OverlayItem::Dpad],
    }
}

/// WASD on the left, arrow keys on the right.
pub fn preset_arrows() -> OverlayLayout {
    OverlayLayout {
        visible: true,
        left: vec![OverlayItem::Wasd],
        middle: vec![OverlayItem::Key(KeyKind::Space), OverlayItem::Mouse(MouseKind::Left)],
        right: vec![OverlayItem::Arrows],
    }
}

pub fn render_overlay(layout: &OverlayLayout) -> String {
    let mut out = format!("overlay={}\n", if layout.visible { "1" } else { "0" });
    for side in Side::ALL {
        for item in layout.column(side) {
            out.push_str(&format!("overlay-{}={}\n", side.token(), item.token()));
        }
    }
    out
}

pub fn parse_overlay_line(layout: &mut OverlayLayout, key: &str, value: &str) {
    match key {
        "overlay" => layout.visible = value != "0",
        "overlay-left" => {
            if let Some(item) = OverlayItem::from_token(value) {
                layout.left.push(item);
            }
        }
        "overlay-mid" => {
            if let Some(item) = OverlayItem::from_token(value) {
                layout.middle.push(item);
            }
        }
        "overlay-right" => {
            if let Some(item) = OverlayItem::from_token(value) {
                layout.right.push(item);
            }
        }
        _ => {}
    }
}

fn pad_snapshot(run: &OverlayRuntime) -> Event {
    gamepad_event(PAD_INDEX, run.pad_buttons, run.axes)
}

fn key_event(code: u32, down: bool) -> Event {
    Event {
        time_us: 0,
        body: Body::Key {
            down,
            source: KeySource::Keysym,
            code,
        },
    }
}

fn mouse_event(buttons: u8) -> Event {
    Event {
        time_us: 0,
        body: Body::Delta {
            dx: 0,
            dy: 0,
            buttons,
            wheel_y: 0,
            wheel_x: 0,
        },
    }
}

fn dir_pad(dir: Dir) -> PadButton {
    match dir {
        Dir::Up => PadButton::Up,
        Dir::Down => PadButton::Down,
        Dir::Left => PadButton::Left,
        Dir::Right => PadButton::Right,
    }
}

fn dir_keysym(item: OverlayItem, dir: Dir) -> Option<u32> {
    match item {
        OverlayItem::Arrows => Some(match dir {
            Dir::Up => 0xff52,
            Dir::Down => 0xff54,
            Dir::Left => 0xff51,
            Dir::Right => 0xff53,
        }),
        OverlayItem::Wasd => Some(u32::from(match dir {
            Dir::Up => 'w',
            Dir::Down => 's',
            Dir::Left => 'a',
            Dir::Right => 'd',
        })),
        _ => None,
    }
}

/// Hold or release one button / key / mouse control.
pub fn hold(item: OverlayItem, down: bool, run: &mut OverlayRuntime) -> Vec<Event> {
    match item {
        OverlayItem::Pad(kind) => {
            if let Some(button) = kind.pad_button() {
                run.pad_buttons = set_pad_button(run.pad_buttons, button, down);
            } else {
                let axis = match kind {
                    PadKind::Lt => PadAxis::LeftTrigger,
                    PadKind::Rt => PadAxis::RightTrigger,
                    _ => return Vec::new(),
                };
                run.axes = set_pad_axis(run.axes, axis, if down { 1.0 } else { 0.0 });
            }
            vec![pad_snapshot(run)]
        }
        OverlayItem::Key(kind) => vec![key_event(kind.keysym(), down)],
        OverlayItem::Mouse(kind) => {
            let bit = kind.bit();
            if down {
                run.mouse |= bit;
            } else {
                run.mouse &= !bit;
            }
            vec![mouse_event(run.mouse)]
        }
        OverlayItem::StickLeft | OverlayItem::StickRight | OverlayItem::Dpad | OverlayItem::Arrows
        | OverlayItem::Wasd => Vec::new(),
    }
}

/// Hold or release one arm of a d-pad / arrow / WASD cluster.
pub fn hold_dir(item: OverlayItem, dir: Dir, down: bool, run: &mut OverlayRuntime) -> Vec<Event> {
    match item {
        OverlayItem::Dpad => {
            run.pad_buttons = set_pad_button(run.pad_buttons, dir_pad(dir), down);
            vec![pad_snapshot(run)]
        }
        OverlayItem::Arrows | OverlayItem::Wasd => {
            let Some(code) = dir_keysym(item, dir) else {
                return Vec::new();
            };
            vec![key_event(code, down)]
        }
        _ => Vec::new(),
    }
}

/// Finger down on a stick: origin is the touch, axes start at rest.
pub fn stick_down(
    side: Side,
    index: usize,
    origin_x: f32,
    origin_y: f32,
    run: &mut OverlayRuntime,
) {
    run.stick = Some(StickTouch {
        side,
        index,
        origin_x,
        origin_y,
        nx: 0.0,
        ny: 0.0,
    });
}

/// Drag a stick. `nx`/`ny` are unit-circle, Y positive up.
pub fn stick_move(x: f32, y: f32, item: OverlayItem, run: &mut OverlayRuntime) -> Vec<Event> {
    let Some(touch) = run.stick.as_mut() else {
        return Vec::new();
    };
    let mut dx = (x - touch.origin_x) / STICK_RADIUS;
    let mut dy = (touch.origin_y - y) / STICK_RADIUS;
    let len = (dx * dx + dy * dy).sqrt();
    if len > 1.0 {
        dx /= len;
        dy /= len;
    }
    touch.nx = dx;
    touch.ny = dy;
    let (axis_x, axis_y) = match item {
        OverlayItem::StickLeft => (PadAxis::LeftX, PadAxis::LeftY),
        OverlayItem::StickRight => (PadAxis::RightX, PadAxis::RightY),
        _ => return Vec::new(),
    };
    run.axes = set_pad_axis(run.axes, axis_x, dx);
    run.axes = set_pad_axis(run.axes, axis_y, dy);
    vec![pad_snapshot(run)]
}

/// Finger up on a stick: axes return to rest.
pub fn stick_up(item: OverlayItem, run: &mut OverlayRuntime) -> Vec<Event> {
    run.stick = None;
    let (axis_x, axis_y) = match item {
        OverlayItem::StickLeft => (PadAxis::LeftX, PadAxis::LeftY),
        OverlayItem::StickRight => (PadAxis::RightX, PadAxis::RightY),
        _ => return Vec::new(),
    };
    run.axes = set_pad_axis(run.axes, axis_x, 0.0);
    run.axes = set_pad_axis(run.axes, axis_y, 0.0);
    vec![pad_snapshot(run)]
}

/// Release every held overlay control. Call this when the session ends.
pub fn release_all(run: &mut OverlayRuntime) -> Vec<Event> {
    let mut events = Vec::new();
    if run.pad_buttons != 0
        || run.axes != Axes::default()
        || run.stick.is_some()
    {
        run.pad_buttons = 0;
        run.axes = Axes::default();
        run.stick = None;
        events.push(pad_snapshot(run));
    }
    if run.mouse != 0 {
        run.mouse = 0;
        events.push(mouse_event(0));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_and_presets_fill_both_sides() {
        for item in catalog() {
            let token = item.token();
            assert_eq!(OverlayItem::from_token(&token), Some(*item), "{token}");
        }
        let game = preset_game();
        assert!(game.left[0].is_stick());
        assert!(game.right[0].is_cluster());
        assert!(!game.middle.is_empty());
        let arrows = preset_arrows();
        assert_eq!(arrows.left[0], OverlayItem::Wasd);
        assert_eq!(arrows.right[0], OverlayItem::Arrows);
        let mut layout = OverlayLayout::default();
        layout.add(Side::Left, OverlayItem::StickLeft);
        layout.add(Side::Right, OverlayItem::Arrows);
        layout.add(Side::Middle, OverlayItem::Key(KeyKind::Letter('e')));
        let text = render_overlay(&layout);
        let mut parsed = OverlayLayout::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            parse_overlay_line(&mut parsed, k, v);
        }
        assert_eq!(parsed, layout);
        parsed.remove(Side::Middle, 0);
        assert!(parsed.middle.is_empty());
    }

    #[test]
    fn analog_stick_writes_left_axes_and_rests_on_release() {
        let mut run = OverlayRuntime::default();
        stick_down(Side::Left, 0, 100.0, 100.0, &mut run);
        let events = stick_move(100.0 + STICK_RADIUS, 100.0, OverlayItem::StickLeft, &mut run);
        match &events[0].body {
            Body::Gamepad { index, axes, .. } => {
                assert_eq!(*index, 0);
                assert_eq!(axes.lx, i16::MAX);
                assert_eq!(axes.ly, 0);
            }
            other => panic!("{other:?}"),
        }
        let rest = stick_up(OverlayItem::StickLeft, &mut run);
        match &rest[0].body {
            Body::Gamepad { axes, .. } => {
                assert_eq!(axes.lx, 0);
                assert_eq!(axes.ly, 0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dpad_and_arrows_and_mouse_each_use_their_eqin_body() {
        let mut run = OverlayRuntime::default();
        let pad = hold_dir(OverlayItem::Dpad, Dir::Up, true, &mut run);
        match &pad[0].body {
            Body::Gamepad { buttons, .. } => {
                assert_ne!(buttons & etherquic_input::PAD_UP, 0);
            }
            other => panic!("{other:?}"),
        }
        let key = hold_dir(OverlayItem::Arrows, Dir::Left, true, &mut run);
        match &key[0].body {
            Body::Key { down, code, .. } => {
                assert!(down);
                assert_eq!(*code, 0xff51);
            }
            other => panic!("{other:?}"),
        }
        let wasd = hold_dir(OverlayItem::Wasd, Dir::Up, true, &mut run);
        match &wasd[0].body {
            Body::Key { code, .. } => assert_eq!(*code, u32::from('w')),
            other => panic!("{other:?}"),
        }
        let mouse = hold(OverlayItem::Mouse(MouseKind::Left), true, &mut run);
        match &mouse[0].body {
            Body::Delta { buttons, dx, dy, .. } => {
                assert_eq!(*buttons, Buttons::LEFT);
                assert_eq!(*dx, 0);
                assert_eq!(*dy, 0);
            }
            other => panic!("{other:?}"),
        }
        let face = hold(OverlayItem::Pad(PadKind::South), true, &mut run);
        match &face[0].body {
            Body::Gamepad { buttons, .. } => {
                assert_ne!(buttons & etherquic_input::PAD_SOUTH, 0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stick_is_clamped_to_the_unit_circle() {
        let mut run = OverlayRuntime::default();
        stick_down(Side::Right, 0, 0.0, 0.0, &mut run);
        stick_move(1000.0, 0.0, OverlayItem::StickRight, &mut run);
        let touch = run.stick.unwrap();
        let len = (touch.nx * touch.nx + touch.ny * touch.ny).sqrt();
        assert!((len - 1.0).abs() < 0.001);
        match run.axes.rx {
            n if n == i16::MAX => {}
            other => panic!("expected max right stick, got {other}"),
        }
    }
}
