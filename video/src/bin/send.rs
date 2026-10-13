//! Send a video file over QUIC-on-L2 (borderless-style pcap/Npcap).

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use etherquic::{
    install_pure_crypto, l2_client_endpoint, parse_mac, peer_to_socket_addr, priority, L2Net,
    ETHERTYPE_QUIC,
};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    install_pure_crypto();

    let args: Vec<String> = env::args().skip(1).collect();
    let mut nic: Option<String> = None;
    let mut local_mac = [0x02, 0, 0, 0, 0, 0x02];
    let mut peer_mac = None::<[u8; 6]>;
    let mut service_id = 4433u16;
    let mut file_path = None::<PathBuf>;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nic" => {
                nic = Some(args.get(i + 1).ok_or("missing --nic")?.clone());
                i += 2;
            }
            "--local-mac" => {
                local_mac = parse_mac(args.get(i + 1).ok_or("missing --local-mac")?)
                    .ok_or("bad --local-mac")?;
                i += 2;
            }
            "--peer-mac" => {
                peer_mac = Some(
                    parse_mac(args.get(i + 1).ok_or("missing --peer-mac")?)
                        .ok_or("bad --peer-mac")?,
                );
                i += 2;
            }
            "--service-id" => {
                service_id = args.get(i + 1).ok_or("missing --service-id")?.parse()?;
                i += 2;
            }
            "--file" => {
                file_path = Some(PathBuf::from(args.get(i + 1).ok_or("missing --file")?));
                i += 2;
            }
            other if !other.starts_with('-') && file_path.is_none() => {
                file_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }
    let file_path = file_path.ok_or(
        "usage: etherquic-video-send --nic IFACE --local-mac .. --peer-mac .. --file VIDEO",
    )?;
    let peer_mac = peer_mac.ok_or("missing --peer-mac")?;

    let net = L2Net::open(nic.as_deref(), 1500, ETHERTYPE_QUIC)?;
    let endpoint = l2_client_endpoint(net, local_mac, service_id)?;
    let peer = peer_to_socket_addr(peer_mac, service_id);

    eprintln!(
        "connecting over L2 to peer_mac={:02x?} service={service_id} ...",
        peer_mac
    );
    let conn = endpoint.connect(peer, "localhost")?.await?;
    eprintln!("connected, sending {} ...", file_path.display());

    let mut send = conn.open_uni().await?;
    send.set_priority(priority::VIDEO)?;
    eprintln!("stream priority video ({})", priority::VIDEO);
    let mut file = tokio::fs::File::open(&file_path).await?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    let start = Instant::now();
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await?;
        total += n as u64;
    }
    send.finish()?;
    match tokio::time::timeout(std::time::Duration::from_secs(60), send.stopped()).await {
        Ok(Ok(_)) | Ok(Err(_)) => {}
        Err(_) => eprintln!("warning: timed out waiting for stream stop"),
    }
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    eprintln!(
        "sent {total} bytes in {elapsed:.2}s ({:.2} MiB/s)",
        (total as f64 / elapsed) / (1024.0 * 1024.0)
    );
    endpoint.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(())
}
