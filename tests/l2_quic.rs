//! End-to-end: Quinn handshake + bidirectional stream over the Ethernet fabric.

use std::path::PathBuf;
use std::time::Duration;

use etherquic::{
    client_endpoint, install_pure_crypto, peer_to_socket_addr, server_endpoint, Fabric, Frame,
    DEFAULT_ETHER_TYPE,
};

const SERVER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x10];
const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x20];
const SERVICE_ID: u16 = 4433;

fn cert_paths() -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("certs");
    (root.join("cert.pem"), root.join("key.pem"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_and_bidi_echo() {
    install_pure_crypto();
    let (cert, key) = cert_paths();
    let fabric = Fabric::new(1500);

    let server = server_endpoint(fabric.clone(), SERVER_MAC, SERVICE_ID, &cert, &key)
        .expect("server endpoint");
    let client = client_endpoint(fabric.clone(), CLIENT_MAC, SERVICE_ID).expect("client endpoint");
    let server_addr = peer_to_socket_addr(SERVER_MAC, SERVICE_ID);

    let server_task = tokio::spawn(async move {
        let incoming = tokio::time::timeout(Duration::from_secs(10), server.accept())
            .await
            .expect("accept timeout")
            .expect("endpoint closed");
        let conn = incoming.await.expect("server handshake");
        let (mut send, mut recv) = conn.accept_bi().await.expect("accept bi");
        let mut buf = vec![0u8; 128];
        let n = recv.read(&mut buf).await.expect("read").expect("data");
        send.write_all(&buf[..n]).await.expect("echo write");
        send.finish().unwrap();
        // Keep the connection alive until the client finishes reading.
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.close(0u32.into(), b"bye");
    });

    let connecting = client
        .connect(server_addr, "localhost")
        .expect("start connect");
    let conn = tokio::time::timeout(Duration::from_secs(10), connecting)
        .await
        .expect("connect timeout")
        .expect("client handshake");

    let (mut send, mut recv) = conn.open_bi().await.expect("open bi");
    let payload = b"quic-on-ethernet";
    send.write_all(payload).await.expect("write");
    send.finish().unwrap();
    let mut out = vec![0u8; 128];
    let n = recv.read(&mut out).await.expect("read echo").expect("echo");
    assert_eq!(&out[..n], payload);

    assert!(
        fabric
            .stats
            .sent
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
    assert_eq!(
        fabric
            .stats
            .dropped_too_big
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    let _ = server_task.await;
    conn.close(0u32.into(), b"done");
}

#[test]
fn frame_preserves_quic_bytes() {
    let payload = vec![0xC0u8; 1200]; // Initial-sized opaque blob
    let frame = Frame {
        dst: SERVER_MAC,
        src: CLIENT_MAC,
        ether_type: DEFAULT_ETHER_TYPE,
        dst_service: SERVICE_ID,
        src_service: SERVICE_ID,
        payload: payload.clone(),
    };
    let bytes = frame.encode();
    let decoded = Frame::decode_filtered(&bytes, DEFAULT_ETHER_TYPE).unwrap();
    assert_eq!(decoded.payload, payload);
}
