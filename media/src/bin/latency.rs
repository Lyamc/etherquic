//! Realtime 30 fps encode -> decode chain per working codec; prints the
//! delay from pushing frame k to decoding frame k. `eqmedia-latency [WxH] [frames]`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use etherquic_media::video::{EncodeSetup, VideoDecoder, VideoEncoder};
use etherquic_media::{caps, Frame};

fn main() {
    let mut args = std::env::args().skip(1);
    let (w, h) = args
        .next()
        .and_then(|s| {
            s.split_once('x')
                .map(|(a, b)| (a.parse().unwrap_or(1280), b.parse().unwrap_or(720)))
        })
        .unwrap_or((1280, 720));
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(90);
    let only: Option<String> = args.next();
    let probe = caps::probe();
    for cap in &probe.caps.video {
        if only.as_deref().is_some_and(|o| o != cap.video.token()) {
            continue;
        }
        if !probe.decodes.contains(&cap.video.codec()) {
            continue;
        }
        let video = cap.video;
        let arrivals = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let a = arrivals.clone();
        let dsink: etherquic_media::Sink<Frame> = Arc::new(move |_f: Frame| {
            a.lock().unwrap().push(Instant::now());
            true
        });
        let decoder = Arc::new(Mutex::new(
            VideoDecoder::open(
                video,
                w,
                h,
                dsink,
                etherquic_media::negotiate::YuvFormat::YUV420P8,
            )
            .expect("decoder"),
        ));
        let d = decoder.clone();
        let bytes = Arc::new(Mutex::new(0usize));
        let b = bytes.clone();
        let esink: etherquic_media::Sink<Vec<u8>> = Arc::new(move |rec: Vec<u8>| {
            *b.lock().unwrap() += rec.len();
            d.lock().unwrap().push(&rec).is_ok()
        });
        let setup = EncodeSetup {
            video,
            encoder: cap.encoder.clone(),
            width: w,
            height: h,
            fps: 30,
            kbps: 0,
            quality: 0,
            yuv: etherquic_media::negotiate::YuvFormat::YUV420P8,
            dirty_rects: false,
        };
        let (mut enc, _) = VideoEncoder::open(&setup, esink).expect("encoder");
        let mut pushed = Vec::new();
        let start = Instant::now();
        for i in 0..count {
            let due = start + Duration::from_millis(i as u64 * 1000 / 30);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            pushed.push(Instant::now());
            enc.push(&moving(w, h, i)).expect("push");
        }
        std::thread::sleep(Duration::from_millis(1500));
        drop(enc);
        let got = arrivals.lock().unwrap().clone();
        let mut lat: Vec<f64> = got
            .iter()
            .zip(pushed.iter())
            .map(|(g, p)| g.duration_since(*p).as_secs_f64() * 1000.0)
            .collect();
        let decoded = lat.len();
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = lat.get(lat.len() / 2).copied().unwrap_or(f64::NAN);
        let p90 = lat.get(lat.len() * 9 / 10).copied().unwrap_or(f64::NAN);
        println!(
            "{:9} {:18} decoded {decoded}/{count} latency median {med:.1} ms p90 {p90:.1} ms, {:.2} Mbit/s",
            video.token(),
            cap.encoder,
            *bytes.lock().unwrap() as f64 * 8.0 * 30.0 / count as f64 / 1e6
        );
    }
}

fn moving(w: u32, h: u32, i: usize) -> Frame {
    let mut rgb = vec![0u8; (w * h * 3) as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = (y * w as usize + x) * 3;
            rgb[p] = ((x + i * 4) & 255) as u8;
            rgb[p + 1] = ((y + i * 2) & 255) as u8;
            rgb[p + 2] = (((x ^ y) + i) & 255) as u8;
        }
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}
