//! Termux-style extra keys for the Android player.
//!
//! The bar sits at the bottom of Desk, which is the top of the IME after
//! `adjustResize`. CTRL, ALT, and SHIFT are sticky: one tap arms the next
//! key, a second tap locks until a third tap clears it.

/// One extra-key. `Keyboard` toggles the IME and sends nothing to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtraKey {
    Esc,
    Tab,
    Ctrl,
    Alt,
    Shift,
    Minus,
    Slash,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Up,
    Right,
    Down,
    Keyboard,
}

/// Sticky modifier: off, one chord, or locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Sticky {
    #[default]
    Off,
    Once,
    Locked,
}

impl Sticky {
    fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Once,
            Self::Once => Self::Locked,
            Self::Locked => Self::Off,
        }
    }

    fn armed(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// True when the next key should include this modifier. `Once` clears.
    fn take_for_key(&mut self) -> bool {
        match *self {
            Self::Off => false,
            Self::Once => {
                *self = Self::Off;
                true
            }
            Self::Locked => true,
        }
    }
}

/// CTRL / ALT / SHIFT held from the extra-keys bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtraMods {
    pub ctrl: Sticky,
    pub alt: Sticky,
    pub shift: Sticky,
}

impl ExtraMods {
    pub fn lit(&self, key: ExtraKey) -> bool {
        match key {
            ExtraKey::Ctrl => self.ctrl.armed(),
            ExtraKey::Alt => self.alt.armed(),
            ExtraKey::Shift => self.shift.armed(),
            _ => false,
        }
    }
}

/// What a tap does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tap {
    ToggleIme,
    Keys(Vec<(bool, u32)>),
}

impl ExtraKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Esc => "ESC",
            Self::Tab => "TAB",
            Self::Ctrl => "CTRL",
            Self::Alt => "ALT",
            Self::Shift => "SHIFT",
            Self::Minus => "-",
            Self::Slash => "/",
            Self::Home => "HOME",
            Self::End => "END",
            Self::PageUp => "PGUP",
            Self::PageDown => "PGDN",
            Self::Left => "←",
            Self::Up => "↑",
            Self::Right => "→",
            Self::Down => "↓",
            Self::Keyboard => "KEYBOARD",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Esc => "ek-esc",
            Self::Tab => "ek-tab",
            Self::Ctrl => "ek-ctrl",
            Self::Alt => "ek-alt",
            Self::Shift => "ek-shift",
            Self::Minus => "ek-minus",
            Self::Slash => "ek-slash",
            Self::Home => "ek-home",
            Self::End => "ek-end",
            Self::PageUp => "ek-pgup",
            Self::PageDown => "ek-pgdn",
            Self::Left => "ek-left",
            Self::Up => "ek-up",
            Self::Right => "ek-right",
            Self::Down => "ek-down",
            Self::Keyboard => "ek-keyboard",
        }
    }

    fn keysym(self) -> Option<u32> {
        Some(match self {
            Self::Esc => 0xff1b,
            Self::Tab => 0xff09,
            Self::Minus => u32::from('-'),
            Self::Slash => u32::from('/'),
            Self::Home => 0xff50,
            Self::End => 0xff57,
            Self::PageUp => 0xff55,
            Self::PageDown => 0xff56,
            Self::Left => 0xff51,
            Self::Up => 0xff52,
            Self::Right => 0xff53,
            Self::Down => 0xff54,
            Self::Ctrl | Self::Alt | Self::Shift | Self::Keyboard => return None,
        })
    }
}

/// Termux default two rows, plus a keyboard toggle on its own row.
pub fn default_rows() -> &'static [&'static [ExtraKey]] {
    &[
        &[
            ExtraKey::Esc,
            ExtraKey::Slash,
            ExtraKey::Minus,
            ExtraKey::Home,
            ExtraKey::Up,
            ExtraKey::End,
            ExtraKey::PageUp,
        ],
        &[
            ExtraKey::Tab,
            ExtraKey::Ctrl,
            ExtraKey::Alt,
            ExtraKey::Left,
            ExtraKey::Down,
            ExtraKey::Right,
            ExtraKey::PageDown,
        ],
        &[ExtraKey::Keyboard],
    ]
}

/// Apply one extra-key tap. Sticky modifiers send no host events until a
/// key that has a keysym is tapped.
pub fn tap(key: ExtraKey, mods: &mut ExtraMods) -> Tap {
    match key {
        ExtraKey::Keyboard => Tap::ToggleIme,
        ExtraKey::Ctrl => {
            mods.ctrl = mods.ctrl.cycle();
            Tap::Keys(Vec::new())
        }
        ExtraKey::Alt => {
            mods.alt = mods.alt.cycle();
            Tap::Keys(Vec::new())
        }
        ExtraKey::Shift => {
            mods.shift = mods.shift.cycle();
            Tap::Keys(Vec::new())
        }
        other => match other.keysym() {
            Some(code) => Tap::Keys(chord(
                code,
                mods.ctrl.take_for_key(),
                mods.alt.take_for_key(),
                mods.shift.take_for_key(),
            )),
            None => Tap::Keys(Vec::new()),
        },
    }
}

