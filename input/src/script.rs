//! One event per line.
//!
//! ```text
//! # comment
//! t=1000 key down hid 0x00070004
//! key up set1 0x11D
//! pointer 32768 32768 left+right wheel=1 hwheel=-1
//! delta -10 4 right
//! pad 0 south+east 0 0 0 0 32767 0
//! sync caps num
//! unknown 99 0102
//! ```
//!
//! `t=` is microseconds. A line without it has time 0; nothing in this crate
//! stamps the clock. `wheel` positive is away from the user.

use crate::error::Error;
use crate::event::{
    Axes, Body, Buttons, Event, KeySource, PAD_BACK, PAD_DOWN, PAD_EAST, PAD_GUIDE, PAD_KNOWN,
    PAD_LB, PAD_LEFT, PAD_LSTICK, PAD_NORTH, PAD_RB, PAD_RIGHT, PAD_RSTICK, PAD_SOUTH, PAD_START,
    PAD_UP, PAD_WEST, SYNC_CAPS, SYNC_KANA, SYNC_KNOWN, SYNC_NUM, SYNC_SCROLL,
};

pub fn parse_script(text: &str) -> Result<Vec<Event>, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut events = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(event) => events.push(event),
            Err(message) => {
                return Err(Error::Script {
                    line: line_no,
                    message,
                });
            }
        }
    }
    Ok(events)
}

pub fn format_script(events: &[Event]) -> String {
    let mut out = String::new();
    for event in events {
        out.push_str(&format_event(event));
        out.push('\n');
    }
    out
}

pub fn format_event(event: &Event) -> String {
    let body = match &event.body {
        Body::Key { down, source, code } => {
            let state = if *down { "down" } else { "up" };
            let source = match source {
                KeySource::Hid => "hid",
                KeySource::Set1 => "set1",
                KeySource::Keysym => "keysym",
            };
            format!("key {state} {source} {code}")
        }
        Body::Pointer {
            x,
            y,
            buttons,
            wheel_y,
            wheel_x,
        } => format!(
            "pointer {x} {y} {}{}{}",
            format_mouse(*buttons),
            axis_suffix("wheel", *wheel_y),
            axis_suffix("hwheel", *wheel_x),
        ),
        Body::Delta {
            dx,
            dy,
            buttons,
            wheel_y,
            wheel_x,
        } => format!(
            "delta {dx} {dy} {}{}{}",
            format_mouse(*buttons),
            axis_suffix("wheel", *wheel_y),
            axis_suffix("hwheel", *wheel_x),
        ),
        Body::Gamepad {
            index,
            buttons,
            axes,
        } => format!(
            "pad {index} {} {} {} {} {} {} {}",
            format_pad(*buttons),
            axes.lx,
            axes.ly,
            axes.rx,
            axes.ry,
            axes.lt,
            axes.rt
        ),
        Body::Sync { flags } => format_sync(*flags),
        Body::Unknown { kind, body } => {
            if body.is_empty() {
                format!("unknown {kind}")
            } else {
                format!("unknown {kind} {}", hex(body))
            }
        }
    };
    if event.time_us == 0 {
        body
    } else {
        format!("t={} {body}", event.time_us)
    }
}

fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return &line[..i];
        }
        i += 1;
    }
    line
}

fn parse_line(line: &str) -> Result<Event, String> {
    let mut tokens: Vec<&str> = line.split_whitespace().collect();
    let mut time_us = 0u64;
    if let Some(rest) = tokens.first().and_then(|token| token.strip_prefix("t=")) {
        time_us = rest
            .parse::<u64>()
            .map_err(|_| format!("bad time '{rest}'"))?;
        tokens.remove(0);
    }
    if tokens.is_empty() {
        return Err("missing event".into());
    }
    let body = match tokens[0] {
        "key" => parse_key(&tokens[1..])?,
        "pointer" => parse_pointer(&tokens[1..])?,
        "delta" => parse_delta(&tokens[1..])?,
        "pad" => parse_pad(&tokens[1..])?,
        "sync" => parse_sync(&tokens[1..])?,
        "unknown" => parse_unknown(&tokens[1..])?,
        other => return Err(format!("unknown event '{other}'")),
    };
    Ok(Event { time_us, body })
}

fn parse_key(tokens: &[&str]) -> Result<Body, String> {
    if tokens.len() != 3 {
        return Err("key expects down|up, hid|set1|keysym, and a code".into());
    }
    let down = match tokens[0] {
        "down" => true,
        "up" => false,
        other => return Err(format!("expected down or up, got '{other}'")),
    };
    let source = match tokens[1] {
        "hid" => KeySource::Hid,
        "set1" => KeySource::Set1,
        "keysym" => KeySource::Keysym,
        other => return Err(format!("expected hid, set1, or keysym, got '{other}'")),
    };
    Ok(Body::Key {
        down,
        source,
        code: parse_u32(tokens[2])?,
    })
}

