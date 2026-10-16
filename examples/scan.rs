//! Discovery from the command line: one burst over raw Ethernet (when it
//! opens) and UDP, then each host's port, capabilities, and picture size.
//!
//! `cargo run --release --example scan -- [NIC] [OUT_DIR]` saves each
//! thumbnail as `OUT_DIR/<name>.jxl`.

use std::sync::atomic::AtomicU64;

fn main() {
    let mut args = std::env::args().skip(1);
    let nic = args.next().filter(|nic| !nic.is_empty() && nic != "-");
    let out = args.next();
    let generation = AtomicU64::new(0);
    let burst = etherquic::discover::discover_burst(nic.as_deref(), &generation, 0);
    if let Some(error) = &burst.l2_error {
        println!("raw Ethernet: {error}");
    }
    if let Some(error) = &burst.udp_error {
        println!("UDP: {error}");
    }
    if burst.found.is_empty() {
        println!("no hosts answered");
    }
    for host in &burst.found {
        let via = match (host.via_l2, host.via_udp) {
            (true, true) => "L2+UDP",
            (true, false) => "L2",
            _ => "UDP",
        };
        println!(
            "{} via {via}: ipv4 {:?} udp port {} l2 service {} driver {}",
            host.name,
            host.ipv4,
            host.l3_port,
            host.l2_service,
            etherquic::discover::l2_backend_label(host.l2_backend)
        );
        if let Some(video) = &host.video {
            for row in video {
                println!(
                    "  video id {} hw-encode {} hw-decode {} gpu {}",
                    row.id, row.hardware_encode, row.hardware_decode, row.gpu
                );
            }
        }
        if let Some(rel) = &host.release {
            println!("  {}", rel.list_line());
        }
        match &host.caps {
            Some(caps) => {
                println!(
                    "  caps: {} host {} monitors {:?} inputs {:#x} vpad {}",
                    caps.os_label,
                    caps.hostname,
                    caps.monitors
                        .iter()
                        .map(|m| format!("{}x{}", m.width, m.height))
                        .collect::<Vec<_>>(),
                    caps.inputs,
                    caps.virtual_pad
                );
                println!("  codecs: {}", caps.codecs);
            }
            None => println!("  caps: none"),
        }
        match &host.thumbnail {
            Some(jxl) => {
                println!("  thumbnail: {} bytes JPEG XL", jxl.len());
                if let Some(dir) = &out {
                    let _ = std::fs::create_dir_all(dir);
                    let path = std::path::Path::new(dir).join(format!("{}.jxl", host.name));
                    if std::fs::write(&path, jxl).is_ok() {
                        println!("  saved {}", path.display());
                    }
                }
            }
            None => println!("  thumbnail: none"),
        }
    }
}
