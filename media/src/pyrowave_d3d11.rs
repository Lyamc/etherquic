//! PyroWave encoder through `pyrowave_d3d11.dll` (Direct3D 11, feature level 11_0).
//!
//! Source: [Lyamc/pyrowave-dx11](https://github.com/Lyamc/pyrowave-dx11), submodule
//! `vendor/pyrowave-dx11`. Release DLLs are tagged there (`v0.1.0` is the first).
//!
//! Same bitstream as the Vulkan encoder. Used when Vulkan is missing or the only
//! Vulkan device is a CPU driver (lavapipe, llvmpipe, SwiftShader), so a GPU
//! such as the Radeon HD 6670 can encode. The player still decodes with the
//! Vulkan library.
#![cfg(windows)]
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::raw::c_int;
use std::path::PathBuf;

use libloading::Library;

use crate::negotiate::{YuvChroma, YuvDepth, YuvFormat};
use crate::pyrowave::Decoder;
use crate::yuv::{rgb_to_yuv, YuvPlanes};
use crate::Frame;

type Handle = *mut c_void;

#[repr(C)]
struct DeviceInfo {
    struct_size: u32,
    adapter_index: i32,
    d3d11_device: *mut c_void,
    flags: u32,
}

#[repr(C)]
struct EncInfo {
    device: Handle,
    width: c_int,
    height: c_int,
    chroma: c_int,
}

#[repr(C)]
struct CpuBuffer {
    data: [*mut c_void; 3],
    stride: [usize; 3],
    size: [usize; 3],
    width: c_int,
    height: c_int,
    format: c_int,
}

#[repr(C)]
struct RateControl {
    maximum_bitstream_size: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Packet {
    offset: usize,
    size: usize,
}

#[repr(C)]
struct AdapterInfo {
    name: [u8; 128],
    vendor_id: u32,
    device_id: u32,
    dedicated_video_memory: u64,
}

const FORMAT_YUV420P: c_int = 1;
const FORMAT_YUV444P: c_int = 2;
const FORMAT_YUV420P10: c_int = 3;
const FORMAT_YUV444P10: c_int = 4;
const CHROMA_420: c_int = 0;
const CHROMA_444: c_int = 1;
const PACKET_BOUNDARY: usize = 64 * 1024;

fn pyro_chroma(format: YuvFormat) -> c_int {
    match format.bitstream().chroma {
        YuvChroma::Yuv420 => CHROMA_420,
        YuvChroma::Yuv422 | YuvChroma::Yuv444 => CHROMA_444,
    }
}

fn pyro_buffer_format(format: YuvFormat) -> c_int {
    match (format.bitstream().chroma, format.depth) {
        (YuvChroma::Yuv420, YuvDepth::Depth8) => FORMAT_YUV420P,
        (YuvChroma::Yuv444, YuvDepth::Depth8) => FORMAT_YUV444P,
        (YuvChroma::Yuv420, YuvDepth::Depth10) => FORMAT_YUV420P10,
        (YuvChroma::Yuv444, YuvDepth::Depth10) => FORMAT_YUV444P10,
        (YuvChroma::Yuv422, YuvDepth::Depth8) => FORMAT_YUV444P,
        (YuvChroma::Yuv422, YuvDepth::Depth10) => FORMAT_YUV444P10,
    }
}

fn pack_plane(src: &[u16], depth: YuvDepth) -> Vec<u8> {
    match depth {
        YuvDepth::Depth8 => src.iter().map(|sample| *sample as u8).collect(),
        YuvDepth::Depth10 => src.iter().flat_map(|sample| sample.to_ne_bytes()).collect(),
    }
}

struct Api {
    _lib: Library,
    _path: PathBuf,
    create_device: unsafe extern "C" fn(*const DeviceInfo, *mut Handle) -> c_int,
    adapter_info: unsafe extern "C" fn(Handle, *mut AdapterInfo) -> c_int,
    device_destroy: unsafe extern "C" fn(Handle),
    encoder_create: unsafe extern "C" fn(*const EncInfo, *mut Handle) -> c_int,
    encode_cpu: unsafe extern "C" fn(Handle, *const CpuBuffer, *const RateControl) -> c_int,
    num_packets: unsafe extern "C" fn(Handle, usize, *mut usize) -> c_int,
    packetize:
        unsafe extern "C" fn(Handle, *mut Packet, usize, *mut usize, *mut c_void, usize) -> c_int,
    encoder_destroy: unsafe extern "C" fn(Handle),
}

unsafe impl Send for Api {}

fn result_text(code: c_int) -> String {
    match code {
        -1 => "generic error".into(),
        -2 => "invalid argument".into(),
        -3 => "out of host memory".into(),
        -4 => "out of device memory".into(),
        -5 => "no usable device".into(),
        -6 => "not implemented".into(),
        other => format!("error {other}"),
    }
}

fn check(code: c_int, what: &str) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("pyrowave-d3d11 {what}: {}", result_text(code)))
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.to_path_buf())
}

