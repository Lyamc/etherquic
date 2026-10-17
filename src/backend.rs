//! Which media tools this process can run.
//!
//! Opus is announced when the selected tool is present. The default order is
//! libopus, then ffmpeg, then GStreamer. `pick_backend` walks whatever order
//! the caller supplies and keeps the first entry when none are installed.
//! On Windows the service runs as LocalSystem,
//! whose `PATH` is the machine path. The signed-in user's path, including a
//! WinGet ffmpeg, is merged in before the probe. `libopus.dll` or `opus.dll`
//! beside the program, in System32, or on that path counts as libopus.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::discover::{
    AnnouncedCodec, AUDIO_OPUS, AUDIO_PCM, VIDEO_AV1, VIDEO_H264, VIDEO_H265, VIDEO_MJPEG,
    VIDEO_PYROWAVE,
};

/// A tool that can encode and decode Opus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaBackend {
    Libopus,
    Ffmpeg,
    Gstreamer,
}

impl MediaBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Libopus => "libopus",
            Self::Ffmpeg => "ffmpeg",
            Self::Gstreamer => "GStreamer",
        }
    }
}

/// What a probe found. Any combination can be present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendProbe {
    pub libopus: bool,
    pub ffmpeg: bool,
    pub gstreamer: bool,
}

impl BackendProbe {
    pub fn has(self, backend: MediaBackend) -> bool {
        match backend {
            MediaBackend::Libopus => self.libopus,
            MediaBackend::Ffmpeg => self.ffmpeg,
            MediaBackend::Gstreamer => self.gstreamer,
        }
    }
}

/// Builtin libopus, then ffmpeg, then GStreamer.
pub fn default_backend_order() -> [MediaBackend; 3] {
    [
        MediaBackend::Libopus,
        MediaBackend::Ffmpeg,
        MediaBackend::Gstreamer,
    ]
}

/// The first installed tool in `order`. An empty probe keeps the first entry
/// so the missing preferred tool is the one named.
pub fn pick_backend(order: &[MediaBackend], probe: &BackendProbe) -> MediaBackend {
    order
        .iter()
        .copied()
        .find(|backend| probe.has(*backend))
        .or_else(|| order.first().copied())
        .unwrap_or(MediaBackend::Libopus)
}

/// Default order: libopus, then ffmpeg, then GStreamer.
pub fn selected_backend(probe: &BackendProbe) -> MediaBackend {
    pick_backend(&default_backend_order(), probe)
}

/// Merge the user and machine paths into this process, then look for the tools.
pub fn probe_backends() -> BackendProbe {
    widen_process_path();
    BackendProbe {
        libopus: native_opus_present(),
        ffmpeg: tool_runs("ffmpeg", "-version"),
        gstreamer: tool_runs("gst-inspect-1.0", "--version"),
    }
}

/// `libopus.dll` or `opus.dll` on Windows. `libopus.so` on other systems.
fn native_opus_present() -> bool {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    #[cfg(windows)]
    {
        dirs.push(PathBuf::from(r"C:\Program Files\etherquic"));
        dirs.push(PathBuf::from(r"C:\Windows\System32"));
        if let Ok(path) = std::env::var("PATH") {
            dirs.extend(split_list(&path, ';'));
        }
        file_named_in(&["libopus.dll", "opus.dll"], &dirs)
    }
    #[cfg(not(windows))]
    {
        if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        dirs.push(PathBuf::from("/run/current-system/sw/lib"));
        dirs.push(PathBuf::from("/usr/lib"));
        dirs.push(PathBuf::from("/usr/lib64"));
        file_named_in(&["libopus.so", "libopus.so.0"], &dirs)
    }
}

#[cfg(windows)]
fn split_list(path: &str, sep: char) -> Vec<PathBuf> {
    path.split(sep)
        .map(|part| PathBuf::from(part.trim().trim_matches('"')))
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect()
}

/// Vulkan compute plus the shared library, or a D3D11 encoder on Windows.
///
/// `d3d11` is true when `pyrowave_d3d11.dll` is installed and this process
/// can create a hardware feature-level 11_0 device. The unit test passes the
/// flag itself so a DLL beside the test binary cannot change the result.
pub fn pyrowave_ready_with(vulkan: bool, library: bool, d3d11: bool) -> bool {
    (vulkan && library) || d3d11
}

