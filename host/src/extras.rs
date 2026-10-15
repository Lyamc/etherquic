//! What the service adds to a discovery offer: this PC's identity and inputs,
//! and a small JPEG XL picture of the console desktop.
//!
//! The picture comes from the desk helper's newest frame, so the service
//! itself never touches the console session. Both are cached and refreshed
//! every [`REFRESH`] on a background thread, so an announce never waits.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use etherquic::discover::{
    Capabilities, MonitorInfo, OfferExtras, INPUT_KEYBOARD, INPUT_MOUSE, OS_WINDOWS, VPAD_NONE,
};
use etherquic::thumbnail::{cjxl_path, downscale, encode_cjxl, fit_to_budget, THUMB_WIDTHS};

/// The newest desktop JPEG, written by the desk bridge.
pub type Picture = Arc<Mutex<Option<Vec<u8>>>>;

pub const REFRESH: Duration = Duration::from_secs(30);
/// Encoded before any player asks, so the first answer already carries a
/// picture. Smaller than any budget an IPv4, IPv6, or L2 offer leaves.
const SEED_BUDGET: usize = 900;

#[derive(Default)]
struct Cache {
    caps: Option<Capabilities>,
    /// The thumbnail for each budget asked for, newest picture.
    thumbs: Vec<(usize, Vec<u8>)>,
    /// Budgets asked for. The refresher encodes these.
    budgets: Vec<usize>,
    /// RGBA and size of the newest decoded frame.
    frame: Option<(Vec<u8>, u32, u32)>,
}

pub struct HostExtras {
    cache: Arc<Mutex<Cache>>,
}

impl HostExtras {
    /// Start the refresher. It stops with `stop`.
    /// `mac` is this host's L2 address. It names the host in every offer, so
    /// a player merges the raw Ethernet and UDP answers into one row.
    pub fn start(picture: Picture, mac: [u8; 6], stop: Arc<AtomicBool>) -> Self {
        let cache = Arc::new(Mutex::new(Cache {
            budgets: vec![SEED_BUDGET],
            ..Cache::default()
        }));
        let worker = Arc::clone(&cache);
        let cjxl = cjxl_path();
        let _ = thread::Builder::new()
            .name("etherquic-extras".into())
            .spawn(move || refresh_loop(worker, picture, cjxl, mac, stop));
        Self { cache }
    }
}

impl OfferExtras for HostExtras {
    fn capabilities(&self) -> Option<Capabilities> {
        lock(&self.cache).caps.clone()
    }

    fn thumbnail(&self, budget: usize) -> Option<Vec<u8>> {
        let mut cache = lock(&self.cache);
        if !cache.budgets.contains(&budget) {
            cache.budgets.push(budget);
        }
        cache
            .thumbs
            .iter()
            .find(|(b, _)| *b == budget)
            .map(|(_, bytes)| bytes.clone())
            .or_else(|| {
                // A smaller cached picture also fits.
                cache
                    .thumbs
                    .iter()
                    .filter(|(_, bytes)| bytes.len() <= budget)
                    .max_by_key(|(_, bytes)| bytes.len())
                    .map(|(_, bytes)| bytes.clone())
            })
    }
}

