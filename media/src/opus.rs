//! Opus through libopus, loaded at run time.
//!
//! The host encodes 10 ms frames at 48 kHz in the restricted-low-delay mode;
//! each packet is one `EAU2` data record. The player decodes each packet to
//! PCM for the existing audio output.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use libloading::Library;

pub const RATE: u32 = 48_000;
/// 10 ms at 48 kHz.
pub const FRAME: usize = 480;
const APPLICATION_RESTRICTED_LOWDELAY: c_int = 2051;
const SET_BITRATE_REQUEST: c_int = 4002;
pub const DEFAULT_BITRATE: i32 = 128_000;

struct Api {
    _lib: Library,
    path: PathBuf,
    encoder_create: unsafe extern "C" fn(i32, c_int, c_int, *mut c_int) -> *mut c_void,
    encode: unsafe extern "C" fn(*mut c_void, *const i16, c_int, *mut u8, i32) -> i32,
    encoder_ctl: unsafe extern "C" fn(*mut c_void, c_int, ...) -> c_int,
    encoder_destroy: unsafe extern "C" fn(*mut c_void),
    decoder_create: unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut c_void,
    decode: unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16, c_int, c_int) -> c_int,
    decoder_destroy: unsafe extern "C" fn(*mut c_void),
    strerror: unsafe extern "C" fn(c_int) -> *const std::os::raw::c_char,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

pub fn library_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("ETHERQUIC_OPUS") {
        out.push(PathBuf::from(path));
    }
    let names: &[&str] = if cfg!(windows) {
        &["opus.dll", "libopus-0.dll", "libopus.dll"]
    } else if cfg!(target_os = "macos") {
        &["libopus.0.dylib", "libopus.dylib"]
    } else {
        &["libopus.so.0", "libopus.so"]
    };
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
    {
        if cfg!(target_os = "macos") {
            dirs.push(dir.join("../Frameworks"));
        }
        dirs.push(dir);
    }
    if cfg!(windows) {
        dirs.push(PathBuf::from(r"C:\Program Files\etherquic"));
    }
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/opt/homebrew/lib"));
        dirs.push(PathBuf::from("/usr/local/lib"));
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        for var in ["LD_LIBRARY_PATH", "NIX_LD_LIBRARY_PATH"] {
            if let Some(paths) = std::env::var_os(var) {
                dirs.extend(std::env::split_paths(&paths));
            }
        }
        for dir in [
            "/run/current-system/sw/lib",
            "/usr/lib",
            "/usr/lib64",
            "/usr/lib/x86_64-linux-gnu",
        ] {
            dirs.push(PathBuf::from(dir));
        }
    }
    for dir in &dirs {
        for name in names {
            let path = dir.join(name);
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out.extend(names.iter().map(PathBuf::from));
    out
}

fn open_api() -> Result<Api, String> {
    let mut errors = Vec::new();
    for path in library_candidates() {
        let lib = match unsafe { Library::new(&path) } {
            Ok(lib) => lib,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        macro_rules! sym {
            ($name:literal) => {
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|error| format!("{}: {}: {error}", path.display(), $name))?
            };
        }
        return Ok(Api {
            path: path.clone(),
            encoder_create: sym!("opus_encoder_create"),
            encode: sym!("opus_encode"),
            encoder_ctl: sym!("opus_encoder_ctl"),
            encoder_destroy: sym!("opus_encoder_destroy"),
            decoder_create: sym!("opus_decoder_create"),
            decode: sym!("opus_decode"),
            decoder_destroy: sym!("opus_decoder_destroy"),
            strerror: sym!("opus_strerror"),
            _lib: lib,
        });
    }
    Err(format!("libopus not loaded ({})", errors.join("; ")))
}

fn api() -> Result<Arc<Api>, String> {
    static API: OnceLock<Result<Arc<Api>, String>> = OnceLock::new();
    API.get_or_init(|| open_api().map(Arc::new)).clone()
}

pub fn library_path() -> Option<PathBuf> {
    api().ok().map(|api| api.path.clone())
}

fn error_text(api: &Api, code: c_int) -> String {
    let text = unsafe { (api.strerror)(code) };
    if text.is_null() {
        return format!("opus error {code}");
    }
    unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

pub struct Encoder {
    api: Arc<Api>,
    handle: *mut c_void,
    channels: usize,
    pending: Vec<i16>,
    out: Vec<u8>,
}

unsafe impl Send for Encoder {}

impl Encoder {
    /// 48 kHz, one or two channels.
    pub fn new(channels: u16, bitrate: i32) -> Result<Encoder, String> {
        if !(1..=2).contains(&channels) {
            return Err(format!("opus takes 1 or 2 channels here, got {channels}"));
        }
        let api = api()?;
        let mut error: c_int = 0;
        let handle = unsafe {
            (api.encoder_create)(
                RATE as i32,
                channels as c_int,
                APPLICATION_RESTRICTED_LOWDELAY,
                &mut error,
            )
        };
        if handle.is_null() || error != 0 {
            return Err(format!("opus encoder: {}", error_text(&api, error)));
        }
        unsafe { (api.encoder_ctl)(handle, SET_BITRATE_REQUEST, bitrate as c_int) };
        Ok(Encoder {
            api,
            handle,
            channels: channels as usize,
            pending: Vec::new(),
            out: vec![0; 4000],
        })
    }

    /// Interleaved 48 kHz samples in, zero or more packets out.
    pub fn push(&mut self, samples: &[i16]) -> Result<Vec<Vec<u8>>, String> {
        self.pending.extend_from_slice(samples);
        let step = FRAME * self.channels;
        let mut packets = Vec::new();
        let mut start = 0;
        while self.pending.len() - start >= step {
            let n = unsafe {
                (self.api.encode)(
                    self.handle,
                    self.pending[start..].as_ptr(),
                    FRAME as c_int,
                    self.out.as_mut_ptr(),
                    self.out.len() as i32,
                )
            };
            if n < 0 {
                return Err(format!("opus encode: {}", error_text(&self.api, n)));
            }
            packets.push(self.out[..n as usize].to_vec());
            start += step;
        }
        self.pending.drain(..start);
        Ok(packets)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { (self.api.encoder_destroy)(self.handle) };
    }
}

pub struct Decoder {
    api: Arc<Api>,
    handle: *mut c_void,
    channels: usize,
    out: Vec<i16>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(channels: u16) -> Result<Decoder, String> {
        if !(1..=2).contains(&channels) {
            return Err(format!("opus takes 1 or 2 channels here, got {channels}"));
        }
        let api = api()?;
        let mut error: c_int = 0;
        let handle = unsafe { (api.decoder_create)(RATE as i32, channels as c_int, &mut error) };
        if handle.is_null() || error != 0 {
            return Err(format!("opus decoder: {}", error_text(&api, error)));
        }
        // 120 ms is the longest Opus packet.
        Ok(Decoder {
            api,
            handle,
            channels: channels as usize,
            out: vec![0; 5760 * channels as usize],
        })
    }

    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, String> {
        let frames = unsafe {
            (self.api.decode)(
                self.handle,
                packet.as_ptr(),
                packet.len() as i32,
                self.out.as_mut_ptr(),
                (self.out.len() / self.channels) as c_int,
                0,
            )
        };
        if frames < 0 {
            return Err(format!("opus decode: {}", error_text(&self.api, frames)));
        }
        Ok(self.out[..frames as usize * self.channels].to_vec())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { (self.api.decoder_destroy)(self.handle) };
    }
}
