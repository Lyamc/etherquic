//! PyroWave encode through [pyrowave-rs](https://github.com/Lyamc/pyrowave-rs),
//! and decode through `libpyrowave-shared`, loaded at run time.
//!
//! The host converts RGB to planar YUV (4:2:0 / 4:2:2 / 4:4:4, 8- or 10-bit).
//! 8-bit pictures are encoded by pyrowave-rs (hardware Vulkan, then D3D11 on
//! Windows, then OpenGL, then a CPU or lavapipe device). 10-bit still uses
//! the C encoder. The player pushes packets and reads the same layout back.
//! 4:2:2 is sent as 4:4:4 after a horizontal chroma upsample. Granite (inside PyroWave) loads the Vulkan loader
//! itself. On Windows Granite only ever does `LoadLibraryA("vulkan-1.dll")`
//! (it ignores `GRANITE_VULKAN_LIBRARY` there), so a PC without a Vulkan
//! loader gets the bundled `vulkan-cpu\vulkan-1.dll` shim preloaded, which
//! hands Granite Mesa lavapipe (CPU Vulkan). SwiftShader cannot run PyroWave:
//! its subgroups are 4 wide, it lacks clustered subgroup ops and 8-bit storage.
//! macOS gets the KosmicKrisp manifest through `VK_DRIVER_FILES` (MoltenVK
//! cannot run the encoder).
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use libloading::Library;

use crate::negotiate::{YuvChroma, YuvDepth, YuvFormat};
use crate::yuv::{rgb_to_yuv, yuv_to_rgb, YuvPlanes};
use crate::Frame;

type Handle = *mut c_void;

#[repr(C)]
struct EncInfo {
    device: Handle,
    width: c_int,
    height: c_int,
    chroma: c_int,
}

#[repr(C)]
struct DecInfo {
    device: Handle,
    width: c_int,
    height: c_int,
    chroma: c_int,
    fragment_path: bool,
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

const FORMAT_YUV420P: c_int = 1;
const FORMAT_YUV444P: c_int = 2;
const FORMAT_YUV420P10: c_int = 3;
const FORMAT_YUV444P10: c_int = 4;
const CHROMA_420: c_int = 0;
const CHROMA_444: c_int = 1;

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

fn unpack_plane(bytes: &[u8], depth: YuvDepth, count: usize) -> Vec<u16> {
    match depth {
        YuvDepth::Depth8 => bytes.iter().take(count).map(|b| u16::from(*b)).collect(),
        YuvDepth::Depth10 => bytes
            .chunks_exact(2)
            .take(count)
            .map(|chunk| u16::from_ne_bytes([chunk[0], chunk[1]]))
            .collect(),
    }
}
/// Packet size cap. QUIC carries a whole frame reliably, so this only
/// bounds the decoder's per-push work.
const PACKET_BOUNDARY: usize = 64 * 1024;

struct Api {
    _lib: Library,
    path: PathBuf,
    version: unsafe extern "C" fn(*mut u32, *mut u32, *mut u32),
    create_default_device: unsafe extern "C" fn(*mut Handle) -> c_int,
    device_destroy: unsafe extern "C" fn(Handle),
    encoder_create: unsafe extern "C" fn(*const EncInfo, *mut Handle) -> c_int,
    encode_cpu: unsafe extern "C" fn(Handle, *const CpuBuffer, *const RateControl) -> c_int,
    num_packets: unsafe extern "C" fn(Handle, usize, *mut usize) -> c_int,
    packetize:
        unsafe extern "C" fn(Handle, *mut Packet, usize, *mut usize, *mut c_void, usize) -> c_int,
    encoder_destroy: unsafe extern "C" fn(Handle),
    prefers_fragment: unsafe extern "C" fn(Handle) -> bool,
    decoder_create: unsafe extern "C" fn(*const DecInfo, *mut Handle) -> c_int,
    decoder_clear: unsafe extern "C" fn(Handle),
    push_packet: unsafe extern "C" fn(Handle, *const c_void, usize) -> c_int,
    is_ready: unsafe extern "C" fn(Handle, bool) -> bool,
    decode_cpu: unsafe extern "C" fn(Handle, *const CpuBuffer) -> c_int,
    decoder_destroy: unsafe extern "C" fn(Handle),
    /// `pyrowave_device_get_vk_device_handles`, missing in older builds.
    vk_handles: Option<unsafe extern "C" fn(Handle, *mut Handle, *mut Handle, *mut Handle)>,
}

// The function pointers and the library handle are plain data.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn result_text(code: c_int) -> String {
    match code {
        -1 => "generic error".into(),
        -2 => "invalid argument".into(),
        -3 => "out of host memory".into(),
        -4 => "out of device memory".into(),
        -5 => "no usable Vulkan device".into(),
        -6 => "not implemented".into(),
        other => format!("error {other}"),
    }
}