fn refresh_loop(
    cache: Arc<Mutex<Cache>>,
    picture: Picture,
    cjxl: Option<PathBuf>,
    mac: [u8; 6],
    stop: Arc<AtomicBool>,
) {
    // The first refresh then carries the real encoders.
    let _ = etherquic_media::caps::probe();
    let mut last: Option<Instant> = None;
    let mut encoded_budgets: Vec<usize> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        // Until the helper's first frame arrives, look again every second.
        let waiting = lock(&cache).frame.is_none()
            && last.is_none_or(|at| at.elapsed() >= Duration::from_secs(1));
        let due = waiting || last.is_none_or(|at| at.elapsed() >= REFRESH);
        let new_budget = {
            let cache = lock(&cache);
            cache.budgets.iter().any(|b| !encoded_budgets.contains(b))
        };
        if due || new_budget {
            if due {
                let jpeg = lock(&picture).clone();
                if let Some((rgba, w, h)) = jpeg.as_deref().and_then(decode_jpeg) {
                    lock(&cache).frame = Some((rgba, w, h));
                }
                let frame_size = lock(&cache).frame.as_ref().map(|(_, w, h)| (*w, *h));
                let mut caps = capabilities(frame_size);
                // Npcap and Membrane report no adapter MAC on Windows.
                caps.nic_mac = Some(mac);
                lock(&cache).caps = Some(caps);
                last = Some(Instant::now());
            }
            let (frame, budgets) = {
                let cache = lock(&cache);
                (cache.frame.clone(), cache.budgets.clone())
            };
            if let (Some((rgba, w, h)), Some(cjxl)) = (frame, cjxl.as_ref()) {
                let mut thumbs = Vec::new();
                for &budget in &budgets {
                    let scaled = |width: u32| downscale(&rgba, w, h, width);
                    if let Some(bytes) = fit_to_budget(budget, &THUMB_WIDTHS, |width, distance| {
                        encode_cjxl(cjxl, &scaled(width), distance)
                    }) {
                        thumbs.push((budget, bytes));
                    }
                }
                lock(&cache).thumbs = thumbs;
            }
            encoded_budgets = budgets;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// RGBA and size of a JPEG.
pub fn decode_jpeg(jpeg: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = zune_jpeg::JpegDecoder::new(jpeg);
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    let count = w.checked_mul(h)?;
    if count == 0 {
        return None;
    }
    let channels = pixels.len() / count;
    let mut rgba = Vec::with_capacity(count * 4);
    match channels {
        3 => {
            for px in pixels.chunks_exact(3) {
                rgba.extend([px[0], px[1], px[2], 255]);
            }
        }
        4 => rgba = pixels,
        1 => {
            for &v in &pixels {
                rgba.extend([v, v, v, 255]);
            }
        }
        _ => return None,
    }
    Some((rgba, w as u32, h as u32))
}

/// This PC as a player sees it. The service cannot see the console's
/// displays, so the one monitor listed is the size the helper captures.
pub fn capabilities(frame: Option<(u32, u32)>) -> Capabilities {
    let monitors = frame
        .map(|(w, h)| {
            vec![MonitorInfo {
                width: w.min(u16::MAX as u32) as u16,
                height: h.min(u16::MAX as u32) as u16,
                refresh_centihz: 0,
                x: 0,
                y: 0,
                primary: true,
            }]
        })
        .unwrap_or_default();
    Capabilities {
        os: OS_WINDOWS,
        os_label: "Windows".into(),
        hostname: std::env::var("COMPUTERNAME").unwrap_or_default(),
        nic_mac: None,
        monitors,
        inputs: INPUT_KEYBOARD | INPUT_MOUSE,
        virtual_pad: VPAD_NONE,
        pads: Vec::new(),
        audio_outputs: Vec::new(),
        audio_inputs: Vec::new(),
        codecs: etherquic_media::caps::caps_text_if_ready(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let rgb: Vec<u8> = (0..(w as usize * h as usize))
            .flat_map(|i| [(i % 256) as u8, 128, 64])
            .collect();
        jpeg_encoder::Encoder::new(&mut out, 80)
            .encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    #[test]
    fn helper_frame_decodes_to_rgba_and_sizes_the_monitor() {
        let (rgba, w, h) = decode_jpeg(&tiny_jpeg(64, 36)).expect("decodes");
        assert_eq!((w, h), (64, 36));
        assert_eq!(rgba.len(), 64 * 36 * 4);
        let caps = capabilities(Some((w, h)));
        assert_eq!(caps.os, OS_WINDOWS);
        assert_eq!(caps.monitors.len(), 1);
        assert_eq!((caps.monitors[0].width, caps.monitors[0].height), (64, 36));
        // The offer round-trips.
        let back = Capabilities::parse(&caps.encode()).expect("decodes");
        assert_eq!(back.monitors, caps.monitors);
    }

    #[test]
    fn no_frame_yet_means_no_picture_but_caps_still_answer() {
        let stop = Arc::new(AtomicBool::new(false));
        let extras = HostExtras::start(Arc::new(Mutex::new(None)), [2, 0, 0, 0, 0, 7], Arc::clone(&stop));
        thread::sleep(Duration::from_millis(400));
        let caps = extras.capabilities().expect("caps");
        assert_eq!(caps.nic_mac, Some([2, 0, 0, 0, 0, 7]));
        assert!(extras.thumbnail(1000).is_none());
        stop.store(true, Ordering::SeqCst);
    }
}