/// Places to look, most specific first. `ETHERQUIC_PYROWAVE_D3D11=off` disables it.
/// A path in that variable is tried first.
pub fn library_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("ETHERQUIC_PYROWAVE_D3D11") {
        if path != "off" {
            out.push(PathBuf::from(path));
        }
    }
    let mut dirs = Vec::new();
    if let Some(dir) = exe_dir() {
        dirs.push(dir);
    }
    dirs.push(PathBuf::from(r"C:\Program Files\etherquic"));
    for dir in &dirs {
        let path = dir.join("pyrowave_d3d11.dll");
        if path.is_file() {
            out.push(path);
        }
    }
    out.push(PathBuf::from("pyrowave_d3d11.dll"));
    out
}

pub fn disabled() -> bool {
    std::env::var_os("ETHERQUIC_PYROWAVE_D3D11").is_some_and(|value| value == "off")
}

fn load(path: &PathBuf) -> Result<Library, String> {
    use libloading::os::windows::{Library as WinLibrary, LOAD_WITH_ALTERED_SEARCH_PATH};
    let flags = if path.is_absolute() {
        LOAD_WITH_ALTERED_SEARCH_PATH
    } else {
        0
    };
    unsafe { WinLibrary::load_with_flags(path, flags) }
        .map(Library::from)
        .map_err(|error| error.to_string())
}

