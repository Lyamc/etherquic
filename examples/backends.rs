//! What this computer offers: media backends, GPU compute, PyroWave, and the
//! codec rows a discovery offer would carry.
//!
//! `cargo run --release --example backends`

fn main() {
    let probe = etherquic::probe_backends();
    println!(
        "libopus {} ffmpeg {} gstreamer {}",
        probe.libopus, probe.ffmpeg, probe.gstreamer
    );
    let gpu = etherquic::gpu_compute_present();
    let library = etherquic::pyrowave_library_present();
    println!(
        "gpu compute {gpu} pyrowave library {library} pyrowave ready {} pyrowave hw-encode {}",
        etherquic::pyrowave_ready(gpu, library),
        etherquic::pyrowave_hardware_encode()
    );
    #[cfg(target_os = "macos")]
    for icd in etherquic::backend::macos_pyrowave_icds() {
        println!("vulkan driver for pyrowave: {}", icd.display());
    }
    let backend = etherquic::selected_backend(&probe);
    let (video, _) = etherquic::offer_from(probe, backend, etherquic::nvidia_present(), gpu);
    for row in video {
        println!(
            "video {} gpu {} hw-encode {} hw-decode {}",
            row.id, row.gpu, row.hardware_encode, row.hardware_decode
        );
    }
}
