//! Stable machine id used only when host.conf has no MAC yet.

#[cfg(windows)]
pub fn machine_id() -> String {
    machine_guid()
        .unwrap_or_else(|| std::env::var("COMPUTERNAME").unwrap_or_else(|_| "etherquic".into()))
}

#[cfg(not(windows))]
pub fn machine_id() -> String {
    if let Ok(text) = std::fs::read_to_string("/etc/machine-id") {
        let text = text.trim();
        if !text.is_empty() {
            return text.to_string();
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(uuid) = io_platform_uuid() {
        return uuid;
    }
    computer_name()
}

/// Hostname a discovery offer shows.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "etherquic".into())
}

/// `"IOPlatformUUID" = "A1B2C3D4-…"` from `ioreg -rd1 -c IOPlatformExpertDevice`.
pub fn parse_io_platform_uuid(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if !line.contains("IOPlatformUUID") {
            continue;
        }
        let Some(end) = line.rfind('"') else {
            continue;
        };
        let Some(start) = line[..end].rfind('"') else {
            continue;
        };
        let uuid = &line[start + 1..end];
        if uuid.len() >= 8 {
            return Some(uuid.to_string());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn io_platform_uuid() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    parse_io_platform_uuid(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(windows)]
fn machine_guid() -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
    };

    let subkey: Vec<u16> = OsStr::new(r"SOFTWARE\Microsoft\Cryptography")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut key = ptr::null_mut();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if opened != 0 || key.is_null() {
        return None;
    }
    struct Close(windows_sys::Win32::System::Registry::HKEY);
    impl Drop for Close {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    let _close = Close(key);
    let name: Vec<u16> = OsStr::new("MachineGuid")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = [0u16; 80];
    let mut size = (buf.len() * 2) as u32;
    let mut kind = 0u32;
    let read = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    // REG_SZ is 1.
    if read != 0 || kind != 1 {
        return None;
    }
    let chars = (size as usize / 2).min(buf.len());
    let end = buf[..chars]
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(chars);
    let text = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_platform_uuid_reads_the_quoted_value() {
        let ioreg = r#"
+-o Macbook <class IOPlatformExpertDevice, id 0x100000112, registered, matched, active, busy 0>
    {
      "IOPlatformUUID" = "A1B2C3D4-E5F6-7890-ABCD-EF1234567890"
    }
"#;
        assert_eq!(
            parse_io_platform_uuid(ioreg).as_deref(),
            Some("A1B2C3D4-E5F6-7890-ABCD-EF1234567890")
        );
        assert_eq!(parse_io_platform_uuid("no uuid here"), None);
    }
}
