//! Print what this computer encodes and decodes, and time a round trip of
//! each working codec. `eqmedia-probe [WIDTHxHEIGHT] [FRAMES]`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use etherquic_media::negotiate::Video;
use etherquic_media::video::{EncodeSetup, VideoDecoder, VideoEncoder};
use etherquic_media::{caps, Frame};

fn main() {
    let mut args = std::env::args().skip(1);
    let (w, h) = args
        .next()
        .and_then(|s| s.split_once('x').map(|(a, b)| (a.parse().unwrap_or(1280), b.parse().unwrap_or(720))))
        .unwrap_or((1280, 720));
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let started = Instant::now();
    let probe = caps::probe();
    println!("probe took {:?}", started.elapsed());
    println!("{}", caps::summary(probe));
    if let Some(path) = etherquic_media::pyrowave::library_path() {
        println!("pyrowave library {}", path.display());
    }
    if let Some(path) = etherquic_media::opus::library_path() {
        println!("opus library {}", path.display());
    }
    for cap in &probe.caps.video {
        let video = cap.video;
        let can_decode = probe.decodes.contains(&video.codec());
        let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let rx = received.clone();
        let setup = EncodeSetup { video, encoder: cap.encoder.clone(), width: w, height: h, fps: 30, kbps: 0 };
        let sink: etherquic_media::Sink<Vec<u8>> = Arc::new(move |b: Vec<u8>| {
            rx.lock().unwrap().push(b);
            true
        });
        let (mut enc, kbps) = match VideoEncoder::open(&setup, sink) {
            Ok(x) => x,
            Err(e) => {
                println!("{}: open failed: {e}", video.token());
                continue;
            }
        };
        let t = Instant::now();
        for i in 0..count {
            let frame = moving(w, h, i);
            if let Err(e) = enc.push(&frame) {
                println!("{}: push failed: {e}", video.token());
                break;
            }
        }
        let enc_time = t.elapsed();
        if video != Video::Mjpeg && video != Video::Pyrowave {
            std::thread::sleep(Duration::from_millis(800));
        }
        drop(enc);
        let records = received.lock().unwrap().clone();
        let bytes: usize = records.iter().map(|r| r.len()).sum();
        let mut decoded = 0usize;
        if can_decode {
            let got = Arc::new(Mutex::new(0usize));
            let g = got.clone();
            let sink: etherquic_media::Sink<Frame> = Arc::new(move |_f: Frame| {
                *g.lock().unwrap() += 1;
                true
            });
            match VideoDecoder::open(video, w, h, sink) {
                Ok(mut dec) => {
                    for r in &records {
                        if let Err(e) = dec.push(r) {
                            println!("{}: decode failed: {e}", video.token());
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1500));
                    drop(dec);
                }
                Err(e) => println!("{}: decoder open failed: {e}", video.token()),
            }
            decoded = *got.lock().unwrap();
        }
        println!(
            "{:9} {:18} {count} frames {w}x{h}: encode {:.1} ms/frame, {:.2} Mbit/s at 30 fps (target {kbps} kbit/s), decoded {decoded}",
            video.token(),
            cap.encoder,
            enc_time.as_secs_f64() * 1000.0 / count as f64,
            bytes as f64 * 8.0 * 30.0 / count as f64 / 1e6,
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
    Frame { width: w, height: h, rgb }
}