fn check(code: c_int, what: &str) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("pyrowave {what}: {}", result_text(code)))
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.to_path_buf())
}

/// Places to look, most specific first. `ETHERQUIC_PYROWAVE` wins.
pub fn library_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("ETHERQUIC_PYROWAVE") {
        out.push(PathBuf::from(path));
    }
    let names: &[&str] = if cfg!(windows) {
        &["libpyrowave-shared-1.dll", "libpyrowave-shared.dll"]
    } else if cfg!(target_os = "macos") {
        &["libpyrowave-shared.1.dylib", "libpyrowave-shared.dylib"]
    } else if cfg!(target_os = "android") {
        &["libpyrowave-shared.so", "libpyrowave-shared.so.1"]
    } else {
        &["libpyrowave-shared.so.1", "libpyrowave-shared.so"]
    };
    let mut dirs = etherquic::native_library_dirs();
    if let Some(dir) = exe_dir() {
        if cfg!(target_os = "macos") {
            dirs.push(dir.join("../Frameworks"));
        }
        if !dirs.iter().any(|existing| existing == &dir) {
            dirs.push(dir);
        }
    }
    if cfg!(windows) {
        dirs.push(PathBuf::from(r"C:\Program Files\etherquic"));
    }
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/opt/homebrew/lib"));
        dirs.push(PathBuf::from("/usr/local/lib"));
    }
    for var in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
        if let Some(paths) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&paths));
        }
    }
    // NixOS puts the library on PATH, beside the installed program.
    dirs.extend(etherquic::backend::pyrowave_search_dirs());
    for dir in &dirs {
        for name in names {
            let path = dir.join(name);
            if path.is_file() {
                out.push(path);
            }
        }
    }
    // A bare name lets the platform loader search its own path.
    out.extend(names.iter().map(PathBuf::from));
    out
}

/// Point Granite at a Vulkan loader and macOS at KosmicKrisp before the first
/// device is made. Only fills variables the user has not set.
fn prepare_vulkan() {
    #[cfg(windows)]
    preload_cpu_vulkan();
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os("VK_DRIVER_FILES").is_none()
            && std::env::var_os("VK_ICD_FILENAMES").is_none()
        {
            if let Some(icd) = etherquic::backend::macos_pyrowave_icds().into_iter().next() {
                std::env::set_var("VK_DRIVER_FILES", icd);
            }
        }
        if std::env::var_os("GRANITE_VULKAN_LIBRARY").is_none() {
            let bundled = exe_dir().map(|dir| dir.join("../Frameworks/libvulkan.1.dylib"));
            let brew = PathBuf::from("/opt/homebrew/lib/libvulkan.1.dylib");
            if let Some(loader) = bundled
                .filter(|p| p.is_file())
                .or(brew.is_file().then_some(brew))
            {
                std::env::set_var("GRANITE_VULKAN_LIBRARY", loader);
            }
        }
    }
    // Linux Granite dlopens `libvulkan.so.1` by name. On NixOS that loader is
    // not in the default search path; the package wrapper is what usually
    // adds it. Point Granite at the file before the first device is created.
    #[cfg(all(unix, not(target_os = "macos")))]
    if std::env::var_os("GRANITE_VULKAN_LIBRARY").is_none() {
        if let Some(loader) = linux_vulkan_loader() {
            std::env::set_var("GRANITE_VULKAN_LIBRARY", loader);
        }
    }
}

/// `libvulkan.so.1` from the library path, or from the installed `etherquic`
/// wrapper, which records the Nix store path of the loader.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_vulkan_loader() -> Option<PathBuf> {
    let names = ["libvulkan.so.1", "libvulkan.so"];
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let search = if cfg!(target_os = "android") {
        [
            "/system/lib64",
            "/system/lib",
            "/vendor/lib64",
            "/vendor/lib",
        ]
        .as_slice()
    } else {
        [
            "/run/opengl-driver/lib",
            "/run/current-system/sw/lib",
            "/usr/lib",
            "/usr/lib64",
        ]
        .as_slice()
    };
    for dir in search {
        dirs.push(PathBuf::from(dir));
    }
    for dir in dirs {
        for name in names {
            let path = dir.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let Ok(text) = std::fs::read_to_string(dir.join("etherquic")) else {
                continue;
            };
            if let Some(loader) = vulkan_loader_in_script(&text) {
                return Some(loader);
            }
        }
    }
    // A desktop session can have a short PATH. The NixOS program link is fixed.
    if let Ok(text) = std::fs::read_to_string("/run/current-system/sw/bin/etherquic") {
        if let Some(loader) = vulkan_loader_in_script(&text) {
            return Some(loader);
        }
    }
    None
}