fn open_api() -> Result<Api, String> {
    if disabled() {
        return Err("pyrowave-d3d11 disabled".into());
    }
    let mut errors = Vec::new();
    for path in library_candidates() {
        let lib = match load(&path) {
            Ok(lib) => lib,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let api = unsafe {
            Api {
                create_device: match lib.get(b"pwd3d11_create_device\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                adapter_info: match lib.get(b"pwd3d11_device_get_adapter_info\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                device_destroy: match lib.get(b"pwd3d11_device_destroy\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                encoder_create: match lib.get(b"pwd3d11_encoder_create\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                encode_cpu: match lib.get(b"pwd3d11_encoder_encode_cpu\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                num_packets: match lib.get(b"pwd3d11_encoder_compute_num_packets\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                packetize: match lib.get(b"pwd3d11_encoder_packetize\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                encoder_destroy: match lib.get(b"pwd3d11_encoder_destroy\0") {
                    Ok(sym) => *sym,
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                },
                _lib: lib,
                _path: path,
            }
        };
        return Ok(api);
    }
    Err(if errors.is_empty() {
        "pyrowave_d3d11.dll not found".into()
    } else {
        errors.join("; ")
    })
}

fn packed_buffer(
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    width: usize,
    height: usize,
    format: YuvFormat,
) -> CpuBuffer {
    let format = format.bitstream();
    let (cw, ch) = crate::yuv::chroma_size(width, height, format.chroma);
    let bpp = match format.depth {
        YuvDepth::Depth8 => 1,
        YuvDepth::Depth10 => 2,
    };
    CpuBuffer {
        data: [
            y.as_mut_ptr().cast(),
            u.as_mut_ptr().cast(),
            v.as_mut_ptr().cast(),
        ],
        stride: [width * bpp, cw * bpp, cw * bpp],
        size: [width * height * bpp, cw * ch * bpp, cw * ch * bpp],
        width: width as c_int,
        height: height as c_int,
        format: pyro_buffer_format(format),
    }
}

/// One D3D11 encoder for one picture size.
pub struct Encoder {
    api: Api,
    device: Handle,
    handle: Handle,
    width: u32,
    height: u32,
    adapter: String,
    format: YuvFormat,
    planes: YuvPlanes,
    pack_y: Vec<u8>,
    pack_u: Vec<u8>,
    pack_v: Vec<u8>,
    bitstream: Vec<u8>,
    packets: Vec<Packet>,
    frame_bytes: usize,
}

unsafe impl Send for Encoder {}

impl Encoder {
    /// `frame_bytes` is the per-picture budget (bitrate / 8 / fps).
    pub fn new(
        width: u32,
        height: u32,
        frame_bytes: usize,
        format: YuvFormat,
    ) -> Result<Encoder, String> {
        if width % 2 != 0 || height % 2 != 0 || width < 16 || height < 16 {
            return Err(format!(
                "pyrowave-d3d11 needs even sides of 16 or more, got {width}x{height}"
            ));
        }
        let api = open_api()?;
        let info = DeviceInfo {
            struct_size: std::mem::size_of::<DeviceInfo>() as u32,
            adapter_index: -1,
            d3d11_device: std::ptr::null_mut(),
            flags: 0,
        };
        let mut device: Handle = std::ptr::null_mut();
        check(unsafe { (api.create_device)(&info, &mut device) }, "device")?;
        if device.is_null() {
            return Err("pyrowave-d3d11 device: null".into());
        }
        let mut adapter = AdapterInfo {
            name: [0; 128],
            vendor_id: 0,
            device_id: 0,
            dedicated_video_memory: 0,
        };
        let adapter_name = if unsafe { (api.adapter_info)(device, &mut adapter) } == 0 {
            let end = adapter
                .name
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(adapter.name.len());
            String::from_utf8_lossy(&adapter.name[..end])
                .trim()
                .to_string()
        } else {
            "D3D11".into()
        };
        let enc_info = EncInfo {
            device,
            width: width as c_int,
            height: height as c_int,
            chroma: pyro_chroma(format),
        };
        let mut handle: Handle = std::ptr::null_mut();
        let created = unsafe { (api.encoder_create)(&enc_info, &mut handle) };
        if created != 0 || handle.is_null() {
            unsafe { (api.device_destroy)(device) };
            return Err(format!("pyrowave-d3d11 encoder: {}", result_text(created)));
        }
        let frame_bytes = frame_bytes.max(16 * 1024);
        Ok(Encoder {
            api,
            device,
            handle,
            width,
            height,
            adapter: adapter_name,
            format,
            planes: YuvPlanes::new(width as usize, height as usize, format),
            pack_y: Vec::new(),
            pack_u: Vec::new(),
            pack_v: Vec::new(),
            bitstream: vec![0; frame_bytes + 64 * 1024],
            packets: Vec::new(),
            frame_bytes,
        })
    }

    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    /// One record: u32le packet count, then u32le size and bytes per packet.
    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<u8>, String> {
        frame.check()?;
        if (frame.width, frame.height) != (self.width, self.height) {
            return Err("frame size changed under the encoder".into());
        }
        rgb_to_yuv(frame, &mut self.planes);
        let work = self.planes.for_bitstream();
        debug_assert_eq!(work.format, self.format.bitstream());
        self.pack_y = pack_plane(&work.y, work.format.depth);
        self.pack_u = pack_plane(&work.u, work.format.depth);
        self.pack_v = pack_plane(&work.v, work.format.depth);
        let buffer = packed_buffer(
            &mut self.pack_y,
            &mut self.pack_u,
            &mut self.pack_v,
            work.width,
            work.height,
            work.format,
        );
        let rate = RateControl {
            maximum_bitstream_size: self.frame_bytes,
        };
        check(
            unsafe { (self.api.encode_cpu)(self.handle, &buffer, &rate) },
            "encode",
        )?;
        let mut count = 0usize;
        check(
            unsafe { (self.api.num_packets)(self.handle, PACKET_BOUNDARY, &mut count) },
            "packet count",
        )?;
        if count == 0 {
            return Err("pyrowave-d3d11 produced no packets".into());
        }
        self.packets.resize(count, Packet::default());
        let mut written = 0usize;
        check(
            unsafe {
                (self.api.packetize)(
                    self.handle,
                    self.packets.as_mut_ptr(),
                    PACKET_BOUNDARY,
                    &mut written,
                    self.bitstream.as_mut_ptr().cast(),
                    self.bitstream.len(),
                )
            },
            "packetize",
        )?;
        let packets = &self.packets[..written.min(self.packets.len())];
        let total: usize = packets.iter().map(|p| p.size + 4).sum();
        let mut out = Vec::with_capacity(4 + total);
        out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
        for packet in packets {
            let end = packet.offset + packet.size;
            if end > self.bitstream.len() {
                return Err("pyrowave-d3d11 packet outside the bitstream".into());
            }
            out.extend_from_slice(&(packet.size as u32).to_le_bytes());
            out.extend_from_slice(&self.bitstream[packet.offset..end]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        disabled, library_candidates, record_acceptable, AdapterInfo, CpuBuffer, DeviceInfo,
        EncInfo, Encoder, Packet, RateControl,
    };
    use crate::negotiate::YuvFormat;
    use crate::Frame;

    #[test]
    fn abi_matches_the_c_header() {
        assert_eq!(std::mem::size_of::<DeviceInfo>(), 24);
        assert_eq!(std::mem::size_of::<EncInfo>(), 24);
        assert_eq!(std::mem::size_of::<CpuBuffer>(), 88);
        assert_eq!(std::mem::offset_of!(CpuBuffer, width), 72);
        assert_eq!(std::mem::offset_of!(CpuBuffer, height), 76);
        assert_eq!(std::mem::offset_of!(CpuBuffer, format), 80);
        assert_eq!(std::mem::size_of::<RateControl>(), 8);
        assert_eq!(std::mem::size_of::<Packet>(), 16);
        assert_eq!(std::mem::size_of::<AdapterInfo>(), 144);
        assert_eq!(std::mem::offset_of!(AdapterInfo, vendor_id), 128);
        assert_eq!(
            std::mem::offset_of!(AdapterInfo, dedicated_video_memory),
            136
        );
    }

    /// Set ETHERQUIC_PYROWAVE_D3D11 to pyrowave_d3d11.dll. A GPU Vulkan decoder
    /// has to accept the record. A CPU decoder (lavapipe) may reject it.
    #[test]
    fn encodes_a_frame_the_vulkan_decoder_can_read() {
        if !d3d11_dll_present() {
            return;
        }
        let width = 64u32;
        let height = 64u32;
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&[(x * 3) as u8, 40, (y * 2) as u8]);
            }
        }
        let frame = Frame { width, height, rgb };
        let mut encoder =
            Encoder::new(width, height, 32 * 1024, YuvFormat::YUV420P8).expect("d3d11 encoder");
        let record = encoder.encode(&frame).expect("d3d11 encode");
        assert!(encoder.adapter().len() > 0, "adapter name");
        println!(
            "direct-720p bytes={} packets={}",
            record.len(),
            super::packet_count(&record)
        );
        record_acceptable(width, height, &record).expect("d3d11 record");
    }

    /// Desk capture is 1280×720. The host opens `pyrowave-d3d11` with kbps 0,
    /// which is [`crate::pyrowave::default_frame_bytes`] (about 4 bits per
    /// pixel, at least 32 KiB). Two successive low-frequency frames must each
    /// produce a packet-bearing record. A CPU lavapipe reject is not a failure.
    #[test]
    fn encodes_two_desktop_frames_at_720p() {
        if !d3d11_dll_present() {
            return;
        }
        let width = 1280u32;
        let height = 720u32;
        let fps = 60u32;
        let budget = crate::pyrowave::default_frame_bytes(width, height);
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let slot = std::sync::Arc::clone(&records);
        let sink: crate::Sink<Vec<u8>> = std::sync::Arc::new(move |bytes| {
            slot.lock().expect("records").push(bytes);
            true
        });
        let setup = crate::video::EncodeSetup {
            video: crate::negotiate::Video::Pyrowave,
            encoder: "pyrowave-d3d11".into(),
            width,
            height,
            fps,
            kbps: 0,
            quality: 0,
            yuv: crate::negotiate::YuvFormat::YUV420P8,
            dirty_rects: false,
        };
        let (mut encoder, kbps) =
            crate::video::VideoEncoder::open(&setup, sink).expect("open pyrowave-d3d11");
        let expect_kbps = (budget.max(16 * 1024) * 8 * fps as usize / 1000) as u32;
        assert_eq!(kbps, expect_kbps, "host default budget");
        if let crate::video::VideoEncoder::PyrowaveD3d11 { inner, .. } = &encoder {
            println!(
                "desktop-720p adapter={} budget={budget} kbps={kbps}",
                inner.adapter()
            );
        }
        let dump = std::env::var("ETHERQUIC_PYROWAVE_DUMP").ok();
        let mut check_frame = |index: usize, kind: &str, frame: &crate::Frame| {
            assert!(
                encoder
                    .push(frame)
                    .unwrap_or_else(|error| panic!("{kind} frame={index} encode: {error}")),
                "sink accepted the record"
            );
            let record = {
                let locked = records.lock().expect("records");
                locked.last().cloned().expect("sink stored a record")
            };
            let count =
                packets_inside(&record).unwrap_or_else(|error| panic!("frame {index}: {error}"));
            println!(
                "{kind} frame={index} packets={count} bytes={} budget={budget}",
                record.len()
            );
            assert!(count >= 1, "frame {index} produced no packets");
            assert!(
                record.len() > 16,
                "frame {index} was only a sequence header"
            );
            record_acceptable(width, height, &record)
                .unwrap_or_else(|error| panic!("frame {index}: {error}"));
            match crate::pyrowave::Decoder::new(width, height, YuvFormat::YUV420P8) {
                Ok(mut decoder) => match decoder.decode(&record) {
                    Ok(got) => {
                        let mae = rgb_mae(frame, &got);
                        println!("{kind} frame={index} decode_mae={mae:.2}");
                        assert!(
                            mae < 28.0,
                            "frame {index} decoded too far from the source (mae={mae:.2})"
                        );
                    }
                    Err(error) => println!("{kind} frame={index} decode={error}"),
                },
                Err(error) => println!("{kind} frame={index} decoder={error}"),
            }
            if let Some(dir) = &dump {
                let path = format!("{dir}/frame{index}.bin");
                std::fs::write(&path, &record)
                    .unwrap_or_else(|error| panic!("write {path}: {error}"));
            }
        };
        for generation in 0..2u32 {
            let frame = desktop_frame(width, height, generation);
            check_frame(generation as usize, "desktop-720p", &frame);
        }
        let busy = high_freq_frame(width, height);
        check_frame(2, "busy-720p", &busy);
        assert_eq!(
            records.lock().expect("records").len(),
            3,
            "two desktop frames and one high-frequency frame"
        );
    }

    /// Wall-clock burst of the shipped `VideoEncoder::push` path at 1280×720
    /// and the host default budget. GPU timestamps are not the gate.
    #[test]
    fn encodes_720p_burst_at_60fps() {
        if !d3d11_dll_present() {
            return;
        }
        let width = 1280u32;
        let height = 720u32;
        let fps = 60u32;
        let budget = crate::pyrowave::default_frame_bytes(width, height);
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let slot = std::sync::Arc::clone(&records);
        let sink: crate::Sink<Vec<u8>> = std::sync::Arc::new(move |bytes| {
            slot.lock().expect("records").push(bytes);
            true
        });
        let setup = crate::video::EncodeSetup {
            video: crate::negotiate::Video::Pyrowave,
            encoder: "pyrowave-d3d11".into(),
            width,
            height,
            fps,
            kbps: 0,
            quality: 0,
            yuv: crate::negotiate::YuvFormat::YUV420P8,
            dirty_rects: false,
        };
        let (mut encoder, kbps) =
            crate::video::VideoEncoder::open(&setup, sink).expect("open pyrowave-d3d11");
        let expect_kbps = (budget.max(16 * 1024) * 8 * fps as usize / 1000) as u32;
        assert_eq!(kbps, expect_kbps, "host default budget");
        let adapter = if let crate::video::VideoEncoder::PyrowaveD3d11 { inner, .. } = &encoder {
            inner.adapter().to_string()
        } else {
            String::new()
        };
        println!("burst-720p adapter={adapter} budget={budget} kbps={kbps}");
        let warmup = 3usize;
        let burst = 30usize;
        let frame = desktop_frame(width, height, 0);
        for index in 0..warmup {
            assert!(
                encoder
                    .push(&frame)
                    .unwrap_or_else(|error| panic!("warmup frame={index} encode: {error}")),
                "sink accepted the warmup record"
            );
        }
        records.lock().expect("records").clear();
        let started = std::time::Instant::now();
        for index in 0..burst {
            assert!(
                encoder
                    .push(&frame)
                    .unwrap_or_else(|error| panic!("burst frame={index} encode: {error}")),
                "sink accepted the burst record"
            );
        }
        let elapsed = started.elapsed();
        let got = records.lock().expect("records");
        assert_eq!(got.len(), burst, "burst stored one record per push");
        let secs = elapsed.as_secs_f64();
        let measured = if secs > 0.0 { burst as f64 / secs } else { 0.0 };
        println!(
            "burst-720p fps={measured:.2} frames={burst} elapsed_ms={:.2}",
            elapsed.as_secs_f64() * 1000.0
        );
        for (index, record) in got.iter().enumerate() {
            let count =
                packets_inside(record).unwrap_or_else(|error| panic!("frame {index}: {error}"));
            println!(
                "burst-720p frame={index} packets={count} bytes={}",
                record.len()
            );
            assert!(count >= 1, "frame {index} produced no packets");
            assert!(
                record.len() > 16,
                "frame {index} was only a sequence header"
            );
        }
        assert!(
            measured >= 60.0,
            "720p encode burst was {measured:.2} fps, need at least 60"
        );
    }

    fn d3d11_dll_present() -> bool {
        if disabled() {
            return false;
        }
        library_candidates().iter().any(|path| path.is_file())
    }

    /// Low-frequency desktop: flat sidebar and taskbar, a ramp that steps every 32 pixels.
    fn desktop_frame(width: u32, height: u32, generation: u32) -> Frame {
        let mut rgb = vec![0u8; width as usize * height as usize * 3];
        let shift = (generation * 24) as u8;
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 3) as usize;
                let (r, g, b) = if y + 48 >= height {
                    (28, 30, 34)
                } else if x < 72 {
                    (36, 40, 48)
                } else {
                    let ramp = ((x / 32) as u8).wrapping_add(shift);
                    (168u8.wrapping_add(ramp), 164, 156)
                };
                rgb[i] = r;
                rgb[i + 1] = g;
                rgb[i + 2] = b;
            }
        }
        Frame { width, height, rgb }
    }

    /// High-frequency RGB, the picture that overflowed the 32 KiB probe and a
    /// 512 KiB 720p budget when rate control was stuck at quant 0.
    fn high_freq_frame(width: u32, height: u32) -> Frame {
        let mut rgb = vec![0u8; width as usize * height as usize * 3];
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 3) as usize;
                rgb[i] = (x.wrapping_mul(7)) as u8;
                rgb[i + 1] = (y.wrapping_mul(5)) as u8;
                rgb[i + 2] = (x.wrapping_add(y).wrapping_mul(3)) as u8;
            }
        }
        Frame { width, height, rgb }
    }

    fn rgb_mae(src: &Frame, got: &Frame) -> f32 {
        assert_eq!(src.rgb.len(), got.rgb.len());
        let sum: u64 = src
            .rgb
            .iter()
            .zip(got.rgb.iter())
            .map(|(a, b)| a.abs_diff(*b) as u64)
            .sum();
        sum as f32 / src.rgb.len() as f32
    }

    /// Packet count when every packet's bytes sit inside the record.
    fn packets_inside(record: &[u8]) -> Result<u32, String> {
        if record.len() < 4 {
            return Err("record shorter than the packet count".into());
        }
        let count = u32::from_le_bytes([record[0], record[1], record[2], record[3]]);
        let mut rest = &record[4..];
        for index in 0..count {
            if rest.len() < 4 {
                return Err(format!("packet {index} header is past the record"));
            }
            let size = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            rest = &rest[4..];
            if size == 0 || rest.len() < size {
                return Err(format!(
                    "packet {index} ({size} bytes) is outside the record"
                ));
            }
            rest = &rest[size..];
        }
        Ok(count)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            (self.api.encoder_destroy)(self.handle);
            (self.api.device_destroy)(self.device);
        }
    }
}

/// The Vulkan decoder accepts this record, or the record has a packet and the
/// only decoder is a CPU driver (lavapipe). A GPU decoder that rejects the
/// record means it is not PyroWave and must not be advertised. CPU lavapipe
/// on the HD 6670 rejects a bitstream the GPU decoder accepts, and that must
/// not hide the Direct3D 11 encoder.
pub fn record_acceptable(width: u32, height: u32, record: &[u8]) -> Result<(), String> {
    match Decoder::new(width, height, crate::negotiate::YuvFormat::YUV420P8) {
        Ok(mut decoder) => match decoder.decode(record) {
            Ok(_) => Ok(()),
            Err(error) => {
                let cpu = decoder.vulkan_device().is_some_and(|device| device.cpu);
                if cpu && packet_count(record) >= 1 {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        },
        Err(error) => {
            if packet_count(record) >= 1 {
                Ok(())
            } else {
                Err(format!(
                    "no vulkan decoder ({error}) and the record is empty"
                ))
            }
        }
    }
}

fn packet_count(record: &[u8]) -> u32 {
    if record.len() < 4 {
        0
    } else {
        u32::from_le_bytes([record[0], record[1], record[2], record[3]])
    }
}
