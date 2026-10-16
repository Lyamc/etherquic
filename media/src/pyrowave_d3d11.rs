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

use crate::pyrowave::Decoder;
use crate::yuv::{rgb_to_i420, I420};
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
const CHROMA_420: c_int = 0;
const PACKET_BOUNDARY: usize = 64 * 1024;

struct Api {
    _lib: Library,
    _path: PathBuf,
    create_device: unsafe extern "C" fn(*const DeviceInfo, *mut Handle) -> c_int,
    adapter_info: unsafe extern "C" fn(Handle, *mut AdapterInfo) -> c_int,
    device_destroy: unsafe extern "C" fn(Handle),
    encoder_create: unsafe extern "C" fn(*const EncInfo, *mut Handle) -> c_int,
    encode_cpu: unsafe extern "C" fn(Handle, *const CpuBuffer, *const RateControl) -> c_int,
    num_packets: unsafe extern "C" fn(Handle, usize, *mut usize) -> c_int,
    packetize: unsafe extern "C" fn(Handle, *mut Packet, usize, *mut usize, *mut c_void, usize) -> c_int,
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
    std::env::current_exe().ok()?.parent().map(|dir| dir.to_path_buf())
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
    let flags = if path.is_absolute() { LOAD_WITH_ALTERED_SEARCH_PATH } else { 0 };
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

fn planes_buffer(planes: &mut I420) -> CpuBuffer {
    let w = planes.width;
    let h = planes.height;
    CpuBuffer {
        data: [
            planes.y.as_mut_ptr().cast(),
            planes.u.as_mut_ptr().cast(),
            planes.v.as_mut_ptr().cast(),
        ],
        stride: [w, w / 2, w / 2],
        size: [w * h, (w / 2) * (h / 2), (w / 2) * (h / 2)],
        width: w as c_int,
        height: h as c_int,
        format: FORMAT_YUV420P,
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
    planes: I420,
    bitstream: Vec<u8>,
    packets: Vec<Packet>,
    frame_bytes: usize,
}

unsafe impl Send for Encoder {}

impl Encoder {
    /// `frame_bytes` is the per-picture budget (bitrate / 8 / fps).
    pub fn new(width: u32, height: u32, frame_bytes: usize) -> Result<Encoder, String> {
        if width % 2 != 0 || height % 2 != 0 || width < 16 || height < 16 {
            return Err(format!("pyrowave-d3d11 needs even sides of 16 or more, got {width}x{height}"));
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
            let end = adapter.name.iter().position(|b| *b == 0).unwrap_or(adapter.name.len());
            String::from_utf8_lossy(&adapter.name[..end]).trim().to_string()
        } else {
            "D3D11".into()
        };
        let enc_info = EncInfo {
            device,
            width: width as c_int,
            height: height as c_int,
            chroma: CHROMA_420,
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
            planes: I420::new(width as usize, height as usize),
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
        rgb_to_i420(frame, &mut self.planes);
        let buffer = planes_buffer(&mut self.planes);
        let rate = RateControl { maximum_bitstream_size: self.frame_bytes };
        check(unsafe { (self.api.encode_cpu)(self.handle, &buffer, &rate) }, "encode")?;
        let mut count = 0usize;
        check(unsafe { (self.api.num_packets)(self.handle, PACKET_BOUNDARY, &mut count) }, "packet count")?;
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
    use super::*;

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
        assert_eq!(std::mem::offset_of!(AdapterInfo, dedicated_video_memory), 136);
    }

    /// Set ETHERQUIC_PYROWAVE_D3D11 to pyrowave_d3d11.dll. When libpyrowave
    /// is installed, the Vulkan decoder has to accept the record.
    #[test]
    fn encodes_a_frame_the_vulkan_decoder_can_read() {
        let Some(path) = std::env::var_os("ETHERQUIC_PYROWAVE_D3D11") else {
            return;
        };
        if path == "off" || !PathBuf::from(&path).is_file() {
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
        let mut encoder = Encoder::new(width, height, 32 * 1024).expect("d3d11 encoder");
        let record = encoder.encode(&frame).expect("d3d11 encode");
        assert!(encoder.adapter().len() > 0, "adapter name");
        match Decoder::new(width, height) {
            Ok(mut decoder) => decoder.decode(&record).expect("vulkan decode of the d3d11 record"),
            Err(error) => panic!("libpyrowave did not load, so the bitstream was not checked: {error}"),
        };
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

/// The Vulkan decoder accepts this record, or no Vulkan library is installed
/// and the record at least has a packet header. A failed decode means the
/// D3D11 bitstream is not PyroWave and must not be advertised.
pub fn record_acceptable(width: u32, height: u32, record: &[u8]) -> Result<(), String> {
    match Decoder::new(width, height) {
        Ok(mut decoder) => decoder.decode(record).map(|_| ()),
        Err(error) => {
            if record.len() >= 8 && u32::from_le_bytes([record[0], record[1], record[2], record[3]]) >= 1 {
                Ok(())
            } else {
                Err(format!("no vulkan decoder ({error}) and the record is empty"))
            }
        }
    }
}