fn parse_pointer(tokens: &[&str]) -> Result<Body, String> {
    if tokens.len() < 2 {
        return Err("pointer expects X Y".into());
    }
    let (buttons, wheel_y, wheel_x) = parse_tail(&tokens[2..])?;
    Ok(Body::Pointer {
        x: parse_u16(tokens[0])?,
        y: parse_u16(tokens[1])?,
        buttons,
        wheel_y,
        wheel_x,
    })
}

fn parse_delta(tokens: &[&str]) -> Result<Body, String> {
    if tokens.len() < 2 {
        return Err("delta expects DX DY".into());
    }
    let (buttons, wheel_y, wheel_x) = parse_tail(&tokens[2..])?;
    Ok(Body::Delta {
        dx: parse_i16(tokens[0])?,
        dy: parse_i16(tokens[1])?,
        buttons,
        wheel_y,
        wheel_x,
    })
}

fn parse_tail(tokens: &[&str]) -> Result<(u8, i16, i16), String> {
    let mut buttons = None;
    let mut wheel_y = 0i16;
    let mut wheel_x = 0i16;
    for token in tokens {
        if let Some(value) = token.strip_prefix("wheel=") {
            wheel_y = parse_i16(value)?;
        } else if let Some(value) = token.strip_prefix("hwheel=") {
            wheel_x = parse_i16(value)?;
        } else if buttons.is_some() {
            return Err(format!("unexpected token '{token}'"));
        } else {
            buttons = Some(parse_mouse(token)?);
        }
    }
    Ok((buttons.unwrap_or(0), wheel_y, wheel_x))
}

fn parse_mouse(token: &str) -> Result<u8, String> {
    if let Some(mask) = parse_number(token) {
        return u8::try_from(mask).map_err(|_| format!("button mask '{token}' does not fit in u8"));
    }
    let mut mask = 0u8;
    for part in token.split(|c| c == '+' || c == ',' || c == '|') {
        if part.is_empty() {
            return Err(format!("bad button list '{token}'"));
        }
        mask |= match part.to_ascii_lowercase().as_str() {
            "left" => Buttons::LEFT,
            "middle" => Buttons::MIDDLE,
            "right" => Buttons::RIGHT,
            "x1" => Buttons::X1,
            "x2" => Buttons::X2,
            "none" => 0,
            _ => return Err(format!("unknown button '{part}'")),
        };
    }
    Ok(mask)
}

fn parse_pad(tokens: &[&str]) -> Result<Body, String> {
    if tokens.is_empty() {
        return Err("pad expects an index".into());
    }
    let index = u8::try_from(parse_u32(tokens[0])?)
        .map_err(|_| format!("pad index '{}' does not fit in u8", tokens[0]))?;
    let mut buttons = 0u32;
    let mut saw_name = false;
    let mut axes = Axes::default();
    let mut saw_kv = false;
    let mut bare = Vec::new();
    for token in &tokens[1..] {
        if let Some((key, value)) = token.split_once('=') {
            let n = parse_i16(value)?;
            match key {
                "lx" => axes.lx = n,
                "ly" => axes.ly = n,
                "rx" => axes.rx = n,
                "ry" => axes.ry = n,
                "lt" => axes.lt = n,
                "rt" => axes.rt = n,
                other => return Err(format!("unknown axis '{other}'")),
            }
            saw_kv = true;
            continue;
        }
        if let Some(mask) = pad_names(token) {
            buttons |= mask;
            saw_name = true;
            continue;
        }
        if let Some(n) = parse_number(token) {
            bare.push(n);
            continue;
        }
        return Err(format!("unexpected pad token '{token}'"));
    }
    if saw_kv {
        if bare.len() > 1 || (bare.len() == 1 && saw_name) {
            return Err("pad mixes a button mask with axis values".into());
        }
        if let Some(mask) = bare.first() {
            buttons |= u32_in(*mask)?;
        }
    } else {
        match bare.len() {
            0 => {}
            1 => {
                if saw_name {
                    return Err("pad has both button names and a mask".into());
                }
                buttons |= u32_in(bare[0])?;
            }
            6 => assign_axes(&mut axes, &bare)?,
            7 => {
                if saw_name {
                    return Err("pad has both button names and a mask".into());
                }
                buttons |= u32_in(bare[0])?;
                assign_axes(&mut axes, &bare[1..])?;
            }
            _ => {
                return Err("pad expects button names, a mask, or six axis values".into());
            }
        }
    }
    Ok(Body::Gamepad {
        index,
        buttons,
        axes,
    })
}

