//! A consumer that is not the `avb` or `st2110` unit tests.

fn main() {
    let frame = avb::picture_frame(
        [0x02, 0, 0, 0, 0, 2],
        [0x02, 0, 0, 0, 0, 1],
        1,
        1,
        1_500,
        b"cvf-sample",
        None,
        true,
    )
    .expect("cvf frame");
    let ethertype = avb::ethertype_of(&frame.ethernet).expect("ethertype");
    let subtype = frame.ethernet.get(14).copied().expect("subtype");
    assert_eq!(ethertype, 0x22F0, "EtherType");
    assert!(
        subtype == avb::SUBTYPE_CVF || subtype == 0x07,
        "subtype {subtype:#x} is not CVF or RVF"
    );
    assert_ne!(subtype, 0x7F, "experimental subtype");

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
        "ok ethertype={ethertype:#06x} subtype={subtype:#x} rows={} first={:?} last={:?}",
        packets.len(),
        times.first(),
        times.last()
    );
}
