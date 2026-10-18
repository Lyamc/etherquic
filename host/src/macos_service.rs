//! LaunchDaemon for the macOS host (`dev.etherquic.host`).
//!
//! The installer copies this binary (and the dylibs it loads) into
//! `/Library/Application Support/etherquic`, which root owns, then bootstraps
//! `/Library/LaunchDaemons/dev.etherquic.host.plist`. The GUI can prompt for
//! an administrator password and run `--install-service`. The optional
//! component in the `.pkg` runs the same command after the app is copied to
//! `/Applications`.

use std::fs;
#[cfg(target_os = "macos")]
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

pub const LABEL: &str = "dev.etherquic.host";
pub const PLIST_NAME: &str = "dev.etherquic.host.plist";
pub const SUPPORT_DIR: &str = "/Library/Application Support/etherquic";
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/dev.etherquic.host.plist";

pub fn support_dir() -> PathBuf {
    PathBuf::from(SUPPORT_DIR)
}

pub fn plist_path() -> PathBuf {
    PathBuf::from(PLIST_PATH)
}

/// True when the LaunchDaemon plist is on disk. launchd may still be starting.
pub fn plist_present() -> bool {
    plist_path().is_file()
}

pub fn program_path() -> PathBuf {
    support_dir().join("MacOS").join("etherquic-host")
}

/// LaunchDaemon plist. `program` is the copied host binary.
pub fn render_plist(program: &Path, certs: &Path, config: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ETHERQUIC_CERTS</key>
        <string>{}</string>
        <key>ETHERQUIC_CONFIG</key>
        <string>{}</string>
    </dict>
    <key>StandardOutPath</key>
    <string>{SUPPORT_DIR}/launchd.out.log</string>
    <key>StandardErrorPath</key>
    <string>{SUPPORT_DIR}/launchd.err.log</string>
</dict>
</plist>
"#,
        program.display(),
        certs.display(),
        config.display(),
    )
}

/// Files to copy from this bundle layout into the support directory.
///
/// `exe` is `…/Contents/MacOS/etherquic-host` (or a lone binary next to the
/// certs). Frameworks sit at `…/Contents/Frameworks`, which becomes
/// `…/MacOS/../Frameworks` the same way the copied tree is laid out.
pub fn collect_copies(exe: &Path) -> Vec<(PathBuf, PathBuf)> {
    let dest = support_dir();
    let mut copies = vec![(exe.to_path_buf(), dest.join("MacOS").join("etherquic-host"))];
    let Some(dir) = exe.parent() else {
        return copies;
    };
    for name in ["cert.pem", "key.pem", "cjxl"] {
        let from = dir.join(name);
        if from.is_file() {
            copies.push((from, dest.join("MacOS").join(name)));
        }
    }
    let frameworks = dir.join("..").join("Frameworks");
    if let Ok(entries) = fs::read_dir(&frameworks) {
        for entry in entries.flatten() {
            let from = entry.path();
            if from.is_file() {
                copies.push((from, dest.join("Frameworks").join(entry.file_name())));
            }
        }
    }
    copies
}

