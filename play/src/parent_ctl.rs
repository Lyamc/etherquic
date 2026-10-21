//! Lines the desk writes on the picture child's stdin.
//!
//! `set key=value` changes the open view or the offer. End of stdin still
//! ends the session.

use etherquic::negotiate::{Audio, FpsWish, Video, YuvFormat};

use etherquic::fruc::FrameGen;
use etherquic::look::{Dither, Look};
use etherquic::upscale::Upscale;

use crate::{CursorMode, LiveTune, LiveView, ScaleMode, SizeMatch};

pub enum Control {
    Applied,
    Stop,
    Ignore,
}

pub fn apply_line(view: &LiveView, tune: &LiveTune, line: &str) -> Control {
    let line = line.trim();
    if line.is_empty() {
        return Control::Ignore;
    }
    if line == "stop" {
        return Control::Stop;
    }
    let Some(rest) = line.strip_prefix("set ") else {
        return Control::Ignore;
    };
    let Some((key, value)) = rest.split_once('=') else {
        return Control::Ignore;
    };
    let key = key.trim();
    let value = value.trim();
    if key == "debug-trace" {
        crate::debug_log::set_enabled(parse_yes(value));
        return Control::Applied;
    }
    if apply_tune(tune, key, value) || apply_view(view, key, value) {
        Control::Applied
    } else {
        Control::Ignore
    }
}

fn apply_tune(tune: &LiveTune, key: &str, value: &str) -> bool {
    let mut slot = match tune.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    match key {
        "video" => {
            slot.video = Video::from_token(value);
            slot.video.is_some()
        }
        "audio" => {
            slot.audio = Audio::from_token(value);
            slot.audio.is_some()
        }
        "carrier" => match value.parse::<u8>() {
            Ok(id) if (1..=8).contains(&id) => {
                slot.carrier = Some(id);
                true
            }
            _ => false,
        },
        "fps" => {
            slot.fps = Some(FpsWish::from_token(value));
            true
        }
        _ => false,
    }
}

fn apply_view(view: &LiveView, key: &str, value: &str) -> bool {
    let mut slot = match view.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    match key {
        "scale" => {
            slot.scale = ScaleMode::from_token(value).unwrap_or(slot.scale);
            true
        }
        "size-match" => {
            slot.size_match = SizeMatch::from_token(value).unwrap_or(slot.size_match);
            true
        }
        "upscale" => {
            slot.upscale = Upscale::from_token(value).unwrap_or(slot.upscale);
            true
        }
        "frame-gen" => {
            slot.frame_gen = FrameGen::from_token(value).unwrap_or(slot.frame_gen);
            true
        }
        "look" => {
            slot.look = Look::from_token(value).unwrap_or(slot.look);
            true
        }
        "dither" => {
            slot.dither = Dither::from_token(value).unwrap_or(slot.dither);
            true
        }
        "pixel-size" => {
            if let Ok(size) = value.parse::<u8>() {
                slot.pixel_size = size.clamp(1, 32);
            }
            true
        }
        "quality" => {
            if let Ok(quality) = value.parse::<u8>() {
                slot.quality = quality.clamp(1, 100);
            }
            true
        }
        "dirty-rects" => {
            slot.dirty_rects = parse_yes(value);
            true
        }
        "debug-dirty-rects" => {
            slot.debug_dirty_rects = parse_yes(value);
            true
        }
        "yuv" => {
            slot.yuv = YuvFormat::from_token(value).unwrap_or(slot.yuv);
            true
        }
        "cursor" => {
            slot.cursor = CursorMode::from_token(value).unwrap_or(slot.cursor);
            true
        }
        "keyboard" => {
            slot.send_keyboard = parse_yes(value);
            true
        }
        "mouse" => {
            slot.send_mouse = parse_yes(value);
            true
        }
        "pads" => {
            slot.send_pads = parse_yes(value);
            true
        }
        _ => false,
    }
}

fn parse_yes(value: &str) -> bool {
    matches!(value, "1" | "yes" | "true" | "on")
}

/// Read stdin until it closes. `set` lines update the open session.
pub fn watch_stdin(stop: std::sync::Arc<std::sync::atomic::AtomicBool>, view: LiveView, tune: LiveTune) {
    let _ = std::thread::Builder::new()
        .name("etherquic-play-parent".into())
        .spawn(move || {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if matches!(apply_line(&view, &tune, &line), Control::Stop) {
                    break;
                }
            }
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{live, snapshot};

    #[test]
    fn a_set_line_changes_the_open_view_and_codec() {
        let view = live(crate::ViewOptions::default());
        let tune = crate::live_tune(crate::SessionTune::default());
        assert!(matches!(
            apply_line(&view, &tune, "set scale=native"),
            Control::Applied
        ));
        assert_eq!(snapshot(&view).scale, ScaleMode::Native);
        assert!(matches!(
            apply_line(&view, &tune, "set video=mjpeg"),
            Control::Applied
        ));
        assert_eq!(
            crate::snapshot_tune(&tune).video,
            Some(Video::Mjpeg)
        );
        assert!(matches!(
            apply_line(&view, &tune, "set carrier=2"),
            Control::Applied
        ));
        assert_eq!(crate::snapshot_tune(&tune).carrier, Some(2));
        assert!(matches!(apply_line(&view, &tune, "stop"), Control::Stop));
        assert!(matches!(
            apply_line(&view, &tune, "noise"),
            Control::Ignore
        ));
        let other = live(crate::ViewOptions::default());
        let other_tune = crate::live_tune(crate::SessionTune::default());
        for line in [
            "set scale=native",
            "set look=blocks",
            "set dither=bayer8",
            "set quality=70",
            "set dirty-rects=yes",
            "set cursor=remote",
            "set keyboard=no",
            "set mouse=no",
            "set upscale=fsr",
            "set frame-gen=on",
            "set yuv=444p8",
            "set audio=opus",
            "set fps=30",
        ] {
            assert!(
                matches!(apply_line(&view, &tune, line), Control::Applied),
                "{line}"
            );
        }
        let updated = snapshot(&view);
        assert_eq!(updated.scale, ScaleMode::Native);
        assert_eq!(updated.look, etherquic::look::Look::Blocks);
        assert_eq!(updated.dither, etherquic::look::Dither::Bayer8);
        assert_eq!(updated.quality, 70);
        assert!(updated.dirty_rects);
        assert_eq!(updated.cursor, CursorMode::Remote);
        assert!(!updated.send_keyboard);
        assert!(!updated.send_mouse);
        assert_eq!(updated.upscale, etherquic::upscale::Upscale::Fsr);
        assert_eq!(updated.frame_gen, etherquic::fruc::FrameGen::On);
        assert_eq!(
            updated.yuv,
            etherquic::negotiate::YuvFormat::YUV444P8
        );
        let tune_now = crate::snapshot_tune(&tune);
        assert_eq!(tune_now.audio, Some(Audio::Opus));
        assert_eq!(tune_now.fps, Some(FpsWish::Fixed(30)));
        assert_eq!(snapshot(&other).scale, ScaleMode::Fit);
        assert_eq!(crate::snapshot_tune(&other_tune).video, None);
        assert_eq!(crate::snapshot_tune(&other_tune).audio, None);
        assert_eq!(crate::snapshot_tune(&other_tune).carrier, None);
    }
}
