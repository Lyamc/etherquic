//! Send Opus audio over QUIC-on-L2 as LOC objects.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use etherquic::{
    install_pure_crypto, l2_client_endpoint, parse_mac, peer_to_socket_addr, priority, L2Net,
    ETHERTYPE_QUIC,
};
use etherquic_audio::{load_as, store, summary, VirtualSound, DEFAULT_CABLE, DEFAULT_SERVICE_ID};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    install_pure_crypto();

    let args: Vec<String> = env::args().skip(1).collect();
    let mut nic: Option<String> = None;
    let mut local_mac = [0x02, 0, 0, 0, 0, 0x02];
    let mut peer_mac = None::<[u8; 6]>;
    let mut service_id = DEFAULT_SERVICE_ID;
    let mut file_path = None::<PathBuf>;
    let mut virtual_path = None::<PathBuf>;
    let mut format = None::<String>;
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
            "--format" => {
                format = Some(args.get(i + 1).ok_or("missing --format")?.clone());
                i += 2;
            }
            "--file" => {
                file_path = Some(PathBuf::from(args.get(i + 1).ok_or("missing --file")?));
                i += 2;
            }
            "--virtual" => {
                virtual_path = Some(virtual_arg(&args, &mut i)?);
            }
            other if !other.starts_with('-') && file_path.is_none() => {
                file_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }
    if file_path.is_some() && virtual_path.is_some() {
        return Err("pass either --file or --virtual".into());
    }
    let peer_mac = peer_mac.ok_or("missing --peer-mac")?;
    let source = match (file_path, virtual_path) {
        (Some(path), None) => Source::File(path),
        (None, Some(path)) => Source::Virtual(path),
        (None, None) => {
            return Err(
                "usage: etherquic-audio-send --nic IFACE --local-mac .. --peer-mac .. --file AUDIO.opus"
                    .into(),
            );
        }
        (Some(_), Some(_)) => unreachable!(),
    };

    let (bytes, label) = match &source {
        Source::File(path) => (std::fs::read(path)?, path.display().to_string()),
        Source::Virtual(path) => {
            eprintln!("capturing virtual sound from {} ...", path.display());
            (
                VirtualSound::capture_file(path, std::time::Duration::from_secs(60))?,
                path.display().to_string(),
            )
        }
    };
    let objects = load_as(&bytes, format.as_deref())?;
    let wire = store(&objects, true)?;
    eprintln!(
        "{} -> {} LOC bytes from {}",
        summary(&objects),
        wire.len(),
        label
    );

    let net = L2Net::open(nic.as_deref(), 1500, ETHERTYPE_QUIC)?;
    let endpoint = l2_client_endpoint(net, local_mac, service_id)?;
    let peer = peer_to_socket_addr(peer_mac, service_id);

    eprintln!(
        "connecting over L2 to peer_mac={:02x?} service={service_id} ...",
        peer_mac
    );
    let conn = endpoint.connect(peer, "localhost")?.await?;
    eprintln!("connected, sending {} LOC object(s) ...", objects.len());

    let mut send = conn.open_uni().await?;
    send.set_priority(priority::AUDIO)?;
    eprintln!("stream priority audio ({})", priority::AUDIO);
    let start = Instant::now();
    send.write_all(&wire).await?;
    send.finish()?;
    match tokio::time::timeout(std::time::Duration::from_secs(60), send.stopped()).await {
        Ok(Ok(_)) | Ok(Err(_)) => {}
        Err(_) => eprintln!("warning: timed out waiting for stream stop"),
    }
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    eprintln!(
        "sent {} bytes in {elapsed:.2}s ({:.2} MiB/s)",
        wire.len(),
        (wire.len() as f64 / elapsed) / (1024.0 * 1024.0)
    );
    endpoint.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(())
}

enum Source {
    File(PathBuf),
    Virtual(PathBuf),
}

fn virtual_arg(
    args: &[String],
    i: &mut usize,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(value) = args.get(*i + 1) {
        if !value.starts_with('-') {
            *i += 2;
            return Ok(PathBuf::from(value));
        }
    }
    *i += 1;
    Ok(PathBuf::from(DEFAULT_CABLE))
}
