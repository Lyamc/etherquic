//! LOC objects for an Opus track survive a QUIC handshake on the in-memory fabric.

use std::path::PathBuf;
use std::time::Duration;

use etherquic::{
    client_endpoint, install_pure_crypto, peer_to_socket_addr, server_endpoint, Fabric,
};
use etherquic_audio::{
    demux_opus, load, mux_opus, opus_id_header, parse_id, store, OpusPacket, OpusStream,
    DEFAULT_SERVICE_ID,
};

const SERVER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

#[tokio::test]
async fn opus_loc_objects_cross_the_fabric() -> Result<(), Box<dyn std::error::Error>> {
    install_pure_crypto();
    let original = OpusStream {
        id: parse_id(&opus_id_header(1, 120, 48_000))?,
        packets: vec![
            OpusPacket {
                data: vec![0x98],
                start_granule: 0,
                samples: 960,
            },
            OpusPacket {
                data: vec![0x98],
                start_granule: 960,
                samples: 960,
            },
        ],
        end_trim: 0,
    };
    let wire = store(&load(&mux_opus(&original)?)?, true)?;

    let cert = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/cert.pem");
    let key = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/key.pem");
    let fabric = Fabric::new(1500);
    let server = server_endpoint(fabric.clone(), SERVER_MAC, DEFAULT_SERVICE_ID, &cert, &key)?;
    let client = client_endpoint(fabric, CLIENT_MAC, DEFAULT_SERVICE_ID)?;
    let server_addr = peer_to_socket_addr(SERVER_MAC, DEFAULT_SERVICE_ID);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        let conn = incoming.await.expect("handshake");
        let mut recv = conn.accept_uni().await.expect("uni");
        let mut got = Vec::new();
        let mut buf = vec![0u8; 2048];
        loop {
            match recv.read(&mut buf).await.expect("read") {
                Some(n) if n > 0 => got.extend_from_slice(&buf[..n]),
                Some(_) => {}
                None => break,
            }
        }
        got
    });

    let conn = tokio::time::timeout(
        Duration::from_secs(10),
        client.connect(server_addr, "localhost")?,
    )
    .await??;
    let mut send = conn.open_uni().await?;
    send.write_all(&wire).await?;
    send.finish()?;
    let _ = tokio::time::timeout(Duration::from_secs(10), send.stopped()).await;

    let got = tokio::time::timeout(Duration::from_secs(10), server_task).await??;
    let restored = demux_opus(&store(&load(&got)?, false)?)?;
    assert_eq!(restored.packets, original.packets);
    conn.close(0u32.into(), b"done");
    Ok(())
}
