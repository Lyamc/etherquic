//! A consumer that is not the `avtp`, `avb`, or `st2110` unit tests.

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let cases: [(&str, &[u8], &[u8]); 2] = [
        ("media", &[0xFF, 0xD8, 0xFF, 0xD9], &[0x03, 0xE8, 0xFC, 0x18]),
        ("record", b"picture-bytes", b"audio-record"),
    ];
    for (run, (kind, picture, audio)) in cases.into_iter().enumerate() {
        let run = run + 1;
        let pic = avb::picture_frame(
            [0x02, 0, 0, 0, 0, 2],
            [0x02, 0, 0, 0, 0, 1],
            1,
            run as u8,
            1_500,
            picture,
            None,
            run == 2,
        )
        .expect("picture frame");
        let aud = if kind == "media" {
            let samples = [1000i16, -1000];
            avb::pcm_frame(
                [0x02, 0, 0, 0, 0, 2],
                [0x02, 0, 0, 0, 0, 1],
                2,
                run as u8,
                48_000,
                2,
                &samples,
                None,
            )
            .expect("pcm frame")
        } else {
            avb::audio_frame(
                [0x02, 0, 0, 0, 0, 2],
                [0x02, 0, 0, 0, 0, 1],
                2,
                run as u8,
                48_000,
                audio,
                None,
            )
            .expect("audio frame")
        };
        let pic_back = avb::decode_pdu(avb::payload_of(&pic.ethernet).expect("picture pdu"))
            .expect("picture decode");
        let aud_back = avb::decode_pdu(avb::payload_of(&aud.ethernet).expect("audio pdu"))
            .expect("audio decode");
        let audio_in = if kind == "media" {
            aud.payload.clone()
        } else {
            audio.to_vec()
        };
        assert_eq!(pic_back.payload, picture);
        assert_eq!(aud_back.payload, audio_in);
        assert_eq!(avb::ethertype_of(&pic.ethernet), Some(0x22F0));
        assert_eq!(avb::ethertype_of(&aud.ethernet), Some(0x22F0));
        if kind == "media" {
            assert_eq!(pic.subtype, avb::SUBTYPE_CVF);
            assert_eq!(avb::cvf_codec(&pic.payload), Some(avb::CvfCodec::Mjpeg));
            assert_eq!(aud.subtype, avb::SUBTYPE_AAF);
        } else {
            assert_eq!(pic.subtype, avb::SUBTYPE_EXPERIMENTAL);
            assert_eq!(aud.subtype, avb::SUBTYPE_EXPERIMENTAL);
            assert!(avb::cvf_codec(picture).is_none());
        }
        println!(
            "run {run} kind={kind} ethertype={:#06x} picture_subtype={:#x} audio_subtype={:#x} picture_identical={} audio_identical={} picture_in={} picture_out={} audio_in={} audio_out={}",
            avb::ethertype_of(&pic.ethernet).unwrap(),
            pic.subtype,
            aud.subtype,
            pic_back.payload == picture,
            aud_back.payload == audio_in,
            hex(picture),
            hex(&pic_back.payload),
            hex(&audio_in),
            hex(&aud_back.payload)
        );
    }

    let mut samples = vec![0u8; 64 * 64 * 3];
    for (i, byte) in samples.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    let packets = st2110::packetize_rfc4175(&samples, 64, 64, 1_500);
    let mut back = vec![0u8; samples.len()];
    let mut times = Vec::new();
    for packet in &packets {
        let (row, row_samples, _, _) = st2110::rfc4175_samples(&packet.rtp).expect("rfc4175");
        let start = row as usize * 64 * 3;
        assert_eq!(
            back[start..start + row_samples.len()].iter().all(|b| *b == 0),
            true,
            "row {row} written twice"
        );
        back[start..start + row_samples.len()].copy_from_slice(&row_samples);
        times.push(packet.send_at);
    }
    assert_eq!(back, samples, "raster covered once");
    assert!(
        times.windows(2).all(|pair| pair[1] != pair[0]),
        "send times are not distinct"
    );
    println!(
        "st2110 rows={} first={:?} last={:?}",
        packets.len(),
        times.first(),
        times.last()
    );
}
