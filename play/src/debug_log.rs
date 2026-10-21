//! Toggleable lines for latency and packet problems. The desk reads them.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

struct Log {
    lines: Vec<String>,
}

fn log() -> &'static Mutex<Log> {
    static LOG: Mutex<Log> = Mutex::new(Log { lines: Vec::new() });
    &LOG
}

pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ON.load(Ordering::SeqCst)
}

/// Picture stats stay on the screen when the network trace is on, even if
/// input-timing trace would have hidden them.
pub fn show_stats(input_trace: bool, network_trace: bool, event_is_stats: bool) -> bool {
    network_trace || !(input_trace && event_is_stats)
}

pub fn push(line: &str) {
    if !enabled() {
        return;
    }
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let mut slot = log().lock().unwrap_or_else(|poison| poison.into_inner());
    if slot.lines.last().map(String::as_str) == Some(line) {
        return;
    }
    slot.lines.push(line.to_string());
    if slot.lines.len() > 40 {
        let drop_n = slot.lines.len() - 40;
        slot.lines.drain(0..drop_n);
    }
}

pub fn lines() -> Vec<String> {
    log()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .lines
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_trace_keeps_stats_that_input_timing_would_hide() {
        assert!(!show_stats(true, false, true));
        assert!(show_stats(true, true, true));
        assert!(show_stats(false, false, true));
        assert!(show_stats(true, false, false));
    }
}
