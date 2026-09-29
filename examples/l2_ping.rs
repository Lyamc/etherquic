fn main() {
    let mut link = etherquic::pcap_link::open(Some("enp4s0"), 0x88b6).expect("open");
    let frame = etherquic::Frame {
        dst: [0xa8, 0xa1, 0x59, 0xe3, 0xe2, 0x69],
        src: [0xd8, 0x5e, 0xd3, 0x88, 0x5e, 0x0e],
        ether_type: 0x88b6,
        dst_service: 4433,
        src_service: 4433,
        payload: vec![0xab; 1200],
    };
    let bytes = frame.encode();
    eprintln!("sending {}-byte frame x5", bytes.len());
    for i in 0..5 {
        link.send(&bytes).expect("send");
        eprintln!("sent {i}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