fn path_token_end(ch: char) -> bool {
    matches!(
        ch,
        '\'' | '"' | ' ' | '\n' | '\t' | '=' | ':' | '$' | '{' | '}'
    )
}

/// The Vulkan loader named by an etherquic wrapper script, when that file exists.
fn vulkan_loader_in_script(text: &str) -> Option<PathBuf> {
    let mut rest = text;
    while let Some(rel) = rest.find("vulkan-loader-") {
        let head = &rest[..rel];
        let begin = head
            .rfind(path_token_end)
            .map(|index| index + 1)
            .unwrap_or(0);
        let after = &rest[rel..];
        let end = after.find(path_token_end).unwrap_or(after.len());
        let token = &rest[begin..rel + end];
        if let Some(lib_at) = token.find("/lib") {
            let so = PathBuf::from(&token[..lib_at + 4]).join("libvulkan.so.1");
            if so.is_file() {
                return Some(so);
            }
        }
        rest = &rest[rel + "vulkan-loader-".len()..];
    }
    None
}

/// Subdirectory of the install directory with the CPU Vulkan fallback.
pub const VULKAN_CPU_DIR: &str = "vulkan-cpu";

/// When Windows has no Vulkan loader, load the bundled shim so Granite's later
/// `LoadLibraryA("vulkan-1.dll")` gets this already-loaded module. The shim
/// loads `vulkan_lvp.dll` from its own directory; nothing is registered
/// system-wide, and the Khronos loader's `VK_DRIVER_FILES` (ignored in our
/// elevated service anyway) is not involved.
#[cfg(windows)]
fn preload_cpu_vulkan() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let system = PathBuf::from(system_root).join(r"System32\vulkan-1.dll");
        if system.is_file() || std::env::var_os("ETHERQUIC_NO_CPU_VULKAN").is_some() {
            return;
        }
        let dirs = [
            exe_dir(),
            Some(PathBuf::from(r"C:\Program Files\etherquic")),
        ];
        let Some(dir) = dirs
            .into_iter()
            .flatten()
            .map(|dir| dir.join(VULKAN_CPU_DIR))
            .find(|dir| dir.join("vulkan-1.dll").is_file() && dir.join("vulkan_lvp.dll").is_file())
        else {
            return;
        };
        // PyroWave needs full 16/32/64-wide subgroups. lavapipe's subgroup is
        // its vector width / 32, so 512 bits gives 16 lanes (LLVM splits it
        // for AVX2). lavapipe reads it when it loads, so set it first.
        if std::env::var_os("LP_NATIVE_VECTOR_WIDTH").is_none() {
            std::env::set_var("LP_NATIVE_VECTOR_WIDTH", "512");
        }
        match load(&dir.join("vulkan-1.dll")) {
            // Kept loaded for the life of the process.
            Ok(lib) => std::mem::forget(lib),
            Err(error) => eprintln!("pyrowave: CPU Vulkan shim in {}: {error}", dir.display()),
        }
    });
}

