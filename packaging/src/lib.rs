//! Build the EtherQuic Play application installers.
//!
//! The Windows package is an MSI with one product name and one UpgradeCode.
//! A later build of the same product passes a higher ProductVersion and a new
//! ProductCode, which Windows Installer installs as a major upgrade.
//! The Android package is a signed APK with one application id. A later build
//! passes a higher version code so the package manager updates that id in place.
//! The manifest targets SDK 35, allows `INTERNET` and `ACCESS_NETWORK_STATE`,
//! and launches `android.app.NativeActivity` with `libetherquic_android.so`
//! so the phone runs the same GPUI Desk as the desktop window. Codec
//! libraries the host can send (`libopus.so`, `libpyrowave-shared.so`,
//! `libffmpeg.so` with H.264/HEVC/AV1/OpenAPV/JPEG-XS) are packed next to it
//! as unversioned `lib/<abi>/lib*.so` so the package manager extracts them.
//! The default abi is `arm64-v8a`. `ETHERQUIC_ANDROID_ABI=x86_64` is the
//! Waydroid image on a PC, and that build does not pack the arm64 codecs.
//!
//! Neither package is the Membrane NDIS driver under `windows/`.

#![forbid(unsafe_code)]

mod axml;
#[allow(dead_code)]
mod dex;
mod hash;
mod zipstore;

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const PRODUCT_NAME: &str = "EtherQuic Play";
pub const UPGRADE_CODE: &str = "C4E8B1A2-7D3F-4E15-9A60-1F2B3C4D5E6F";
pub const APPLICATION_ID: &str = "dev.etherquic.play";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn message(text: impl Into<String>) -> Self {
        Error(text.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err(message: impl Into<String>) -> Error {
    Error(message.into())
}

/// Write one per-user MSI for `version` (for example `1.0.0`).
pub fn build_windows_installer(output: &Path, version: &str) -> Result<()> {
    check_product_version(version)?;
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    if output.exists() {
        fs::remove_file(output)?;
    }

    let work = WorkDir::new("msi")?;
    let stamp = work.path.join("installed-version.txt");
    let play_exe = work.path.join("etherquic-play.exe");
    fs::write(&stamp, format!("{version}\n"))?;
    stage_play_exe(&play_exe, version)?;

    let wxs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("windows/etherquic-play.wxs");
    run(
        "wixl",
        &[
            arg("-a"),
            arg("x64"),
            arg("-o"),
            arg(output),
            arg("-D"),
            arg(format!("ProductVersion={version}")),
            arg("-D"),
            arg(format!("VersionStamp={}", stamp.display())),
            arg("-D"),
            arg(format!("PlayExe={}", play_exe.display())),
            arg(&wxs),
        ],
    )?;
    if !output.is_file() {
        return Err(err(format!(
            "wixl reported success but {} was not written",
            output.display()
        )));
    }
    Ok(())
}

/// Write one signed APK for `version_code` / `version_name`.
pub fn build_android_package(output: &Path, version_code: u32, version_name: &str) -> Result<()> {
    check_version_name(version_name)?;
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    if output.exists() {
        fs::remove_file(output)?;
    }

    let manifest = axml::manifest(APPLICATION_ID, version_code, version_name);
    let natives = android_libraries();
    let mut files: Vec<(&str, &[u8])> = vec![("AndroidManifest.xml", manifest.as_slice())];
    files.extend(
        natives
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice())),
    );
    let unsigned_bytes = zipstore::store_zip(&files);
    let work = WorkDir::new("apk")?;
    let unsigned = work.path.join("unsigned.apk");
    fs::write(&unsigned, unsigned_bytes)?;

    // Sideload key beside this crate. A later package updates the installed
    // application only when it is signed by this same certificate.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let key = root.join("android/debug.pk8");
    let cert = root.join("android/debug.crt");
    run(
        "apksigner",
        &[
            arg("sign"),
            arg("--key"),
            arg(&key),
            arg("--cert"),
            arg(&cert),
            arg("--min-sdk-version"),
            arg("21"),
            arg("--v1-signing-enabled"),
            arg("true"),
            arg("--v2-signing-enabled"),
            arg("true"),
            arg("--v3-signing-enabled"),
            arg("true"),
            arg("--v4-signing-enabled"),
            arg("false"),
            arg("--out"),
            arg(output),
            arg(&unsigned),
        ],
    )?;
    if !output.is_file() {
        return Err(err(format!(
            "apksigner reported success but {} was not written",
            output.display()
        )));
    }
    Ok(())
}

/// `ETHERQUIC_ANDROID_ABI` selects the APK directory. Waydroid on a PC is
/// `x86_64`. Phones stay `arm64-v8a`.
fn jni_abi() -> String {
    std::env::var("ETHERQUIC_ANDROID_ABI").unwrap_or_else(|_| "arm64-v8a".into())
}

fn jni_root() -> String {
    format!("lib/{}", jni_abi())
}