fn pad_names(token: &str) -> Option<u32> {
    if token.is_empty() || parse_number(token).is_some() {
        return None;
    }
    let mut mask = 0u32;
    for part in token.split(|c| c == '+' || c == ',' || c == '|') {
        mask |= single_pad(part)?;
    }
    Some(mask)
}

fn single_pad(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "south" | "a" => PAD_SOUTH,
        "east" | "b" => PAD_EAST,
        "west" | "x" => PAD_WEST,
        "north" | "y" => PAD_NORTH,
        "lb" => PAD_LB,
        "rb" => PAD_RB,
        "back" => PAD_BACK,
        "start" => PAD_START,
        "guide" => PAD_GUIDE,
        "lstick" => PAD_LSTICK,
        "rstick" => PAD_RSTICK,
        "up" => PAD_UP,
        "down" => PAD_DOWN,
        "left" => PAD_LEFT,
        "right" => PAD_RIGHT,
        _ => return None,
    })
}

fn assign_axes(axes: &mut Axes, nums: &[i64]) -> Result<(), String> {
    let mut vals = [0i16; 6];
    for (slot, value) in vals.iter_mut().zip(nums) {
        *slot =
            i16::try_from(*value).map_err(|_| format!("axis value {value} does not fit in i16"))?;
    }
    axes.lx = vals[0];
    axes.ly = vals[1];
    axes.rx = vals[2];
    axes.ry = vals[3];
    axes.lt = vals[4];
    axes.rt = vals[5];
    Ok(())
}

fn parse_sync(tokens: &[&str]) -> Result<Body, String> {
    let mut flags = 0u8;
    for token in tokens {
        if let Some(mask) = parse_number(token) {
            flags |= u8::try_from(mask).map_err(|_| format!("sync flags '{token}' do not fit"))?;
            continue;
        }
        flags |= match token.to_ascii_lowercase().as_str() {
            "caps" => SYNC_CAPS,
            "num" => SYNC_NUM,
            "scroll" => SYNC_SCROLL,
            "kana" => SYNC_KANA,
            other => return Err(format!("unknown sync flag '{other}'")),
        };
    }
    Ok(Body::Sync { flags })
}

fn parse_unknown(tokens: &[&str]) -> Result<Body, String> {
    if tokens.is_empty() || tokens.len() > 2 {
        return Err("unknown expects a kind and optional hex body".into());
    }
    let kind = u8::try_from(parse_u32(tokens[0])?)
        .map_err(|_| format!("kind '{}' does not fit in u8", tokens[0]))?;
    let body = if tokens.len() == 2 {
        parse_hex(tokens[1])?
    } else {
        Vec::new()
    };
    Ok(Body::Unknown { kind, body })
}

fn parse_u16(token: &str) -> Result<u16, String> {
    let value = parse_number(token).ok_or_else(|| format!("bad number '{token}'"))?;
    u16::try_from(value).map_err(|_| format!("number '{token}' does not fit in u16"))
}

fn parse_i16(token: &str) -> Result<i16, String> {
    let value = parse_number(token).ok_or_else(|| format!("bad number '{token}'"))?;
    i16::try_from(value).map_err(|_| format!("number '{token}' does not fit in i16"))
}

fn parse_u32(token: &str) -> Result<u32, String> {
    let value = parse_number(token).ok_or_else(|| format!("bad number '{token}'"))?;
    u32_in(value).map_err(|_| format!("number '{token}' does not fit in u32"))
}

fn u32_in(value: i64) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("number {value} does not fit in u32"))
}

fn parse_number(token: &str) -> Option<i64> {
    if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok()
    } else {
        token.parse().ok()
    }
}

fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err(format!("odd hex length in '{text}'"));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_val(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("bad hex digit '{}'", byte as char)),
    }
}

fn format_mouse(buttons: u8) -> String {
    if buttons & !Buttons::KNOWN != 0 {
        return buttons.to_string();
    }
    if buttons == 0 {
        return "0".into();
    }
    let mut names = Vec::new();
    if buttons & Buttons::LEFT != 0 {
        names.push("left");
    }
    if buttons & Buttons::MIDDLE != 0 {
        names.push("middle");
    }
    if buttons & Buttons::RIGHT != 0 {
        names.push("right");
    }
    if buttons & Buttons::X1 != 0 {
        names.push("x1");
    }
    if buttons & Buttons::X2 != 0 {
        names.push("x2");
    }
    names.join("+")
}