fn load(path: &PathBuf) -> Result<Library, String> {
    #[cfg(windows)]
    {
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
    #[cfg(not(windows))]
    {
        unsafe { Library::new(path) }.map_err(|error| error.to_string())
    }
}

fn open_api() -> Result<Api, String> {
    prepare_vulkan();
    let mut errors = Vec::new();
    for path in library_candidates() {
        let lib = match load(&path) {
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
        let api = Api {
            path: path.clone(),
            version: sym!("pyrowave_get_api_version"),
            create_default_device: sym!("pyrowave_create_default_device"),
            device_destroy: sym!("pyrowave_device_destroy"),
            encoder_create: sym!("pyrowave_encoder_create"),
            encode_cpu: sym!("pyrowave_encoder_encode_cpu"),
            num_packets: sym!("pyrowave_encoder_compute_num_packets"),
            packetize: sym!("pyrowave_encoder_packetize"),
            encoder_destroy: sym!("pyrowave_encoder_destroy"),
            prefers_fragment: sym!("pyrowave_decoder_device_prefers_fragment_path"),
            decoder_create: sym!("pyrowave_decoder_create"),
            decoder_clear: sym!("pyrowave_decoder_clear"),
            push_packet: sym!("pyrowave_decoder_push_packet"),
            is_ready: sym!("pyrowave_decoder_decode_is_ready"),
            decode_cpu: sym!("pyrowave_decoder_decode_cpu_buffer_synchronous"),
            decoder_destroy: sym!("pyrowave_decoder_destroy"),
            vk_handles: unsafe { lib.get(b"pyrowave_device_get_vk_device_handles\0") }
                .ok()
                .map(|f| *f),
            _lib: lib,
        };
        let (mut major, mut minor, mut patch) = (0, 0, 0);
        unsafe { (api.version)(&mut major, &mut minor, &mut patch) };
        if major != 1 {
            errors.push(format!(
                "{}: API {major}.{minor}.{patch}, want 1.x",
                path.display()
            ));
            continue;
        }
        return Ok(api);
    }
    Err(format!(
        "libpyrowave-shared not loaded ({})",
        errors.join("; ")
    ))
}

fn api() -> Result<Arc<Api>, String> {
    static API: OnceLock<Result<Arc<Api>, String>> = OnceLock::new();
    API.get_or_init(|| open_api().map(Arc::new)).clone()
}

/// Which library file was loaded, for logs.
pub fn library_path() -> Option<PathBuf> {
    api().ok().map(|api| api.path.clone())
}

struct Device {
    api: Arc<Api>,
    handle: Handle,
}

impl Device {
    fn new() -> Result<Device, String> {
        let api = api()?;
        let mut handle: Handle = std::ptr::null_mut();
        check(
            unsafe { (api.create_default_device)(&mut handle) },
            "device",
        )?;
        if handle.is_null() {
            return Err("pyrowave device: null handle".into());
        }
        Ok(Device { api, handle })
    }
}

/// The Vulkan device PyroWave picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanDevice {
    pub name: String,
    /// `VK_PHYSICAL_DEVICE_TYPE_CPU`, or a known CPU driver name.
    pub cpu: bool,
}

const VK_PHYSICAL_DEVICE_TYPE_CPU: i32 = 4;

/// A Vulkan device name that is a CPU driver, for drivers that do not say so.
pub fn cpu_driver_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["llvmpipe", "lavapipe", "swiftshader", "softpipe"]
        .iter()
        .any(|n| name.contains(n))
}

/// Read `deviceType` and `deviceName` out of a `VkPhysicalDeviceProperties`.
fn device_from_properties(bytes: &[u8]) -> Option<VulkanDevice> {
    // apiVersion, driverVersion, vendorID, deviceID: 4 x u32, then deviceType,
    // then deviceName[256].
    let kind = i32::from_ne_bytes(bytes.get(16..20)?.try_into().ok()?);
    let raw = bytes.get(20..276)?;
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let name = String::from_utf8_lossy(&raw[..end]).into_owned();
    let cpu = kind == VK_PHYSICAL_DEVICE_TYPE_CPU || cpu_driver_name(&name);
    Some(VulkanDevice { name, cpu })
}

type GetInstanceProcAddr = unsafe extern "system" fn(
    Handle,
    *const std::os::raw::c_char,
) -> Option<unsafe extern "system" fn()>;

/// `vkGetInstanceProcAddr` of the loader Granite already opened. Only an
/// already-loaded module is used, so this never pairs Granite's instance with
/// a second loader.
fn loaded_instance_proc_addr() -> Option<GetInstanceProcAddr> {
    #[cfg(windows)]
    let lib: Library = libloading::os::windows::Library::open_already_loaded("vulkan-1.dll")
        .ok()?
        .into();
    #[cfg(unix)]
    let lib: Library = {
        use libloading::os::unix::{Library as UnixLibrary, RTLD_LAZY, RTLD_LOCAL};
        const RTLD_NOLOAD: std::os::raw::c_int = if cfg!(target_os = "macos") { 0x10 } else { 0x4 };
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        if let Some(path) = std::env::var_os("GRANITE_VULKAN_LIBRARY") {
            names.push(path);
        }
        let defaults: &[&str] = if cfg!(target_os = "macos") {
            &["libvulkan.1.dylib", "libMoltenVK.dylib"]
        } else {
            &["libvulkan.so.1", "libvulkan.so"]
        };
        names.extend(defaults.iter().map(|n| n.into()));
        names
            .into_iter()
            .find_map(|name| {
                unsafe { UnixLibrary::open(Some(name), RTLD_LAZY | RTLD_LOCAL | RTLD_NOLOAD) }.ok()
            })?
            .into()
    };
    let gipa: GetInstanceProcAddr = *unsafe { lib.get(b"vkGetInstanceProcAddr\0") }.ok()?;
    // The module stays loaded for Granite anyway; drop only our reference count.
    drop(lib);
    Some(gipa)
}

