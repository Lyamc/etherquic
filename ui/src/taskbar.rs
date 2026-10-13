//! Taskbar button for the host window.
//!
//! A normal GPUI window is created with `WS_EX_APPWINDOW`, so the shell gives
//! it a taskbar button. Idle and player mode strip that bit and set
//! `WS_EX_TOOLWINDOW`. The shell rebuilds the button when the window is
//! hidden and shown again. Hosting puts `WS_EX_APPWINDOW` back.
//! There is no notification-area icon.

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

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
        WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    let hwnd = hwnd as *mut c_void;
    unsafe {
        // The HWND came from the live GPUI window. Hide first so the shell
        // drops the old button, then publish the style, then show it again.
        ShowWindow(hwnd, SW_HIDE);
        let mut style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if on {
            style |= WS_EX_APPWINDOW as isize;
            style &= !(WS_EX_TOOLWINDOW as isize);
        } else {
            style &= !(WS_EX_APPWINDOW as isize);
            style |= WS_EX_TOOLWINDOW as isize;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        ShowWindow(hwnd, SW_SHOW);
    }
}

#[cfg(not(windows))]
pub fn apply(_hwnd: isize, _on: bool) {}
