//! Receive a video file over QUIC-on-L2 (borderless-style pcap/Npcap) and write it to disk.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use etherquic::{
    install_pure_crypto, l2_server_endpoint, parse_mac, L2Net, ETHERTYPE_QUIC,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    install_pure_crypto();

    let args: Vec<String> = env::args().skip(1).collect();
    let mut nic: Option<String> = None;
    let mut local_mac = [0x02, 0, 0, 0, 0, 0x01];
    let mut service_id = 4433u16;
    let mut out = PathBuf::from("received.mp4");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nic" => {
                nic = Some(args.get(i + 1).ok_or("missing --nic value")?.clone());
                i += 2;
            }
            "--local-mac" => {
                local_mac = parse_mac(args.get(i + 1).ok_or("missing --local-mac")?)
                    .ok_or("bad --local-mac")?;
                i += 2;
            }
            "--service-id" => {
                service_id = args.get(i + 1).ok_or("missing --service-id")?.parse()?;
                i += 2;
            }
            "--out" => {
                out = PathBuf::from(args.get(i + 1).ok_or("missing --out")?);
                i += 2;
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }

    let cert_dir = PathBuf::from(env::var("ETHERQUIC_CERTS").unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../certs")
            .display()
            .to_string()
    }));

    let net = L2Net::open(nic.as_deref(), 1500, ETHERTYPE_QUIC)?;
    let endpoint = l2_server_endpoint(
        net,
        local_mac,
        service_id,
        &cert_dir.join("cert.pem"),
        &cert_dir.join("key.pem"),
    )?;

    eprintln!(
        "etherquic-video-recv L2 local_mac={:02x?} service={service_id} -> {}",
        local_mac,
        out.display()
    );
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(format!("{}.ready", out.display()), b"ready\n")?;

    let incoming = endpoint
        .accept()
        .await
        .ok_or("endpoint closed before accept")?;
    let conn = incoming.await?;
    eprintln!("accepted from {:?}", conn.remote_address());

    let mut recv = conn.accept_uni().await?;
    let mut file = tokio::fs::File::create(&out).await?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    let start = Instant::now();
    loop {
        match recv.read(&mut buf).await? {
            Some(n) => {
                use tokio::io::AsyncWriteExt;
                file.write_all(&buf[..n]).await?;
                total += n as u64;
            }
            None => break,
        }
    }
    use tokio::io::AsyncWriteExt;
    file.flush().await?;
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    eprintln!(
        "received {total} bytes in {elapsed:.2}s ({:.2} MiB/s) -> {}",
        (total as f64 / elapsed) / (1024.0 * 1024.0),
        out.display()
    );
    std::fs::write(format!("{}.done", out.display()), format!("{total}\n"))?;
    conn.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(())
}
