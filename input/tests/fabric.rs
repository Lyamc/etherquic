//! Input events survive a QUIC handshake on the in-memory fabric,
//! and the send stream priority reads back as input, above audio and video.

use std::path::PathBuf;
use std::time::Duration;

use etherquic::{
    client_endpoint, install_pure_crypto, peer_to_socket_addr, server_endpoint, Fabric,
    PRIORITY_AUDIO, PRIORITY_INPUT, PRIORITY_VIDEO,
};
use etherquic_input::{
    decode, encode, Axes, Body, Buttons, Event, KeySource, DEFAULT_SERVICE_ID, PAD_SOUTH, SYNC_CAPS,
};

const SERVER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

#[tokio::test]
async fn input_events_cross_the_fabric_at_input_priority() -> Result<(), Box<dyn std::error::Error>>
{
    install_pure_crypto();
    let original = vec![
        Event {
            time_us: 1,
            body: Body::Key {
                down: true,
                source: KeySource::Hid,
                code: 0x0007_0004,
            },
        },
        Event {
            time_us: 2,
            body: Body::Pointer {
                x: 100,
                y: 200,
                buttons: Buttons::LEFT,
                wheel_y: 1,
                wheel_x: 0,
            },
        },
        Event {
            time_us: 3,
            body: Body::Delta {
                dx: 4,
                dy: -5,
                buttons: 0,
                wheel_y: 0,
                wheel_x: 0,
            },
        },
        Event {
            time_us: 4,
            body: Body::Gamepad {
                index: 0,
                buttons: PAD_SOUTH,
                axes: Axes {
                    lx: 10,
                    ly: -10,
                    rx: 0,
                    ry: 0,
                    lt: 100,
                    rt: 0,
                },
            },
        },
        Event {
            time_us: 5,
            body: Body::Sync { flags: SYNC_CAPS },
        },
        Event {
            time_us: 6,
            body: Body::Unknown {
                kind: 40,
                body: vec![9, 8, 7],
            },
        },
    ];
    let wire = encode(&original)?;

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
    send.set_priority(PRIORITY_INPUT)?;
    assert_eq!(send.priority()?, PRIORITY_INPUT);
    assert!(PRIORITY_INPUT > PRIORITY_AUDIO);
    assert!(PRIORITY_AUDIO > PRIORITY_VIDEO);
    send.write_all(&wire).await?;
    send.finish()?;
    let _ = tokio::time::timeout(Duration::from_secs(10), send.stopped()).await;

    let got = tokio::time::timeout(Duration::from_secs(10), server_task).await??;
    assert_eq!(decode(&got)?, original);
    conn.close(0u32.into(), b"done");
    Ok(())
}