impl Device {
    fn vulkan_device(&self) -> Option<VulkanDevice> {
        let get = self.api.vk_handles?;
        let (mut instance, mut physical): (Handle, Handle) =
            (std::ptr::null_mut(), std::ptr::null_mut());
        unsafe {
            get(
                self.handle,
                &mut instance,
                &mut physical,
                std::ptr::null_mut(),
            )
        };
        if instance.is_null() || physical.is_null() {
            return None;
        }
        let gipa = loaded_instance_proc_addr()?;
        let f = unsafe { gipa(instance, b"vkGetPhysicalDeviceProperties\0".as_ptr().cast()) }?;
        let props: unsafe extern "system" fn(Handle, *mut u8) = unsafe { std::mem::transmute(f) };
        // VkPhysicalDeviceProperties is 824 bytes; leave room.
        let mut buf = vec![0u64; 160];
        unsafe { props(physical, buf.as_mut_ptr().cast()) };
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
        device_from_properties(bytes)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { (self.api.device_destroy)(self.handle) };
    }
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

enum Engine {
    Rs(pyrowave_rs::Encoder),
    /// 10-bit pictures. pyrowave-rs encodes 8-bit planes only.
    C {
        device: Device,
        handle: Handle,
        bitstream: Vec<u8>,
        packets: Vec<Packet>,
    },
}

/// One encoder for one picture size.
pub struct Encoder {
    engine: Engine,
    width: u32,
    height: u32,
    format: YuvFormat,
    planes: YuvPlanes,
    pack_y: Vec<u8>,
    pack_u: Vec<u8>,
    pack_v: Vec<u8>,
    frame_bytes: usize,
    reported: Option<VulkanDevice>,
    /// `vulkan`, `d3d11`, `opengl`, `cpu`, or `c`.
    backend: &'static str,
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
                "pyrowave needs even sides of 16 or more, got {width}x{height}"
            ));
        }
        let frame_bytes = frame_bytes.max(16 * 1024);
        let planes = YuvPlanes::new(width as usize, height as usize, format);
        if format.bitstream().depth == YuvDepth::Depth10 {
            return Self::open_c(width, height, frame_bytes, format, planes);
        }
        let (inner, reported, backend) = open_rs()?;
        Ok(Encoder {
            engine: Engine::Rs(inner),
            width,
            height,
            format,
            planes,
            pack_y: Vec::new(),
            pack_u: Vec::new(),
            pack_v: Vec::new(),
            frame_bytes,
            reported: Some(reported),
            backend,
        })
    }

    fn open_c(
        width: u32,
        height: u32,
        frame_bytes: usize,
        format: YuvFormat,
        planes: YuvPlanes,
    ) -> Result<Encoder, String> {
        let device = Device::new()?;
        let info = EncInfo {
            device: device.handle,
            width: width as c_int,
            height: height as c_int,
            chroma: pyro_chroma(format),
        };
        let mut handle: Handle = std::ptr::null_mut();
        check(
            unsafe { (device.api.encoder_create)(&info, &mut handle) },
            "encoder",
        )?;
        let reported = device.vulkan_device();
        Ok(Encoder {
            engine: Engine::C {
                device,
                handle,
                bitstream: vec![0; frame_bytes + 64 * 1024],
                packets: Vec::new(),
            },
            width,
            height,
            format,
            planes,
            pack_y: Vec::new(),
            pack_u: Vec::new(),
            pack_v: Vec::new(),
            frame_bytes,
            reported,
            backend: "c",
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// `vulkan`, `d3d11`, `opengl`, `cpu`, or `c` (10-bit C encoder).
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// Per-picture budget used on the next [`Self::encode`]. Quality preview
    /// reuses one encoder and only changes this.
    pub fn set_frame_bytes(&mut self, frame_bytes: usize) {
        self.frame_bytes = frame_bytes.max(16 * 1024);
        if let Engine::C { bitstream, .. } = &mut self.engine {
            if bitstream.len() < self.frame_bytes + 64 * 1024 {
                bitstream.resize(self.frame_bytes + 64 * 1024, 0);
            }
        }
    }

    /// The device this encoder runs on. CPU and lavapipe set [`VulkanDevice::cpu`].
    pub fn vulkan_device(&self) -> Option<VulkanDevice> {
        self.reported.clone()
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
        let frame_bytes = self.frame_bytes;
        match &mut self.engine {
            Engine::Rs(inner) => encode_rs(
                inner,
                &self.pack_y,
                &self.pack_u,
                &self.pack_v,
                work.width,
                work.height,
                work.format,
                frame_bytes,
            ),
            Engine::C {
                device,
                handle,
                bitstream,
                packets,
            } => encode_c(
                device,
                *handle,
                bitstream,
                packets,
                &mut self.pack_y,
                &mut self.pack_u,
                &mut self.pack_v,
                work.width,
                work.height,
                work.format,
                frame_bytes,
            ),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Engine::C { device, handle, .. } = &self.engine {
            unsafe { (device.api.encoder_destroy)(*handle) };
        }
    }
}

fn open_rs() -> Result<(pyrowave_rs::Encoder, VulkanDevice, &'static str), String> {
    if let Some(backend) = forced_backend() {
        let allow_software = backend == pyrowave_rs::Backend::Vulkan;
        let inner = pyrowave_rs::Encoder::create(pyrowave_rs::CreateInfo {
            backend,
            allow_software,
        })?;
        let name = backend_name(backend);
        let device = report(&inner, name);
        return Ok((inner, device, name));
    }
    let mut errors = Vec::new();
    let mut software = None;
    match try_backend(pyrowave_rs::Backend::Vulkan, true) {
        Ok(inner) => {
            let device = report(&inner, "vulkan");
            if !device.cpu {
                return Ok((inner, device, "vulkan"));
            }
            software = Some((inner, device));
        }
        Err(error) => errors.push(format!("vulkan: {error}")),
    }
    #[cfg(windows)]
    {
        match try_backend(pyrowave_rs::Backend::D3d11, false) {
            Ok(inner) => {
                let device = report(&inner, "d3d11");
                return Ok((inner, device, "d3d11"));
            }
            Err(error) => errors.push(format!("d3d11: {error}")),
        }
    }
    match try_backend(pyrowave_rs::Backend::OpenGl, false) {
        Ok(inner) => {
            let device = report(&inner, "opengl");
            return Ok((inner, device, "opengl"));
        }
        Err(error) => errors.push(format!("opengl: {error}")),
    }
    if let Some((inner, device)) = software {
        return Ok((inner, device, "vulkan"));
    }
    match try_backend(pyrowave_rs::Backend::Cpu, false) {
        Ok(inner) => {
            let device = report(&inner, "cpu");
            Ok((inner, device, "cpu"))
        }
        Err(error) => Err(format!(
            "pyrowave-rs ({}; cpu: {error})",
            errors.join("; ")
        )),
    }
}

fn try_backend(
    backend: pyrowave_rs::Backend,
    allow_software: bool,
) -> Result<pyrowave_rs::Encoder, String> {
    pyrowave_rs::Encoder::create(pyrowave_rs::CreateInfo {
        backend,
        allow_software,
    })
}

fn forced_backend() -> Option<pyrowave_rs::Backend> {
    match std::env::var("ETHERQUIC_PYROWAVE_BACKEND")
        .ok()
        .as_deref()
    {
        Some("cpu") => Some(pyrowave_rs::Backend::Cpu),
        Some("vulkan") => Some(pyrowave_rs::Backend::Vulkan),
        Some("d3d11") => Some(pyrowave_rs::Backend::D3d11),
        Some("opengl") => Some(pyrowave_rs::Backend::OpenGl),
        _ => None,
    }
}

fn backend_name(backend: pyrowave_rs::Backend) -> &'static str {
    match backend {
        pyrowave_rs::Backend::Cpu => "cpu",
        pyrowave_rs::Backend::D3d11 => "d3d11",
        pyrowave_rs::Backend::OpenGl => "opengl",
        pyrowave_rs::Backend::Vulkan => "vulkan",
    }
}

fn report(enc: &pyrowave_rs::Encoder, backend: &str) -> VulkanDevice {
    let name = if enc.device_name.is_empty() {
        backend.to_string()
    } else {
        enc.device_name.clone()
    };
    let cpu = backend == "cpu" || cpu_driver_name(&name);
    VulkanDevice { name, cpu }
}

fn encode_rs(
    inner: &mut pyrowave_rs::Encoder,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
    format: YuvFormat,
    frame_bytes: usize,
) -> Result<Vec<u8>, String> {
    let chroma = match format.chroma {
        YuvChroma::Yuv420 => pyrowave_rs::Chroma::Yuv420,
        YuvChroma::Yuv422 | YuvChroma::Yuv444 => pyrowave_rs::Chroma::Yuv444,
    };
    let target = u32::try_from(frame_bytes).unwrap_or(u32::MAX);
    inner.encode_target(
        &pyrowave_rs::YuvFrame {
            width: width as u32,
            height: height as u32,
            chroma,
            y,
            u,
            v,
        },
        target,
    )?;
    let mut bitstream = vec![0u8; frame_bytes.saturating_add(64 * 1024)];
    let packets = inner.packetize(PACKET_BOUNDARY, &mut bitstream)?;
    record_from_slices(&bitstream, &packets[..])
}

fn encode_c(
    device: &Device,
    handle: Handle,
    bitstream: &mut [u8],
    packets: &mut Vec<Packet>,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    width: usize,
    height: usize,
    format: YuvFormat,
    frame_bytes: usize,
) -> Result<Vec<u8>, String> {
    let api = &device.api;
    let buffer = packed_buffer(y, u, v, width, height, format);
    let rate = RateControl {
        maximum_bitstream_size: frame_bytes,
    };
    check(
        unsafe { (api.encode_cpu)(handle, &buffer, &rate) },
        "encode",
    )?;
    let mut count = 0usize;
    check(
        unsafe { (api.num_packets)(handle, PACKET_BOUNDARY, &mut count) },
        "packet count",
    )?;
    packets.resize(count.max(1), Packet::default());
    let mut written = 0usize;
    check(
        unsafe {
            (api.packetize)(
                handle,
                packets.as_mut_ptr(),
                PACKET_BOUNDARY,
                &mut written,
                bitstream.as_mut_ptr().cast(),
                bitstream.len(),
            )
        },
        "packetize",
    )?;
    let packets = &packets[..written.min(packets.len())];
    let total: usize = packets.iter().map(|p| p.size + 4).sum();
    let mut out = Vec::with_capacity(4 + total);
    out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    for packet in packets {
        let end = packet.offset + packet.size;
        if end > bitstream.len() {
            return Err("pyrowave packet outside the bitstream".into());
        }
        out.extend_from_slice(&(packet.size as u32).to_le_bytes());
        out.extend_from_slice(&bitstream[packet.offset..end]);
    }
    Ok(out)
}

fn record_from_slices(bitstream: &[u8], packets: &[pyrowave_rs::Packet]) -> Result<Vec<u8>, String> {
    let total: usize = packets.iter().map(|p| p.size + 4).sum();
    let mut out = Vec::with_capacity(4 + total);
    out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    for packet in packets {
        let end = packet.offset + packet.size;
        if end > bitstream.len() {
            return Err("pyrowave packet outside the bitstream".into());
        }
        out.extend_from_slice(&(packet.size as u32).to_le_bytes());
        out.extend_from_slice(&bitstream[packet.offset..end]);
    }
    Ok(out)
}

/// One decoder for one picture size.
pub struct Decoder {
    device: Device,
    handle: Handle,
    format: YuvFormat,
    planes: YuvPlanes,
    pack_y: Vec<u8>,
    pack_u: Vec<u8>,
    pack_v: Vec<u8>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(width: u32, height: u32, format: YuvFormat) -> Result<Decoder, String> {
        if width % 2 != 0 || height % 2 != 0 || width == 0 || height == 0 {
            return Err(format!("pyrowave needs even sides, got {width}x{height}"));
        }
        let format = format.bitstream();
        let device = Device::new()?;
        let fragment = unsafe { (device.api.prefers_fragment)(device.handle) };
        let info = DecInfo {
            device: device.handle,
            width: width as c_int,
            height: height as c_int,
            chroma: pyro_chroma(format),
            fragment_path: fragment,
        };
        let mut handle: Handle = std::ptr::null_mut();
        check(
            unsafe { (device.api.decoder_create)(&info, &mut handle) },
            "decoder",
        )?;
        let (cw, ch) = crate::yuv::chroma_size(width as usize, height as usize, format.chroma);
        let bpp = match format.depth {
            YuvDepth::Depth8 => 1,
            YuvDepth::Depth10 => 2,
        };
        Ok(Decoder {
            device,
            handle,
            format,
            planes: YuvPlanes::new(width as usize, height as usize, format),
            pack_y: vec![0; width as usize * height as usize * bpp],
            pack_u: vec![0; cw * ch * bpp],
            pack_v: vec![0; cw * ch * bpp],
        })
    }

    /// The Vulkan device this decoder runs on, when the library can say.
    pub fn vulkan_device(&self) -> Option<VulkanDevice> {
        self.device.vulkan_device()
    }

    pub fn decode(&mut self, record: &[u8]) -> Result<Frame, String> {
        let api = &self.device.api;
        unsafe { (api.decoder_clear)(self.handle) };
        let mut rest = record;
        let count = take_u32(&mut rest)? as usize;
        for _ in 0..count {
            let size = take_u32(&mut rest)? as usize;
            if rest.len() < size {
                return Err("pyrowave record is short".into());
            }
            let (packet, tail) = rest.split_at(size);
            check(
                unsafe { (api.push_packet)(self.handle, packet.as_ptr().cast(), packet.len()) },
                "push",
            )?;
            rest = tail;
        }
        if !unsafe { (api.is_ready)(self.handle, false) }
            && !unsafe { (api.is_ready)(self.handle, true) }
        {
            return Err("pyrowave frame is incomplete".into());
        }
        let buffer = packed_buffer(
            &mut self.pack_y,
            &mut self.pack_u,
            &mut self.pack_v,
            self.planes.width,
            self.planes.height,
            self.format,
        );
        check(unsafe { (api.decode_cpu)(self.handle, &buffer) }, "decode")?;
        let (cw, ch) = self.planes.chroma_size();
        self.planes.y = unpack_plane(&self.pack_y, self.format.depth, self.planes.y.len());
        self.planes.u = unpack_plane(&self.pack_u, self.format.depth, cw * ch);
        self.planes.v = unpack_plane(&self.pack_v, self.format.depth, cw * ch);
        Ok(yuv_to_rgb(&self.planes))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { (self.device.api.decoder_destroy)(self.handle) };
    }
}

fn take_u32(rest: &mut &[u8]) -> Result<u32, String> {
    if rest.len() < 4 {
        return Err("pyrowave record is short".into());
    }
    let value = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    *rest = &rest[4..];
    Ok(value)
}

/// Default per-frame budget: about 4 bits per pixel.
///
/// Quality 40 is 1.0× this. The D3D11 encoder skips per-block RDO and, when
/// the payload exceeds the budget, raises every leftover block from quant 0
/// to 7. 1.5 bpp often crossed that cliff on desktop UI. 4 bpp keeps typical
/// 720p desks at quant 0 on a LAN (about 111 Mbit/s at 30 fps).
pub fn default_frame_bytes(width: u32, height: u32) -> usize {
    (width as usize * height as usize / 2).max(32 * 1024)
}

#[cfg(test)]
mod budget_tests {
    #[test]
    fn default_is_four_bits_per_pixel() {
        assert_eq!(super::default_frame_bytes(1280, 720), 1280 * 720 / 2);
        assert_eq!(super::default_frame_bytes(64, 64), 32 * 1024);
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;

    fn props(kind: i32, name: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 824];
        bytes[16..20].copy_from_slice(&kind.to_ne_bytes());
        bytes[20..20 + name.len()].copy_from_slice(name.as_bytes());
        bytes
    }

    #[test]
    fn cpu_device_type_or_driver_name_is_software() {
        assert!(
            device_from_properties(&props(4, "llvmpipe (LLVM 19.1.7, 256 bits)"))
                .unwrap()
                .cpu
        );
        assert!(device_from_properties(&props(4, "Some CPU")).unwrap().cpu);
        // A driver that reports "other" but is SwiftShader.
        assert!(
            device_from_properties(&props(0, "SwiftShader Device (Subzero)"))
                .unwrap()
                .cpu
        );
        let gpu = device_from_properties(&props(2, "NVIDIA GeForce RTX 4090")).unwrap();
        assert!(!gpu.cpu);
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 4090");
        let dir = std::env::temp_dir().join(format!("eq-vk-{}", std::process::id()));
        let root = dir.join("vulkan-loader-test");
        let lib = root.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let so = lib.join("libvulkan.so.1");
        std::fs::write(&so, b"").unwrap();
        let script = format!("LD_LIBRARY_PATH='{}'", lib.display());
        assert_eq!(vulkan_loader_in_script(&script).as_ref(), Some(&so));
        // An earlier `/lib` on the same line must not hide the loader path.
        let joined = format!("LD_LIBRARY_PATH=/run/opengl-driver/lib:{}", lib.display());
        assert_eq!(vulkan_loader_in_script(&joined).as_ref(), Some(&so));
        assert!(vulkan_loader_in_script("no loader here").is_none());
        let _ = std::fs::remove_dir_all(&dir);
        // KosmicKrisp on Metal is an integrated GPU.
        assert!(
            !device_from_properties(&props(1, "Apple M2 Pro (KosmicKrisp)"))
                .unwrap()
                .cpu
        );
    }
}