/// Vulkan compute is not enough. The shared library has to be installed too,
/// unless the Windows D3D11 encoder is present on a feature-level 11_0 GPU.
pub fn pyrowave_ready(vulkan: bool, library: bool) -> bool {
    pyrowave_ready_with(vulkan, library, d3d11_encoder_present())
}

/// PyroWave encode on a GPU, so the default order treats it as hardware.
///
/// On Windows that is `pyrowave_d3d11.dll` plus a hardware feature-level 11_0
/// device, or an NVIDIA GPU with `libpyrowave-shared`. CPU Vulkan is the
/// `pyrowave-cpu` row and is not hardware encode.
pub fn pyrowave_hardware_encode() -> bool {
    #[cfg(windows)]
    {
        d3d11_encoder_present() || (nvidia_present() && pyrowave_library_present())
    }
    #[cfg(not(windows))]
    {
        gpu_compute_present() && pyrowave_library_present()
    }
}

/// `pyrowave_d3d11.dll` next to this program, in the install directory, or
/// named by `ETHERQUIC_PYROWAVE_D3D11`, and a hardware FL 11_0 device.
fn d3d11_encoder_present() -> bool {
    #[cfg(not(windows))]
    {
        false
    }
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static READY: OnceLock<bool> = OnceLock::new();
        *READY.get_or_init(|| pyrowave_d3d11_dll_present() && hardware_feature_level_11())
    }
}

#[cfg(windows)]
fn pyrowave_d3d11_dll_present() -> bool {
    if let Some(value) = std::env::var_os("ETHERQUIC_PYROWAVE_D3D11") {
        if value == "off" {
            return false;
        }
        if Path::new(&value).is_file() {
            return true;
        }
    }
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs.push(PathBuf::from(r"C:\Program Files\etherquic"));
    file_named_in(&["pyrowave_d3d11.dll"], &dirs)
}

/// `D3D11CreateDevice` with a null adapter and `D3D_DRIVER_TYPE_HARDWARE`.
/// Feature level 11_0 is `0xb000`. SDK version 7 is `D3D11_SDK_VERSION`.
#[cfg(windows)]
#[allow(unsafe_code)]
fn hardware_feature_level_11() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const u8) -> *mut core::ffi::c_void;
        fn GetProcAddress(
            module: *mut core::ffi::c_void,
            name: *const u8,
        ) -> *mut core::ffi::c_void;
        fn FreeLibrary(module: *mut core::ffi::c_void) -> i32;
    }
    type CreateDevice = unsafe extern "system" fn(
        *mut core::ffi::c_void,
        u32,
        *mut core::ffi::c_void,
        u32,
        *const u32,
        u32,
        u32,
        *mut *mut core::ffi::c_void,
        *mut u32,
        *mut *mut core::ffi::c_void,
    ) -> i32;
    const DRIVER_HARDWARE: u32 = 1;
    const FEATURE_LEVEL_11_0: u32 = 0xb000;
    const SDK_VERSION: u32 = 7;
    unsafe {
        let module = LoadLibraryA(b"d3d11.dll\0".as_ptr());
        if module.is_null() {
            return false;
        }
        let symbol = GetProcAddress(module, b"D3D11CreateDevice\0".as_ptr());
        if symbol.is_null() {
            FreeLibrary(module);
            return false;
        }
        let create: CreateDevice = std::mem::transmute(symbol);
        let mut device: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut context: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut level = 0u32;
        let feature = FEATURE_LEVEL_11_0;
        let hr = create(
            std::ptr::null_mut(),
            DRIVER_HARDWARE,
            std::ptr::null_mut(),
            0,
            &feature,
            1,
            SDK_VERSION,
            &mut device,
            &mut level,
            &mut context,
        );
        // IUnknown::Release is vtable slot 2.
        if !context.is_null() {
            let vt = *(context as *const *const usize);
            let release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 =
                std::mem::transmute(*vt.add(2));
            release(context);
        }
        if !device.is_null() {
            let vt = *(device as *const *const usize);
            let release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 =
                std::mem::transmute(*vt.add(2));
            release(device);
        }
        FreeLibrary(module);
        hr >= 0 && level >= FEATURE_LEVEL_11_0
    }
}

