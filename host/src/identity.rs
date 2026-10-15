//! MachineGuid from HKLM, used only when host.conf has no MAC yet.

#[cfg(windows)]
pub fn machine_id() -> String {
    machine_guid()
        .unwrap_or_else(|| std::env::var("COMPUTERNAME").unwrap_or_else(|_| "etherquic".into()))
}

#[cfg(not(windows))]
pub fn machine_id() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "etherquic".into())
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