/// NativeActivity plus codec `.so` files the player dlopens or execs.
///
/// Android's package manager only extracts unversioned `lib*.so` names, so
/// versioned SONAMEs (`libopus.so.0`) are skipped. `ETHERQUIC_ANDROID_SO`
/// names the NativeActivity library; `ETHERQUIC_ANDROID_LIBS` is a directory
/// of extra `lib*.so` files (opus, PyroWave, ffmpeg, and their deps).
fn android_libraries() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    push_jni(
        &mut out,
        &mut seen,
        "libetherquic_android.so",
        android_activity_library(),
    );
    for dir in android_codec_dirs() {
        for (name, bytes) in android_jni_files_from(&dir) {
            let file = name.rsplit('/').next().unwrap_or(&name).to_string();
            push_jni(&mut out, &mut seen, &file, Some(bytes));
        }
    }
    out
}

fn push_jni(
    out: &mut Vec<(String, Vec<u8>)>,
    seen: &mut BTreeSet<String>,
    file: &str,
    bytes: Option<Vec<u8>>,
) {
    let Some(bytes) = bytes else {
        return;
    };
    if bytes.is_empty() {
        return;
    }
    let name = format!("{}/{}", jni_root(), file);
    if seen.insert(name.clone()) {
        out.push((name, bytes));
    }
}

fn android_activity_library() -> Option<Vec<u8>> {
    let path = std::env::var_os("ETHERQUIC_ANDROID_SO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/aarch64-linux-android/release/libetherquic_android.so")
        });
    let bytes = fs::read(&path).ok()?;
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

fn android_codec_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("ETHERQUIC_ANDROID_LIBS") {
        dirs.push(PathBuf::from(path));
    }
    if jni_abi() == "arm64-v8a" {
        dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("android/jni/arm64-v8a"));
    }
    dirs
}

/// Unversioned `lib*.so` files under `dir`, as APK zip paths.
fn android_jni_files_from(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        names.push(entry.path());
    }
    names.sort();
    for path in names {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !jni_so_name(name) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        out.push((format!("{}/{}", jni_root(), name), bytes));
    }
    out
}

fn jni_so_name(name: &str) -> bool {
    name.starts_with("lib") && name.ends_with(".so") && !name[3..name.len() - 3].contains('.')
}

fn stage_play_exe(dest: &Path, version: &str) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../play/target/x86_64-pc-windows-msvc/release/etherquic-play.exe");
    if source.is_file() {
        fs::copy(&source, dest)?;
    } else {
        fs::write(dest, format!("EtherQuic Play {version}\n"))?;
    }
    Ok(())
}

fn check_product_version(version: &str) -> Result<()> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(err(format!(
            "windows product version {version} must be major.minor or major.minor.build"
        )));
    }
    let limits = [255u64, 255, 65535];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(err(format!(
                "windows product version {version} has a non-numeric component"
            )));
        }
        let number: u64 = part.parse().map_err(|_| {
            err(format!(
                "windows product version {version} has a non-numeric component"
            ))
        })?;
        if number > limits[index] {
            return Err(err(format!(
                "windows product version {version} has a component above {}",
                limits[index]
            )));
        }
    }
    Ok(())
}

fn check_version_name(version_name: &str) -> Result<()> {
    if version_name.is_empty() || version_name.chars().any(|ch| ch == '\0' || ch.is_control()) {
        return Err(err(
            "android version name must be a non-empty string without control characters",
        ));
    }
    Ok(())
}

fn run(program: &str, args: &[OsString]) -> Result<()> {
    let output = Command::new(program).args(args).output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            err(format!(
                "could not run {program}: {error}. Enter `nix-shell nix/installers.nix` so the installer tools are on PATH."
            ))
        } else {
            err(format!("could not run {program}: {error}"))
        }
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(err(format!(
        "{program} exited {}\n{stderr}{stdout}",
        output.status
    )))
}

fn arg(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

struct WorkDir {
    path: PathBuf,
}

impl WorkDir {
    fn new(label: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "etherquic-inst-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jni_so_names_are_unversioned_lib_dot_so() {
        assert!(jni_so_name("libopus.so"));
        assert!(jni_so_name("libpyrowave-shared.so"));
        assert!(jni_so_name("libffmpeg.so"));
        assert!(jni_so_name("libc++_shared.so"));
        assert!(!jni_so_name("libopus.so.0"));
        assert!(!jni_so_name("libpyrowave-shared.so.1"));
        assert!(!jni_so_name("ffmpeg"));
        assert!(!jni_so_name("notes.txt"));
    }

    #[test]
    fn android_jni_dir_packs_codec_libraries_and_skips_sonames() {
        let root = std::env::temp_dir().join(format!(
            "etherquic-android-jni-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).expect("jni fixture dir");
        fs::write(root.join("libopus.so"), b"opus").unwrap();
        fs::write(root.join("libopus.so.0"), b"skip-soname").unwrap();
        fs::write(root.join("libpyrowave-shared.so"), b"pyro").unwrap();
        fs::write(root.join("libffmpeg.so"), b"ffmpeg").unwrap();
        fs::write(root.join("notes.txt"), b"no").unwrap();
        let names: Vec<String> = android_jni_files_from(&root)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            names,
            vec![
                "lib/arm64-v8a/libffmpeg.so".to_string(),
                "lib/arm64-v8a/libopus.so".to_string(),
                "lib/arm64-v8a/libpyrowave-shared.so".to_string(),
            ]
        );
    }
}
