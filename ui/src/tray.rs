//! Notification-area icon for the etherquic window.
//!
//! The artwork is `assets/etherquic.svg`, rasterized into icon resource 1
//! by `build.rs`. Idle and player mode take the taskbar button away, so this
//! icon is how the window is found again. Closing the window hides it here.
//! Quit is on the icon's menu.

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
        NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, GetCursorPos, GetSystemMetrics, IsIconic, IsWindowVisible, LoadImageW,
        PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, ShowWindow,
        TrackPopupMenu, HICON, HMENU, ICON_BIG, ICON_SMALL, IMAGE_ICON, MF_STRING, SM_CXICON,
        SM_CXSMICON, SW_HIDE, SW_RESTORE, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
        TPM_RIGHTALIGN, WM_CONTEXTMENU, WM_NULL, WM_SETICON, WNDCLASSW,
    };

    const CLASS: &[u16] = &[
        b'E' as u16,
        b't' as u16,
        b'h' as u16,
        b'e' as u16,
        b'r' as u16,
        b'Q' as u16,
        b'u' as u16,
        b'i' as u16,
        b'c' as u16,
        b'T' as u16,
        b'r' as u16,
        b'a' as u16,
        b'y' as u16,
        0,
    ];
    const TIP: &str = "etherquic";
    const CALLBACK: u32 = 0x8000 + 1;
    /// Posted by a test or another tool to ask the app to leave.
    const QUIT_MSG: u32 = 0x8000 + 2;
    const ICON_ID: u32 = 1;
    const CMD_OPEN: usize = 1;
    const CMD_QUIT: usize = 2;
    /// Keyboard select. Shell uses `NIN_SELECT + 1`, which windows-sys does not name.
    const NIN_KEYSELECT: u32 = 1025;
    const NIN_SELECT: u32 = 1024;

    static MAIN: AtomicIsize = AtomicIsize::new(0);
    static TRAY: AtomicIsize = AtomicIsize::new(0);
    static QUIT: AtomicBool = AtomicBool::new(false);
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    static SMALL_ICON: AtomicIsize = AtomicIsize::new(0);
    static BIG_ICON: AtomicIsize = AtomicIsize::new(0);

    pub fn install(main: isize) -> bool {
        MAIN.store(main, Ordering::SeqCst);
        let hwnd = match message_window() {
            Some(hwnd) => hwnd,
            None => return false,
        };
        TRAY.store(hwnd as isize, Ordering::SeqCst);
        let small = load_icon(metric(SM_CXSMICON, 16));
        let big = load_icon(metric(SM_CXICON, 32));
        SMALL_ICON.store(small as isize, Ordering::SeqCst);
        BIG_ICON.store(big as isize, Ordering::SeqCst);
        apply_window_icons(main as HWND, small, big);
        if !add_icon(hwnd, small) {
            let _ = unsafe { DestroyWindow(hwnd) };
            TRAY.store(0, Ordering::SeqCst);
            return false;
        }
        // Windows 11 parks a new icon in the overflow. Promote it, then add
        // again so the shell reads that and shows the icon on the taskbar.
        if promote_tray_icon() {
            let data = blank(hwnd);
            unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            if !add_icon(hwnd, small) {
                let _ = unsafe { DestroyWindow(hwnd) };
                TRAY.store(0, Ordering::SeqCst);
                return false;
            }
        }
        INSTALLED.store(true, Ordering::SeqCst);
        true
    }

    pub fn installed() -> bool {
        INSTALLED.load(Ordering::SeqCst)
    }

    pub fn quit_requested() -> bool {
        QUIT.load(Ordering::SeqCst)
    }

    pub fn take_quit() -> bool {
        QUIT.swap(false, Ordering::SeqCst)
    }

    pub fn hide_main() {
        let hwnd = MAIN.load(Ordering::SeqCst) as HWND;
        if hwnd.is_null() {
            return;
        }
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }

    pub fn remove() {
        let hwnd = TRAY.swap(0, Ordering::SeqCst) as HWND;
        if hwnd.is_null() {
            return;
        }
        let data = blank(hwnd);
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &data);
            DestroyWindow(hwnd);
            destroy_icon(&SMALL_ICON);
            destroy_icon(&BIG_ICON);
        }
        INSTALLED.store(false, Ordering::SeqCst);
    }

    fn metric(index: i32, fallback: i32) -> i32 {
        let value = unsafe { GetSystemMetrics(index) };
        if value > 0 {
            value
        } else {
            fallback
        }
    }

    fn load_icon(edge: i32) -> HICON {
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            LoadImageW(module, 1 as *const u16, IMAGE_ICON, edge, edge, 0)
        }
    }

    fn apply_window_icons(hwnd: HWND, small: HICON, big: HICON) {
        unsafe {
            if !big.is_null() {
                SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as LPARAM);
            }
            if !small.is_null() {
                SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as LPARAM);
            }
        }
    }

    fn add_icon(hwnd: HWND, icon: HICON) -> bool {
        let mut data = blank(hwnd);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = icon;
        let mut tip = [0u16; 128];
        for (slot, unit) in tip.iter_mut().zip(TIP.encode_utf16()) {
            *slot = unit;
        }
        data.szTip = tip;
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
                return false;
            }
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
        true
    }

    /// `true` when a hidden icon was switched to the visible tray.
    fn promote_tray_icon() -> bool {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD,
        };

        let Ok(current) = std::env::current_exe() else {
            return false;
        };
        let current = current.to_string_lossy().to_string();
        let subkey = wide(r"Control Panel\NotifyIconSettings");
        let mut changed = false;
        unsafe {
            let mut root: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE,
                &mut root,
            ) != 0
            {
                return false;
            }
            let mut index = 0u32;
            loop {
                let mut name = [0u16; 64];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(
                    root,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) != 0
                {
                    break;
                }
                index += 1;
                let mut key: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(
                    root,
                    name.as_ptr(),
                    0,
                    KEY_QUERY_VALUE | KEY_SET_VALUE,
                    &mut key,
                ) != 0
                {
                    continue;
                }
                let promote = reg_sz(key, "ExecutablePath")
                    .is_some_and(|path| same_executable(&path, &current))
                    && reg_dword(key, "IsPromoted") != Some(1);
                if promote {
                    let one = 1u32;
                    let value = wide("IsPromoted");
                    if RegSetValueExW(
                        key,
                        value.as_ptr(),
                        0,
                        REG_DWORD,
                        &one as *const u32 as *const u8,
                        4,
                    ) == 0
                    {
                        changed = true;
                    }
                }
                RegCloseKey(key);
            }
            RegCloseKey(root);
        }
        changed
    }

    fn same_executable(stored: &str, current: &str) -> bool {
        if stored.eq_ignore_ascii_case(current) {
            return true;
        }
        const MARKER: &str = r"\etherquic-setup\etherquic.exe";
        let tail = |text: &str| {
            text.len() >= MARKER.len()
                && text[text.len() - MARKER.len()..].eq_ignore_ascii_case(MARKER)
        };
        tail(stored) && tail(current)
    }

    fn reg_sz(key: windows_sys::Win32::System::Registry::HKEY, name: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::RegQueryValueExW;
        let name = wide(name);
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let mut kind = 0u32;
        let err = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                buf.as_mut_ptr() as *mut u8,
                &mut size,
            )
        };
        if err != 0 || kind != 1 {
            return None;
        }
        let chars = (size as usize / 2).min(buf.len());
        let end = buf[..chars]
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(chars);
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    fn reg_dword(key: windows_sys::Win32::System::Registry::HKEY, name: &str) -> Option<u32> {
        use windows_sys::Win32::System::Registry::{RegQueryValueExW, REG_DWORD};
        let name = wide(name);
        let mut value = 0u32;
        let mut size = 4u32;
        let mut kind = 0u32;
        let err = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                &mut value as *mut u32 as *mut u8,
                &mut size,
            )
        };
        if err != 0 || kind != REG_DWORD {
            None
        } else {
            Some(value)
        }
    }

    fn blank(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = ICON_ID;
        data
    }

    fn message_window() -> Option<HWND> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance,
                lpszClassName: CLASS.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                0,
                CLASS.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                -3isize as HWND,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                None
            } else {
                Some(hwnd)
            }
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == CALLBACK {
            on_tray(hwnd, wparam, lparam);
            return 0;
        }
        if msg == QUIT_MSG {
            QUIT.store(true, Ordering::SeqCst);
            return 0;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn on_tray(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let event = (lparam as u32) & 0xFFFF;
        let x = (wparam as i32) as i16 as i32;
        let y = ((wparam >> 16) as i32) as i16 as i32;
        match event {
            NIN_SELECT | NIN_KEYSELECT => toggle_main(),
            WM_CONTEXTMENU => menu(hwnd, x, y),
            _ => {}
        }
    }

    fn toggle_main() {
        let hwnd = MAIN.load(Ordering::SeqCst) as HWND;
        if hwnd.is_null() {
            return;
        }
        unsafe {
            if IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 {
                ShowWindow(hwnd, SW_HIDE);
            } else {
                show_main(hwnd);
            }
        }
    }

    fn show_main(hwnd: HWND) {
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }

    fn menu(hwnd: HWND, x: i32, y: i32) {
        let mut point = POINT { x, y };
        unsafe {
            if point.x == 0 && point.y == 0 {
                GetCursorPos(&mut point);
            }
            let menu: HMENU = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let open = wide("Open");
            let quit = wide("Quit");
            AppendMenuW(menu, MF_STRING, CMD_OPEN, open.as_ptr());
            AppendMenuW(menu, MF_STRING, CMD_QUIT, quit.as_ptr());
            SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            match cmd as usize {
                CMD_OPEN => {
                    let main = MAIN.load(Ordering::SeqCst) as HWND;
                    if !main.is_null() {
                        show_main(main);
                    }
                }
                CMD_QUIT => {
                    QUIT.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn destroy_icon(slot: &AtomicIsize) {
        let icon = slot.swap(0, Ordering::SeqCst) as HICON;
        if !icon.is_null() {
            unsafe {
                DestroyIcon(icon);
            }
        }
    }
}

#[cfg(windows)]
pub use win::{hide_main, install, installed, quit_requested, remove, take_quit};

#[cfg(not(windows))]
pub fn install(_main: isize) -> bool {
    false
}

#[cfg(not(windows))]
pub fn installed() -> bool {
    false
}

#[cfg(not(windows))]
pub fn quit_requested() -> bool {
    false
}

#[cfg(not(windows))]
pub fn take_quit() -> bool {
    false
}

#[cfg(not(windows))]
pub fn hide_main() {}

#[cfg(not(windows))]
pub fn remove() {}