/// Directories that can hold `libpyrowave-shared` besides the program directory.
///
/// NixOS installs the library next to `etherquic` on `PATH`
/// (`/run/current-system/sw/bin`), not under `lib/`.
pub fn pyrowave_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    dirs.push(PathBuf::from("/run/current-system/sw/bin"));
    dirs
}

/// `libpyrowave-shared` beside this program, on the library path, or in the install directory.
pub fn pyrowave_library_present() -> bool {
    if let Some(path) = std::env::var_os("ETHERQUIC_PYROWAVE") {
        if Path::new(&path).is_file() {
            return true;
        }
    }
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    #[cfg(windows)]
    {
        dirs.push(PathBuf::from(r"C:\Program Files\etherquic"));
        if let Ok(path) = std::env::var("PATH") {
            dirs.extend(split_list(&path, ';'));
        }
        file_named_in(
            &["libpyrowave-shared.dll", "libpyrowave-shared-1.dll"],
            &dirs,
        )
    }
    #[cfg(target_os = "macos")]
    {
        dirs.extend(macos_library_dirs());
        file_named_in(
            &[
                "libpyrowave-shared.dylib",
                "libpyrowave-shared.1.dylib",
                "libpyrowave-shared.1.0.0.dylib",
            ],
            &dirs,
        )
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        dirs.push(PathBuf::from("/run/current-system/sw/lib"));
        dirs.push(PathBuf::from("/usr/lib"));
        dirs.push(PathBuf::from("/usr/lib64"));
        dirs.extend(pyrowave_search_dirs());
        file_named_in(
            &[
                "libpyrowave-shared.so",
                "libpyrowave-shared.so.1",
                "libpyrowave-shared.so.1.0.0",
            ],
            &dirs,
        )
    }
}

fn file_named_in(names: &[&str], dirs: &[PathBuf]) -> bool {
    dirs.iter()
        .any(|dir| names.iter().any(|name| dir.join(name).is_file()))
}

/// Video and audio rows for a version-2 discovery offer.
///
/// `selected` decides whether Opus is runnable. Both tools are still named
/// when they were found, so the other computer can show them.
pub fn offer_media(selected: MediaBackend) -> (Vec<AnnouncedCodec>, Vec<AnnouncedCodec>) {
    offer_from(
        probe_backends(),
        selected,
        nvidia_present(),
        gpu_compute_present(),
    )
}

pub fn offer_from(
    probe: BackendProbe,
    selected: MediaBackend,
    nvidia: bool,
    gpu: bool,
) -> (Vec<AnnouncedCodec>, Vec<AnnouncedCodec>) {
    let video = vec![
        row(
            VIDEO_PYROWAVE,
            pyrowave_hardware_encode(),
            false,
            false,
            pyrowave_ready(gpu, pyrowave_library_present()),
            false,
            false,
            false,
        ),
        row(VIDEO_MJPEG, false, false, false, false, false, false, false),
        row(
            VIDEO_H264, nvidia, nvidia, false, false, false, false, false,
        ),
        row(
            VIDEO_H265, nvidia, nvidia, false, false, false, false, false,
        ),
        row(VIDEO_AV1, false, false, false, false, false, false, false),
    ];
    let audio = vec![
        row(AUDIO_PCM, false, false, true, false, false, false, false),
        row(
            AUDIO_OPUS,
            false,
            false,
            probe.has(selected),
            false,
            probe.libopus,
            probe.ffmpeg,
            probe.gstreamer,
        ),
    ];
    (video, audio)
}

fn row(
    id: u8,
    hardware_encode: bool,
    hardware_decode: bool,
    cpu: bool,
    gpu: bool,
    libopus: bool,
    ffmpeg: bool,
    gstreamer: bool,
) -> AnnouncedCodec {
    AnnouncedCodec {
        id,
        hardware_encode,
        hardware_decode,
        cpu,
        gpu,
        libopus,
        ffmpeg,
        gstreamer,
    }
}

