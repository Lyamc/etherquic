//! `etherquic-play host` shares this desktop. `etherquic-play connect` views it.

#![forbid(unsafe_code)]

mod args;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use etherquic::{L2Net, ETHERTYPE_QUIC};

fn main() {
    if let Err(error) = dispatch() {
        eprintln!("etherquic-play: {error}");
        std::process::exit(1);
    }
}

fn dispatch() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Err(error) = etherquic::capture_from_env() {
        eprintln!("etherquic-play: capture: {error}");
    }
    let _capture = StopCapture;
    let stop = Arc::new(AtomicBool::new(false));
    watch_ctrl_c(Arc::clone(&stop));
    match args::parse(std::env::args().skip(1))? {
        args::Command::Host(host) => {
            let net = L2Net::open(host.nic.as_deref(), 1500, ETHERTYPE_QUIC)?;
            etherquic_play::host_desktop(
                net,
                etherquic_play::HostSession {
                    local_mac: host.local_mac,
                    service_id: host.service_id,
                    max_width: host.max_width,
                    fps: host.fps,
                    cert: host.cert,
                    key: host.key,
                },
                stop,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicUsize::new(0)),
                etherquic_play::stderr_status(),
            )
        }
        args::Command::Connect(connect) => {
            let net = L2Net::open(connect.nic.as_deref(), 1500, ETHERTYPE_QUIC)?;
            etherquic_play::connect_view(
                Some(net),
                etherquic_play::ConnectSession {
                    local_mac: connect.local_mac,
                    peer_mac: connect.peer_mac,
                    service_id: connect.service_id,
                    trace_input: false,
                    udp: None,
                },
                stop,
                etherquic_play::stderr_status(),
            )
        }
    }
}

struct StopCapture;

impl Drop for StopCapture {
    fn drop(&mut self) {
        let _ = etherquic::capture_stop();
    }
}

/// Ctrl-C sets the flag the desktop session already watches.
fn watch_ctrl_c(stop: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("etherquic-play-signal".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            runtime.block_on(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    stop.store(true, Ordering::SeqCst);
                }
            });
        });
}
