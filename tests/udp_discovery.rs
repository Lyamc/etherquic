//! Discovery over UDP, end to end on this machine: a host answers on port
//! 4440, a newer player gets capabilities and the thumbnail, and an older
//! announce gets the older offer back.
//!
//! Binds the fixed discovery port, so it is ignored by default:
//! `cargo test --test udp_discovery -- --ignored`

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use etherquic::discover::{
    discover_burst, serve_udp, Capabilities, HostCard, MonitorInfo, OfferExtras, PadInfo,
    INPUT_KEYBOARD, INPUT_MOUSE, OS_LINUX, PAD_API_EVDEV, PAD_KIND_GAMEPAD, UDP4_PAYLOAD_LIMIT,
    UDP_PORT, VPAD_UINPUT,
};

const PICTURE: &[u8] = include_bytes!("../ui/src/testdata/thumb.jxl");

struct Extras;

impl OfferExtras for Extras {
    fn capabilities(&self) -> Option<Capabilities> {
        Some(Capabilities {
            os: OS_LINUX,
            os_label: "NixOS".into(),
            hostname: "udp-test".into(),
            nic_mac: Some([0x02, 0x11, 0x22, 0x33, 0x44, 0x55]),
            monitors: vec![
                MonitorInfo {
                    width: 2560,
                    height: 1440,
                    refresh_centihz: 16500,
                    x: 0,
                    y: 0,
                    primary: true,
                },
                MonitorInfo {
                    width: 1920,
                    height: 1080,
                    refresh_centihz: 6000,
                    x: 2560,
                    y: 0,
                    primary: false,
                },
            ],
            inputs: INPUT_KEYBOARD | INPUT_MOUSE,
            virtual_pad: VPAD_UINPUT,
            pads: vec![PadInfo {
                api: PAD_API_EVDEV,
                kind: PAD_KIND_GAMEPAD,
                name: "8BitDo Pro 2".into(),
            }],
            audio_outputs: Vec::new(),
            audio_inputs: Vec::new(),
            codecs: String::new(),
        })
    }

    fn thumbnail(&self, budget: usize) -> Option<Vec<u8>> {
        (PICTURE.len() <= budget).then(|| PICTURE.to_vec())
    }
}

fn card() -> HostCard {
    HostCard {
        name: "udp-test".into(),
        l2_mac: [0x02, 0, 0, 0, 0, 1],
        l2_service: 4433,
        l3_port: 4433,
        video: Some(Vec::new()),
        audio: Some(Vec::new()),
        l2_backend: 0,
    }
}

#[test]
#[ignore = "binds UDP port 4440"]
fn udp_discovery_end_to_end() {
    let stop = Arc::new(AtomicBool::new(false));
    let host = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || serve_udp(None, &card(), &Extras, &stop))
    };
    std::thread::sleep(Duration::from_millis(300));

    let burst = discover_burst(None, &AtomicU64::new(0), 0);
    let found = burst
        .found
        .iter()
        .find(|found| found.name == "udp-test")
        .unwrap_or_else(|| panic!("host not found: {burst:?}"));
    assert!(found.via_udp);
    assert_eq!(found.key, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let caps = found.caps.as_ref().expect("capabilities");
    assert_eq!(caps.monitors.len(), 2);
    assert_eq!(caps.pads[0].name, "8BitDo Pro 2");
    assert_eq!(found.thumbnail.as_deref(), Some(PICTURE));

    // An older player: an unmarked id gets a version-2 offer with no extras.
    let old = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    old.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut announce = b"EQDS".to_vec();
    announce.extend([1, 1]);
    announce.extend(0x0000_1234u32.to_be_bytes());
    old.send_to(&announce, (Ipv4Addr::LOCALHOST, UDP_PORT))
        .unwrap();
    let mut buf = [0u8; 2048];
    let (len, _) = old.recv_from(&mut buf).expect("an answer");
    assert!(len <= UDP4_PAYLOAD_LIMIT);
    assert_eq!(&buf[..4], b"EQDS");
    assert_eq!(buf[4], 2, "older players get version 2");
    assert_eq!(u32::from_be_bytes(buf[6..10].try_into().unwrap()), 0x1234);

    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    host.join().unwrap().unwrap();
}