/// Down/up for `code`, wrapped in any modifiers that are on.
pub fn chord(code: u32, ctrl: bool, alt: bool, shift: bool) -> Vec<(bool, u32)> {
    let mut out = Vec::with_capacity(2 + 2 * (ctrl as usize + alt as usize + shift as usize));
    if ctrl {
        out.push((true, 0xffe3));
    }
    if alt {
        out.push((true, 0xffe9));
    }
    if shift {
        out.push((true, 0xffe1));
    }
    out.push((true, code));
    out.push((false, code));
    if shift {
        out.push((false, 0xffe1));
    }
    if alt {
        out.push((false, 0xffe9));
    }
    if ctrl {
        out.push((false, 0xffe3));
    }
    out
}

/// X11 keysym for a GPUI key name (`escape`, `enter`, a single character).
pub fn keysym_for_named(key: &str) -> Option<u32> {
    match key {
        "escape" => Some(0xff1b),
        "enter" => Some(0xff0d),
        "tab" => Some(0xff09),
        "backspace" => Some(0xff08),
        "delete" => Some(0xffff),
        "up" => Some(0xff52),
        "down" => Some(0xff54),
        "left" => Some(0xff51),
        "right" => Some(0xff53),
        "home" => Some(0xff50),
        "end" => Some(0xff57),
        "pageup" => Some(0xff55),
        "pagedown" => Some(0xff56),
        "space" => Some(u32::from(' ')),
        other => {
            let mut chars = other.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || ch.is_control() {
                return None;
            }
            Some(u32::from(ch))
        }
    }
}

/// IME / hardware key with extra-key sticky modifiers mixed in.
pub fn type_named(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    mods: &mut ExtraMods,
) -> Vec<(bool, u32)> {
    let Some(code) = keysym_for_named(key) else {
        return Vec::new();
    };
    chord(
        code,
        ctrl || mods.ctrl.take_for_key(),
        alt || mods.alt.take_for_key(),
        shift || mods.shift.take_for_key(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn downs(keys: &[(bool, u32)]) -> Vec<u32> {
        keys.iter()
            .filter_map(|(down, code)| down.then_some(*code))
            .collect()
    }

    #[test]
    fn termux_rows_cover_esc_ctrl_and_arrows() {
        let flat: Vec<ExtraKey> = default_rows().iter().flat_map(|row| row.iter().copied()).collect();
        assert!(flat.contains(&ExtraKey::Esc));
        assert!(flat.contains(&ExtraKey::Ctrl));
        assert!(flat.contains(&ExtraKey::Alt));
        assert!(flat.contains(&ExtraKey::Left));
        assert!(flat.contains(&ExtraKey::Keyboard));
        assert_eq!(default_rows()[0].len(), 7);
        assert_eq!(default_rows()[1].len(), 7);
    }

    #[test]
    fn esc_is_down_then_up() {
        let mut mods = ExtraMods::default();
        match tap(ExtraKey::Esc, &mut mods) {
            Tap::Keys(keys) => assert_eq!(keys, vec![(true, 0xff1b), (false, 0xff1b)]),
            other => panic!("{other:?}"),
        }
        assert_eq!(tap(ExtraKey::Keyboard, &mut mods), Tap::ToggleIme);
    }

    #[test]
    fn ctrl_is_sticky_then_consumed_by_the_next_key() {
        let mut mods = ExtraMods::default();
        assert_eq!(tap(ExtraKey::Ctrl, &mut mods), Tap::Keys(Vec::new()));
        assert_eq!(mods.ctrl, Sticky::Once);
        match tap(ExtraKey::Esc, &mut mods) {
            Tap::Keys(keys) => {
                assert_eq!(
                    downs(&keys),
                    vec![0xffe3, 0xff1b]
                );
                assert_eq!(keys.last(), Some(&(false, 0xffe3)));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(mods.ctrl, Sticky::Off);
    }

    #[test]
    fn a_second_ctrl_tap_locks_until_a_third() {
        let mut mods = ExtraMods::default();
        tap(ExtraKey::Ctrl, &mut mods);
        tap(ExtraKey::Ctrl, &mut mods);
        assert_eq!(mods.ctrl, Sticky::Locked);
        let _ = tap(ExtraKey::Tab, &mut mods);
        assert_eq!(mods.ctrl, Sticky::Locked);
        tap(ExtraKey::Ctrl, &mut mods);
        assert_eq!(mods.ctrl, Sticky::Off);
    }

    #[test]
    fn ime_letter_picks_up_a_sticky_alt() {
        let mut mods = ExtraMods::default();
        tap(ExtraKey::Alt, &mut mods);
        let keys = type_named("c", false, false, false, &mut mods);
        assert_eq!(downs(&keys), vec![0xffe9, u32::from('c')]);
        assert_eq!(mods.alt, Sticky::Off);
        assert_eq!(keysym_for_named("enter"), Some(0xff0d));
        assert_eq!(keysym_for_named("pageup"), Some(0xff55));
        let mut shift = ExtraMods::default();
        assert_eq!(tap(ExtraKey::Shift, &mut shift), Tap::Keys(Vec::new()));
        match tap(ExtraKey::Minus, &mut shift) {
            Tap::Keys(keys) => assert_eq!(downs(&keys), vec![0xffe1, u32::from('-')]),
            other => panic!("{other:?}"),
        }
    }
}