/// An encode ASIC is recorded only for NVIDIA. AV1 is left unset.
pub fn nvidia_present() -> bool {
    #[cfg(windows)]
    {
        [
            r"C:\Windows\System32\nvapi64.dll",
            r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe",
        ]
        .iter()
        .any(|path| Path::new(path).is_file())
            || tool_runs("nvidia-smi", "-L")
    }
    #[cfg(not(windows))]
    {
        Path::new("/dev/nvidia0").exists() || Path::new("/proc/driver/nvidia/version").exists()
    }
}

/// Where a macOS build looks for a dylib: the app bundle's `Frameworks`,
/// the dyld paths, then Homebrew and `/usr/local`.
#[cfg(target_os = "macos")]
fn macos_library_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(contents) = macos_bundle_contents() {
        dirs.push(contents.join("Frameworks"));
    }
    for var in ["DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
        if let Some(path) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&path));
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/lib"));
    dirs.push(PathBuf::from("/usr/local/lib"));
    dirs
}

/// `Foo.app/Contents` when this program runs from an app bundle.
#[cfg(target_os = "macos")]
fn macos_bundle_contents() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents")
        .then(|| contents.to_path_buf())
}

/// Vulkan driver manifests a macOS build can use for PyroWave.
///
/// PyroWave's encoder needs a compute shader pinned to one subgroup size.
/// MoltenVK reports subgroup sizes 4 to 32 and no stage that can require a
/// size, so the encoder refuses it. Mesa's KosmicKrisp reports exactly 32,
/// which PyroWave accepts. Only KosmicKrisp counts here.
#[cfg(target_os = "macos")]
pub fn macos_pyrowave_icds() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(contents) = macos_bundle_contents() {
        dirs.push(contents.join("Resources/vulkan/icd.d"));
    }
    for dir in [
        "/opt/homebrew/share/vulkan/icd.d",
        "/opt/homebrew/etc/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
        "/usr/local/etc/vulkan/icd.d",
    ] {
        dirs.push(PathBuf::from(dir));
    }
    let mut found: Vec<PathBuf> = std::env::var_os("VK_DRIVER_FILES")
        .or_else(|| std::env::var_os("VK_ICD_FILENAMES"))
        .map(|files| std::env::split_paths(&files).collect())
        .unwrap_or_default();
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            names.sort();
            found.extend(names);
        }
    }
    found
        .into_iter()
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().contains("kosmickrisp"))
                && path.is_file()
        })
        .collect()
}

/// A Vulkan loader and a driver PyroWave runs on, on macOS.
#[cfg(target_os = "macos")]
fn macos_vulkan_compute() -> bool {
    file_named_in(
        &["libvulkan.1.dylib", "libvulkan.dylib"],
        &macos_library_dirs(),
    ) && !macos_pyrowave_icds().is_empty()
}

pub fn gpu_compute_present() -> bool {
    #[cfg(target_os = "macos")]
    if macos_vulkan_compute() {
        return true;
    }
    nvidia_present()
        || std::env::var_os("VK_DRIVER_FILES").is_some()
        || Path::new("/dev/dri/renderD128").exists()
        || Path::new("/run/opengl-driver/lib/libvulkan.so.1").exists()
}

/// No-op off Windows. The service account does not see the signed-in user's path.
pub fn widen_process_path() {
    #[cfg(windows)]
    {
        let extras = user_path_dirs();
        let current = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", merge_windows_path(&current, &extras));
    }
}

