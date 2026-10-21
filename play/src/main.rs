//! `etherquic-play host` shares this desktop. `etherquic-play connect` views it.

#![forbid(unsafe_code)]

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
    match etherquic_play::args::parse(std::env::args().skip(1))? {
        etherquic_play::args::Command::Host(host) => {
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
                    audio_device: None,
                },
                stop,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicUsize::new(0)),
                etherquic_play::stderr_status(),
            )
        }
        etherquic_play::args::Command::Connect(connect) => {
            if let Some(profile) = connect.profile.as_deref() {
                let plain = profile == "plain";
                etherquic::carrier::choose_profile(plain, !plain);
            }
            let view = etherquic_play::live(connect.view);
            let tune = etherquic_play::live_tune(etherquic_play::SessionTune::default());
            if connect.parent_pipe {
                etherquic_play::parent_ctl::watch_stdin(Arc::clone(&stop), view.clone(), tune.clone());
            }
            let net = etherquic::carrier::connect_net(
                connect.udp,
                connect.l2_offer,
                connect.nic.as_deref(),
                |nic| L2Net::open(nic, 1500, ETHERTYPE_QUIC),
            )?;
            etherquic_play::connect_view(
                net,
                etherquic_play::ConnectSession {
                    local_mac: connect.local_mac,
                    peer_mac: connect.peer_mac,
                    service_id: connect.service_id,
                    trace_input: connect.trace_input,
                    udp: connect.udp,
                    l2_offer: connect.l2_offer,
                    rtp: connect.rtp,
                    view,
                    tune,
                    codecs: connect.codecs,
                    picture: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