fn format_pad(buttons: u32) -> String {
    if buttons & !PAD_KNOWN != 0 {
        return buttons.to_string();
    }
    if buttons == 0 {
        return "0".into();
    }
    let pairs = [
        (PAD_SOUTH, "south"),
        (PAD_EAST, "east"),
        (PAD_WEST, "west"),
        (PAD_NORTH, "north"),
        (PAD_LB, "lb"),
        (PAD_RB, "rb"),
        (PAD_BACK, "back"),
        (PAD_START, "start"),
        (PAD_GUIDE, "guide"),
        (PAD_LSTICK, "lstick"),
        (PAD_RSTICK, "rstick"),
        (PAD_UP, "up"),
        (PAD_DOWN, "down"),
        (PAD_LEFT, "left"),
        (PAD_RIGHT, "right"),
    ];
    pairs
        .into_iter()
        .filter(|(bit, _)| buttons & bit != 0)
        .map(|(_, name)| name)
        .collect::<Vec<_>>()
        .join("+")
}

fn format_sync(flags: u8) -> String {
    if flags == 0 {
        return "sync".into();
    }
    if flags & !SYNC_KNOWN != 0 {
        return format!("sync {flags}");
    }
    let mut parts = vec!["sync".to_string()];
    if flags & SYNC_CAPS != 0 {
        parts.push("caps".into());
    }
    if flags & SYNC_NUM != 0 {
        parts.push("num".into());
    }
    if flags & SYNC_SCROLL != 0 {
        parts.push("scroll".into());
    }
    if flags & SYNC_KANA != 0 {
        parts.push("kana".into());
    }
    parts.join(" ")
}

fn axis_suffix(name: &str, value: i16) -> String {
    if value == 0 {
        String::new()
    } else {
        format!(" {name}={value}")
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Axes, KeySource, PAD_EAST, PAD_SOUTH, SYNC_CAPS, SYNC_NUM};

    fn events() -> Vec<Event> {
        vec![
            Event {
                time_us: 1000,
                body: Body::Key {
                    down: true,
                    source: KeySource::Hid,
                    code: 0x0007_0004,
                },
            },
            Event {
                time_us: 0,
                body: Body::Key {
                    down: false,
                    source: KeySource::Set1,
                    code: 0x11D,
                },
            },
            Event {
                time_us: 0,
                body: Body::Pointer {
                    x: 32768,
                    y: 32768,
                    buttons: Buttons::LEFT | Buttons::RIGHT,
                    wheel_y: 1,
                    wheel_x: -1,
                },
            },
            Event {
                time_us: 0,
                body: Body::Delta {
                    dx: -10,
                    dy: 4,
                    buttons: Buttons::RIGHT,
                    wheel_y: 0,
                    wheel_x: 0,
                },
            },
            Event {
                time_us: 5,
                body: Body::Gamepad {
                    index: 0,
                    buttons: PAD_SOUTH | PAD_EAST,
                    axes: Axes {
                        lx: 0,
                        ly: 0,
                        rx: 0,
                        ry: 0,
                        lt: 32767,
                        rt: 0,
                    },
                },
            },
            Event {
                time_us: 0,
                body: Body::Sync {
                    flags: SYNC_CAPS | SYNC_NUM,
                },
            },
            Event {
                time_us: 0,
                body: Body::Unknown {
                    kind: 99,
                    body: vec![0x01, 0x02],
                },
            },
        ]
    }

    #[test]
    fn script_roundtrip() {
        let original = events();
        let text = format_script(&original);
        assert_eq!(parse_script(&text).unwrap(), original);
    }

    #[test]
    fn comments_blank_lines_and_aliases() {
        let text = "\u{feff}# comment\n\nkey down hid 0x4\npointer 1 2 left,middle wheel=-2\npad 0 a lx=1 ly=-2\nsync\nunknown 9\n";
        let parsed = parse_script(text).unwrap();
        assert_eq!(parsed.len(), 5);
        assert_eq!(
            parsed[0].body,
            Body::Key {
                down: true,
                source: KeySource::Hid,
                code: 4,
            }
        );
        assert_eq!(parsed[1].time_us, 0);
        match &parsed[2].body {
            Body::Gamepad { buttons, axes, .. } => {
                assert_eq!(*buttons, PAD_SOUTH);
                assert_eq!(axes.lx, 1);
                assert_eq!(axes.ly, -2);
            }
            other => panic!("expected pad, got {other:?}"),
        }
    }

    #[test]
    fn script_error_names_the_line() {
        let err = parse_script("key down hid 1\nnope\n").unwrap_err();
        assert!(err.to_string().contains("line 2"));
    }
}
