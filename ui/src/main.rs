//! `etherquic` window. `host` and `connect` are the two commands.

#![windows_subsystem = "windows"]

mod badge;
mod console;
mod place;
mod scan;
mod taskbar;
mod tray;
mod view;

fn main() {
    console::attach();
    if let Err(error) = etherquic::capture_from_env() {
        eprintln!("etherquic: capture: {error}");
    }
    view::run();
    if let Ok(info) = etherquic::capture_stop() {
        if info.dropped_ether > 0 || info.dropped_app > 0 {
            eprintln!(
                "etherquic: capture in {} dropped {} ethernet frames and {} records",
                info.dir.display(),
                info.dropped_ether,
                info.dropped_app
            );
        }
    }
}
