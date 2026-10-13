//! Client and server over one in-memory Ethernet fabric.

use std::path::PathBuf;
use std::time::Duration;

use etherquic::{
    client_endpoint, install_pure_crypto, peer_to_socket_addr, server_endpoint, Fabric,
};

const SERVER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
const SERVICE_ID: u16 = 4433;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    install_pure_crypto();

    let cert_dir = std::env::var("ETHERQUIC_CERTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("certs"));
    let cert = cert_dir.join("cert.pem");
    let key = cert_dir.join("key.pem");

    let fabric = Fabric::new(1500);
    let server = server_endpoint(fabric.clone(), SERVER_MAC, SERVICE_ID, &cert, &key)?;
    let client = client_endpoint(fabric.clone(), CLIENT_MAC, SERVICE_ID)?;
    let server_addr = peer_to_socket_addr(SERVER_MAC, SERVICE_ID);

    let server_task = tokio::spawn(async move {
        let incoming = server
            .accept()
            .await
            .expect("server endpoint closed before accept");
        let conn = incoming.await.expect("handshake");
        let (mut send, mut recv) = conn.accept_bi().await.expect("accept bi");
        let mut buf = vec![0u8; 64];
        let n = recv.read(&mut buf).await.expect("read").expect("eof");
        send.write_all(&buf[..n]).await.expect("echo");
        send.finish().unwrap();
        conn.closed().await;
    });

    let conn = tokio::time::timeout(
        Duration::from_secs(10),
        client.connect(server_addr, "localhost")?,
    )
    .await??;

    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"hello-over-ethernet").await?;
    send.finish()?;
    let mut out = vec![0u8; 64];
    let n = recv.read(&mut out).await?.expect("response");
    assert_eq!(&out[..n], b"hello-over-ethernet");

    println!(
        "ok: echoed {} bytes over in-memory Ethernet (sent={}, delivered={})",
        n,
        fabric.stats.sent.load(std::sync::atomic::Ordering::Relaxed),
        fabric
            .stats
            .delivered
            .load(std::sync::atomic::Ordering::Relaxed),
    );

    conn.close(0u32.into(), b"done");
    let _ = server_task.await;
    Ok(())
}
