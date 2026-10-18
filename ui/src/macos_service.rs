//! Prompt for, and detect, the macOS host LaunchDaemon.
//!
//! The GUI does not need root. It asks osascript for an administrator
//! password, then runs `etherquic-host --install-service` from this app
//! bundle. The plist path is the same one the host installer writes.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub const PLIST_PATH: &str = "/Library/LaunchDaemons/dev.etherquic.host.plist";

/// True when the LaunchDaemon plist is on disk.
pub fn service_installed() -> bool {
    Path::new(PLIST_PATH).is_file()
}

/// `Contents/MacOS/etherquic-host` next to this GUI binary.
pub fn host_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let host = dir.join("etherquic-host");
    host.is_file().then_some(host)
}

/// AppleScript that runs `--install-service` after the password dialog.
pub fn install_script(host: &Path) -> String {
    format!(
        r#"do shell script (quoted form of "{}" & " --install-service") with administrator privileges"#,
        applescript_string(&host.display().to_string())
    )
}

/// AppleScript that unloads the LaunchDaemon. `host.conf` is kept.
pub fn uninstall_script(host: &Path) -> String {
    format!(
        r#"do shell script (quoted form of "{}" & " --uninstall-service") with administrator privileges"#,
        applescript_string(&host.display().to_string())
    )
}

fn applescript_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Show the macOS password dialog and install. Blocks until it finishes.
pub fn install_with_prompt() -> Result<(), String> {
    run_osascript(&install_script(&host_binary().ok_or_else(missing_host)?))
}

pub fn uninstall_with_prompt() -> Result<(), String> {
    run_osascript(&uninstall_script(&host_binary().ok_or_else(missing_host)?))
}

fn missing_host() -> String {
    "etherquic-host is not in this app bundle. Use the etherquic installer package, or rebuild with packaging/macos/make-app.sh.".into()
}

fn run_osascript(script: &str) -> Result<(), String> {
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map_err(|err| format!("osascript: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = format!("{stderr}{stdout}");
    if text.to_ascii_lowercase().contains("user canceled")
        || text.to_ascii_lowercase().contains("user cancelled")
        || output.status.code() == Some(-128)
    {
        return Err("Administrator password was cancelled.".into());
    }
    Err(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_script_quotes_a_path_with_spaces() {
        let host = PathBuf::from("/Applications/etherquic.app/Contents/MacOS/etherquic-host");
        let script = install_script(&host);
        assert!(script.contains("quoted form of"));
        assert!(script.contains("/Applications/etherquic.app/Contents/MacOS/etherquic-host"));
        assert!(script.contains("--install-service"));
        assert!(script.contains("with administrator privileges"));
        assert!(!script.contains("--console"));
    }

    #[test]
    fn uninstall_script_keeps_host_conf() {
        let host = PathBuf::from("/Applications/etherquic.app/Contents/MacOS/etherquic-host");
        let script = uninstall_script(&host);
        assert!(script.contains("--uninstall-service"));
        assert!(!script.contains("--purge"));
    }

    #[test]
    fn applescript_escapes_quotes() {
        let host = PathBuf::from(r#"/tmp/"odd"/etherquic-host"#);
        let script = install_script(&host);
        assert!(script.contains(r#"/tmp/\"odd\"/etherquic-host"#));
    }
}
