//! Taskbar button for the host window.
//!
//! Idle and player mode take the taskbar button away with `ITaskbarList`.
//! The window stays a normal window, so Alt-Tab can still reach it.
//! `WS_EX_TOOLWINDOW` would remove it from that list. The notification-area
//! icon is `tray`.

use gpui::Window;

#[cfg(windows)]
pub fn hwnd_of(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // `Window::window_handle` is GPUI's own handle. The Win32 HWND is the trait method.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win) => Some(win.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
pub fn hwnd_of(_window: &Window) -> Option<isize> {
    None
}

#[cfg(windows)]
pub fn apply(hwnd: isize, on: bool) {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use windows_sys::core::GUID;
    use windows_sys::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER};
    use windows_sys::Win32::UI::Shell::TaskbarList;

    const COINIT_APARTMENTTHREADED: u32 = 2;
    const IID_ITASKBAR_LIST: GUID = GUID::from_u128(0x56fdf342_fd6d_11d0_958a_006097c9a090);

    #[repr(C)]
    struct Vtbl {
        _query: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        hr_init: unsafe extern "system" fn(*mut c_void) -> i32,
        add_tab: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
        delete_tab: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    }

    static BAR: AtomicIsize = AtomicIsize::new(0);

    let hwnd = hwnd as *mut c_void;
    unsafe {
        let mut bar = BAR.load(Ordering::SeqCst) as *mut c_void;
        if bar.is_null() {
            let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED);
            let mut created = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &TaskbarList,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ITASKBAR_LIST,
                &mut created,
            );
            if hr < 0 || created.is_null() {
                return;
            }
            let vtbl = *(created as *const *const Vtbl);
            if ((*vtbl).hr_init)(created) < 0 {
                ((*vtbl).release)(created);
                return;
            }
            BAR.store(created as isize, Ordering::SeqCst);
            bar = created;
        }
        let vtbl = *(bar as *const *const Vtbl);
        if on {
            ((*vtbl).add_tab)(bar, hwnd);
        } else {
            ((*vtbl).delete_tab)(bar, hwnd);
        }
    }
}

#[cfg(not(windows))]
pub fn apply(_hwnd: isize, _on: bool) {}
