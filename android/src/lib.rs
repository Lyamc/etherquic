//! Native library the EtherQuic Play APK loads.
//!
//! Android joins over UDP. Parse the host-card text a camera shows after a
//! QR scan; [`udp_peer_from_text`] is the address [`etherquic_play::join_udp`]
//! dials. The NativeActivity entry [`android_main`] binds GPUI and opens Desk.

#![allow(unsafe_code)]

use std::ffi::{c_char, CStr};
use std::net::SocketAddr;

use etherquic::share::Share;

pub use etherquic_play::{join_udp, parse_share, udp_peer};

/// Parse host-card text and return the UDP peer a phone dials.
pub fn udp_peer_from_text(text: &str) -> Option<SocketAddr> {
    Share::parse(text)?.udp_peer()
}

/// 1 when `text` is a host card with a UDP address, 0 otherwise.
///
/// `text` is a UTF-8 C string from JNI / `GetStringUTFChars`.
#[no_mangle]
pub extern "C" fn etherquic_android_has_udp_peer(text: *const c_char) -> i32 {
    let Some(text) = (unsafe { cstr(text) }) else {
        return 0;
    };
    i32::from(udp_peer_from_text(text).is_some())
}

unsafe fn cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok()
}

/// NativeActivity entry. GPUI polls this app and draws Desk on the surface.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("etherquic"),
    );
    log::info!("etherquic play starting");
    wait_for_window(&app);
    log::info!("native window ready, starting desk");
    etherquic_ui::bind_android_app(app);
    etherquic_ui::run();
}

#[cfg(target_os = "android")]
fn wait_for_window(app: &android_activity::AndroidApp) {
    use android_activity::{MainEvent, PollEvent};
    use std::time::Duration;

    if app.native_window().is_some() {
        return;
    }
    loop {
        let mut destroy = false;
        app.poll_events(Some(Duration::from_millis(50)), |event| {
            if let PollEvent::Main(main) = event {
                log::info!("android event: {main:?}");
                if matches!(main, MainEvent::Destroy) {
                    destroy = true;
                }
            }
        });
        if destroy {
            return;
        }
        if app.native_window().is_some() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::net::Ipv4Addr;

    #[test]
    fn host_card_text_yields_the_udp_peer() {
        let text = "etherquic\nmac 02:00:00:00:00:01\nservice 4433\nport 4434\nip 192.168.0.24\n";
        assert_eq!(
            udp_peer_from_text(text),
            Some(SocketAddr::from((Ipv4Addr::new(192, 168, 0, 24), 4434)))
        );
        let c = CString::new(text).unwrap();
        assert_eq!(etherquic_android_has_udp_peer(c.as_ptr()), 1);
        let bad = CString::new("not a card").unwrap();
        assert_eq!(etherquic_android_has_udp_peer(bad.as_ptr()), 0);
        assert_eq!(etherquic_android_has_udp_peer(std::ptr::null()), 0);
    }
}