/// Copy this process into the support directory and load the LaunchDaemon.
/// Must run as root.
#[cfg(target_os = "macos")]
pub fn install() -> Result<(), String> {
    require_root()?;
    let exe = std::env::current_exe().map_err(|err| format!("current exe: {err}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let copies = collect_copies(&exe);
    let has_cert = copies.iter().any(|(_, to)| {
        to.file_name()
            .is_some_and(|name| name == "cert.pem")
    });
    let has_key = copies.iter().any(|(_, to)| {
        to.file_name().is_some_and(|name| name == "key.pem")
    });
    if !has_cert || !has_key {
        return Err(format!(
            "missing cert.pem or key.pem next to {}",
            exe.display()
        ));
    }
    let dest = support_dir();
    fs::create_dir_all(dest.join("MacOS"))
        .map_err(|err| format!("create {}: {err}", dest.display()))?;
    fs::create_dir_all(dest.join("Frameworks"))
        .map_err(|err| format!("create {}: {err}", dest.display()))?;
    for (from, to) in &copies {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", parent.display()))?;
        }
        fs::copy(from, to).map_err(|err| format!("copy {} → {}: {err}", from.display(), to.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if to.ends_with("etherquic-host") || to.ends_with("cjxl") {
                0o755
            } else {
                0o644
            };
            fs::set_permissions(to, fs::Permissions::from_mode(mode))
                .map_err(|err| format!("chmod {}: {err}", to.display()))?;
        }
    }
    let program = program_path();
    let certs = dest.join("MacOS");
    let config = dest.join("host.conf");
    let body = render_plist(&program, &certs, &config);
    let plist = plist_path();
    let _ = launchctl(&["bootout", &format!("system/{LABEL}")]);
    fs::write(&plist, body).map_err(|err| format!("write {}: {err}", plist.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&plist, fs::Permissions::from_mode(0o644))
            .map_err(|err| format!("chmod {}: {err}", plist.display()))?;
    }
    launchctl(&["bootstrap", "system", plist.to_str().unwrap_or(PLIST_PATH)])?;
    let _ = launchctl(&["kickstart", "-k", &format!("system/{LABEL}")]);
    println!("installed {LABEL}");
    println!("address file {}/address.txt", dest.display());
    Ok(())
}

/// Unload the LaunchDaemon. `purge` also removes host.conf, logs, and address.
#[cfg(target_os = "macos")]
pub fn uninstall(purge: bool) -> Result<(), String> {
    require_root()?;
    let _ = launchctl(&["bootout", &format!("system/{LABEL}")]);
    remove_path(&plist_path())?;
    let dest = support_dir();
    remove_path(&dest.join("MacOS"))?;
    remove_path(&dest.join("Frameworks"))?;
    remove_path(&dest.join("launchd.out.log"))?;
    remove_path(&dest.join("launchd.err.log"))?;
    if purge {
        remove_path(&dest)?;
    }
    println!("removed {LABEL}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_path(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) if err.kind() == ErrorKind::IsADirectory => {}
        Err(err) => return Err(format!("remove {}: {err}", path.display())),
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("remove {}: {err}", path.display())),
    }
}

#[cfg(target_os = "macos")]
fn require_root() -> Result<(), String> {
    if running_as_root() {
        Ok(())
    } else {
        Err(
            "etherquic-host --install-service needs root. The app can prompt for an administrator password."
                .into(),
        )
    }
}

#[cfg(target_os = "macos")]
fn running_as_root() -> bool {
    #[cfg(unix)]
    {
        // Effective uid 0 is root. The GUI elevates through osascript.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<(), String> {
    let output = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|err| format!("launchctl: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "launchctl {} failed\n{stderr}{stdout}",
        args.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_the_copied_binary_as_root_keepalive() {
        let program = PathBuf::from("/Library/Application Support/etherquic/MacOS/etherquic-host");
        let certs = PathBuf::from("/Library/Application Support/etherquic/MacOS");
        let config = PathBuf::from("/Library/Application Support/etherquic/host.conf");
        let body = render_plist(&program, &certs, &config);
        assert!(body.contains("<string>dev.etherquic.host</string>"));
        assert!(body.contains("/Library/Application Support/etherquic/MacOS/etherquic-host"));
        assert!(body.contains("ETHERQUIC_CERTS"));
        assert!(body.contains("ETHERQUIC_CONFIG"));
        assert!(body.contains("<key>RunAtLoad</key>"));
        assert!(body.contains("<key>KeepAlive</key>"));
        assert!(body.contains("<integer>5</integer>"));
        assert!(!body.contains("etherquic-host --console"));
        assert_eq!(plist_path(), PathBuf::from(PLIST_PATH));
        assert_eq!(program_path(), support_dir().join("MacOS").join("etherquic-host"));
        assert_eq!(PLIST_NAME, "dev.etherquic.host.plist");
        assert!(!plist_present() || Path::new(PLIST_PATH).is_file());
    }

    #[test]
    fn copies_follow_the_app_bundle_layout() {
        let root = std::env::temp_dir().join(format!(
            "eq-macos-svc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let macos = root.join("Contents").join("MacOS");
        let frameworks = root.join("Contents").join("Frameworks");
        fs::create_dir_all(&macos).unwrap();
        fs::create_dir_all(&frameworks).unwrap();
        fs::write(macos.join("etherquic-host"), b"bin").unwrap();
        fs::write(macos.join("cert.pem"), b"cert").unwrap();
        fs::write(macos.join("key.pem"), b"key").unwrap();
        fs::write(macos.join("cjxl"), b"cjxl").unwrap();
        fs::write(frameworks.join("libopus.0.dylib"), b"opus").unwrap();
        let copies = collect_copies(&macos.join("etherquic-host"));
        let dests: Vec<String> = copies
            .iter()
            .map(|(_, to)| to.to_string_lossy().replace('\\', "/"))
            .collect();
        assert!(dests
            .iter()
            .any(|p| p.ends_with("/etherquic/MacOS/etherquic-host")));
        assert!(dests.iter().any(|p| p.ends_with("/etherquic/MacOS/cert.pem")));
        assert!(dests.iter().any(|p| p.ends_with("/etherquic/MacOS/key.pem")));
        assert!(dests.iter().any(|p| p.ends_with("/etherquic/MacOS/cjxl")));
        assert!(dests
            .iter()
            .any(|p| p.ends_with("/etherquic/Frameworks/libopus.0.dylib")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_lone_binary_still_takes_certs_beside_it() {
        let dir = std::env::temp_dir().join(format!(
            "eq-macos-lone-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("etherquic-host"), b"bin").unwrap();
        fs::write(dir.join("cert.pem"), b"c").unwrap();
        fs::write(dir.join("key.pem"), b"k").unwrap();
        let copies = collect_copies(&dir.join("etherquic-host"));
        assert_eq!(copies.len(), 3);
        let _ = fs::remove_dir_all(dir);
    }
}