fn tool_runs(program: &str, arg: &str) -> bool {
    let mut cmd = Command::new(program);
    cmd.arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// User directories first, then the process path. A repeated directory is kept once.
pub fn merge_windows_path(current: &str, extras: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for part in extras.iter().map(String::as_str).chain(current.split(';')) {
        let dir = part.trim().trim_matches('"');
        if dir.is_empty() {
            continue;
        }
        let key = dir.trim_end_matches(['\\', '/']).to_ascii_lowercase();
        if seen.insert(key) {
            out.push(dir.to_string());
        }
    }
    out.join(";")
}

#[cfg(windows)]
fn user_path_dirs() -> Vec<String> {
    let mut dirs = Vec::new();
    for key in [
        r"HKCU\Environment",
        r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    ] {
        if let Some(path) = reg_path(key) {
            dirs.extend(split_path(&path));
        }
    }
    if let Some(text) = reg_query(&["query", "HKU"]) {
        for line in text.lines() {
            let line = line.trim();
            if line.contains("S-1-5-21-") && !line.contains("_Classes") {
                let key = format!(r"{line}\Environment");
                if let Some(path) = reg_path(&key) {
                    dirs.extend(split_path(&path));
                }
            }
        }
    }
    dirs.extend(winget_ffmpeg_bins());
    if Path::new(r"C:\Program Files\gstreamer\1.0\msvc_x86_64\bin\gst-inspect-1.0.exe").is_file() {
        dirs.push(r"C:\Program Files\gstreamer\1.0\msvc_x86_64\bin".into());
    }
    dirs
}

#[cfg(windows)]
fn split_path(path: &str) -> Vec<String> {
    path.split(';')
        .map(|part| part.trim().trim_matches('"').to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(windows)]
fn reg_path(key: &str) -> Option<String> {
    let text = reg_query(&["query", key, "/v", "Path"])?;
    for line in text.lines() {
        if let Some(value) = path_value(line) {
            return Some(expand_percents(&value));
        }
    }
    None
}

#[cfg(windows)]
fn reg_query(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("reg");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(decode_reg_output(&out.stdout))
}

#[cfg(windows)]
fn winget_ffmpeg_bins() -> Vec<String> {
    let mut found = Vec::new();
    let Ok(users) = std::fs::read_dir(r"C:\Users") else {
        return found;
    };
    for user in users.flatten() {
        let packages = user
            .path()
            .join("AppData")
            .join("Local")
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        let Ok(pkgs) = std::fs::read_dir(&packages) else {
            continue;
        };
        for pkg in pkgs.flatten() {
            push_ffmpeg_bin(&mut found, &pkg.path().join("bin"));
            let Ok(nested) = std::fs::read_dir(pkg.path()) else {
                continue;
            };
            for child in nested.flatten() {
                push_ffmpeg_bin(&mut found, &child.path().join("bin"));
            }
        }
    }
    found
}

#[cfg(windows)]
fn push_ffmpeg_bin(found: &mut Vec<String>, bin: &Path) {
    if bin.join("ffmpeg.exe").is_file() {
        found.push(bin.display().to_string());
    }
}

#[cfg(windows)]
fn expand_percents(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if name.is_empty() {
                out.push('%');
            } else if let Ok(value) = std::env::var(name) {
                out.push_str(&value);
            } else if name.eq_ignore_ascii_case("SystemRoot") {
                out.push_str(r"C:\Windows");
            } else {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// `reg query` writes UTF-16 when stdout is a pipe. A console writes the
/// active code page, which is ASCII for these keys.
#[cfg(any(windows, test))]
fn decode_reg_output(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let wide = bytes.len() >= 4
        && bytes.len() % 2 == 0
        && bytes.chunks_exact(2).filter(|pair| pair[1] == 0).count() * 2 > bytes.len() / 2;
    if wide {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// The value after `REG_SZ` or `REG_EXPAND_SZ` on one `reg query` line.
#[cfg(any(windows, test))]
fn path_value(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("Path")
        .or_else(|| trimmed.strip_prefix("PATH"))?;
    if !rest.starts_with(|ch: char| ch.is_whitespace()) {
        return None;
    }
    let type_at = rest.find("REG_EXPAND_SZ").or_else(|| rest.find("REG_SZ"))?;
    let after_type = if rest[type_at..].starts_with("REG_EXPAND_SZ") {
        &rest[type_at + "REG_EXPAND_SZ".len()..]
    } else {
        &rest[type_at + "REG_SZ".len()..]
    };
    let value = after_type.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_output_decodes_utf16() {
        let text = "    Path    REG_SZ    C:\\Program Files\\ffmpeg\\bin\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let decoded = decode_reg_output(&bytes);
        assert_eq!(
            path_value(decoded.lines().next().unwrap()).as_deref(),
            Some(r"C:\Program Files\ffmpeg\bin")
        );
        assert_eq!(
            decode_reg_output(b"    Path    REG_SZ    C:\\Windows\\System32\r\n")
                .lines()
                .next()
                .and_then(path_value)
                .as_deref(),
            Some(r"C:\Windows\System32")
        );
    }

    #[test]
    fn registry_path_keeps_spaces() {
        let line = "    Path    REG_SZ    C:\\Program Files\\ffmpeg\\bin;C:\\Windows";
        assert_eq!(
            path_value(line).as_deref(),
            Some(r"C:\Program Files\ffmpeg\bin;C:\Windows")
        );
        let expanded = "    Path    REG_EXPAND_SZ    %SystemRoot%\\System32";
        assert_eq!(
            path_value(expanded).as_deref(),
            Some(r"%SystemRoot%\System32")
        );
    }

    #[test]
    fn user_ffmpeg_dir_is_prepended_once() {
        let dir = r"C:\Users\Lyam\AppData\Local\Microsoft\WinGet\Packages\yt-dlp.FFmpeg_x\bin";
        let merged = merge_windows_path(&format!(r"C:\Windows\system32;{dir}"), &[dir.into()]);
        let parts: Vec<_> = merged.split(';').collect();
        assert_eq!(parts[0], dir);
        assert_eq!(
            parts.iter().filter(|part| part.contains("FFmpeg")).count(),
            1
        );
    }

    #[test]
    fn libopus_is_preferred_then_ffmpeg() {
        assert_eq!(
            selected_backend(&BackendProbe {
                libopus: true,
                ffmpeg: true,
                gstreamer: true,
            }),
            MediaBackend::Libopus
        );
        assert_eq!(
            selected_backend(&BackendProbe {
                libopus: false,
                ffmpeg: true,
                gstreamer: true,
            }),
            MediaBackend::Ffmpeg
        );
        assert_eq!(
            selected_backend(&BackendProbe {
                libopus: false,
                ffmpeg: false,
                gstreamer: true,
            }),
            MediaBackend::Gstreamer
        );
        assert_eq!(
            selected_backend(&BackendProbe {
                libopus: false,
                ffmpeg: false,
                gstreamer: false,
            }),
            MediaBackend::Libopus
        );
        let probe = BackendProbe {
            libopus: true,
            ffmpeg: true,
            gstreamer: true,
        };
        assert_eq!(
            pick_backend(
                &[
                    MediaBackend::Gstreamer,
                    MediaBackend::Ffmpeg,
                    MediaBackend::Libopus,
                ],
                &probe,
            ),
            MediaBackend::Gstreamer
        );
        assert_eq!(
            pick_backend(
                &[
                    MediaBackend::Ffmpeg,
                    MediaBackend::Libopus,
                    MediaBackend::Gstreamer,
                ],
                &BackendProbe {
                    libopus: true,
                    ffmpeg: false,
                    gstreamer: true,
                },
            ),
            MediaBackend::Libopus
        );
        let probe = BackendProbe {
            libopus: true,
            ffmpeg: true,
            gstreamer: false,
        };
        let (_, audio) = offer_from(probe, MediaBackend::Libopus, false, false);
        let opus = audio.iter().find(|row| row.id == AUDIO_OPUS).unwrap();
        assert!(opus.cpu);
        assert!(opus.libopus);
        assert!(opus.ffmpeg);
        assert!(!opus.gstreamer);
        let only_gst = BackendProbe {
            libopus: false,
            ffmpeg: false,
            gstreamer: true,
        };
        let (_, audio) = offer_from(only_gst, MediaBackend::Libopus, false, false);
        let opus = audio.iter().find(|row| row.id == AUDIO_OPUS).unwrap();
        assert!(!opus.cpu);
        assert!(!opus.libopus);
        assert!(opus.gstreamer);
    }

    #[test]
    fn pyrowave_needs_the_library_and_vulkan() {
        assert!(!pyrowave_ready_with(true, false, false));
        assert!(!pyrowave_ready_with(false, true, false));
        assert!(pyrowave_ready_with(true, true, false));
        assert!(pyrowave_ready_with(false, false, true));
        assert!(!pyrowave_ready_with(false, false, false));
    }

    #[test]
    fn a_library_file_in_the_program_directory_counts() {
        let dir = std::env::temp_dir().join(format!("etherquic-opus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libopus.dll"), b"").unwrap();
        assert!(file_named_in(&["libopus.dll", "opus.dll"], &[dir.clone()]));
        assert!(!file_named_in(&["libopus.dll"], &[dir.join("missing")]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
