//! Acceptance checks for dirty rectangles. These call the shipped offer,
//! config, plan, and encoder-argument functions.

use etherquic_media::dirty::{self, Action};
use etherquic_media::ffmpeg::{encoder_args, LONG_GOP};
use etherquic_media::host::{self, offer_wants_dirty_rects};
use etherquic_media::negotiate::{
    build_offer, DirtyRectsMode, Offer, OfferInput, StreamConfig, Video,
};
use etherquic_media::Frame;

fn rgb(width: u32, height: u32, fill: u8) -> Vec<u8> {
    vec![fill; width as usize * height as usize * 3]
}

fn args_text(name: &str) -> String {
    encoder_args(name, 30, 4_000, true).join(" ")
}

#[test]
fn dirty_rects_follow_the_offer_the_config_and_the_picture() {
    let offer = build_offer(&OfferInput {
        video_order: &[Video::Mjpeg],
        decodes: &[etherquic_media::negotiate::VideoCodec::Mjpeg],
        want_dirty_rects: true,
        ..OfferInput::default()
    });
    let encoded = offer.encode();
    let text = String::from_utf8(encoded.clone()).unwrap();
    assert!(text.contains("dirty-rects 1"), "{text}");
    let back = Offer::decode(&encoded).unwrap();
    assert_eq!(back.want_dirty_rects, Some(true));

    let mut missing = back.clone();
    missing.want_dirty_rects = None;
    let missing_text = String::from_utf8(missing.encode()).unwrap();
    assert!(
        !missing_text.lines().any(|line| line.starts_with("dirty-rects")),
        "{missing_text}"
    );
    assert_eq!(Offer::decode(&missing.encode()).unwrap().want_dirty_rects, None);
    let settings = host::dirty_rects();
    assert_eq!(offer_wants_dirty_rects(Some(&missing)), settings);
    let mut flipped = missing.clone();
    flipped.want_dirty_rects = Some(!settings);
    assert_eq!(offer_wants_dirty_rects(Some(&flipped)), !settings);

    let mut patches = StreamConfig::video(Video::Mjpeg, "jpeg", 32, 32, 30);
    patches.dirty_rects = Video::Mjpeg.dirty_rects_mode(true, true);
    assert_eq!(patches.dirty_rects, DirtyRectsMode::Patches);
    assert_eq!(
        StreamConfig::decode(&patches.encode()).unwrap().dirty_rects,
        DirtyRectsMode::Patches
    );
    let mut pskip = StreamConfig::video(Video::H264Sw, "libx264", 32, 32, 30);
    pskip.dirty_rects = Video::H264Sw.dirty_rects_mode(true, true);
    assert_eq!(
        StreamConfig::decode(&pskip.encode()).unwrap().dirty_rects.token(),
        "pskip"
    );
    let mut off = patches.clone();
    off.dirty_rects = Video::Mjpeg.dirty_rects_mode(false, true);
    assert_eq!(
        StreamConfig::decode(&off.encode()).unwrap().dirty_rects.token(),
        "off"
    );

    let prev = rgb(32, 32, 40);
    let mut next = prev.clone();
    next[0] = 200;
    assert_eq!(
        dirty::action(true, true, Video::Mjpeg, Some(&prev), &prev, 32, 32),
        Action::Skip
    );
    let Action::Patches(rects) =
        dirty::action(true, true, Video::Mjpeg, Some(&prev), &next, 32, 32)
    else {
        panic!("small motion should be type-R tiles");
    };
    let mut canvas = Frame {
        width: 32,
        height: 32,
        rgb: prev.clone(),
    };
    let full = Frame {
        width: 32,
        height: 32,
        rgb: next.clone(),
    };
    for rect in &rects {
        let (rect, tile) = dirty::crop(&full, *rect).unwrap();
        let coded = etherquic_media::jpeg::encode(&tile, 80).expect("jpeg tile");
        let payload = dirty::encode_payload(rect, &coded);
        let (got, bytes) = dirty::decode_payload(&payload).unwrap();
        assert_eq!(got, rect);
        assert!(!bytes.is_empty());
        dirty::blit(&mut canvas, &tile, rect.x, rect.y).unwrap();
    }
    assert_eq!(canvas.rgb, next, "patched canvas matches the full picture");

    assert!(matches!(
        dirty::action(true, true, Video::H264Sw, None, &next, 32, 32),
        Action::Full
    ));
    assert_eq!(
        dirty::action(true, true, Video::H264Sw, Some(&next), &next, 32, 32),
        Action::Skip
    );
    for name in ["libx264", "hevc_nvenc", "av1_nvenc"] {
        let text = args_text(name);
        assert!(text.contains("-bf 0"), "{name}: {text}");
        assert!(
            text.contains(&format!("-g {LONG_GOP}")),
            "{name}: {text}"
        );
    }
    assert!(args_text("libx264").contains("keyint=infinite"));

    let mut on = offer.clone();
    on.want_dirty_rects = Some(true);
    let mut turned_off = on.clone();
    turned_off.want_dirty_rects = Some(false);
    assert!(
        !on.same_codecs(&turned_off),
        "turning the request off reopens the encoder"
    );
    assert!(matches!(
        dirty::action(false, true, Video::Mjpeg, Some(&prev), &prev, 32, 32),
        Action::Full
    ));
    let mut echo = patches.clone();
    echo.dirty_rects = DirtyRectsMode::Off;
    assert_eq!(
        StreamConfig::decode(&echo.encode()).unwrap().dirty_rects.token(),
        "off"
    );

    assert!(
        !matches!(
            dirty::action(true, false, Video::Mjpeg, Some(&prev), &next, 32, 32),
            Action::Patches(_)
        ),
        "an old player never receives type-R"
    );
    assert!(
        !matches!(
            dirty::action(true, true, Video::Pyrowave, Some(&prev), &next, 32, 32),
            Action::Patches(_)
        ),
        "PyroWave never sends type-R"
    );
    assert!(matches!(
        dirty::action(true, true, Video::Mj2k, Some(&prev), &next, 32, 32),
        Action::Patches(_)
    ));
}
