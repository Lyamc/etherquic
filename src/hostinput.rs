//! Which inputs this Windows PC has, for the discovery capabilities record.

use crate::discover::{INPUT_KEYBOARD, INPUT_MOUSE, INPUT_PEN, INPUT_TOUCH};

/// Keyboard and mouse from the raw input device list, touch and pen from
/// `SM_DIGITIZER`. A remote-desktop or virtual device counts too.
#[allow(unsafe_code)]
pub fn windows_input_flags() -> u16 {
    use windows_sys::Win32::UI::Input::{
        GetRawInputDeviceList, RAWINPUTDEVICELIST, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_DIGITIZER};

    const NID_INTEGRATED_TOUCH: i32 = 0x01;
    const NID_EXTERNAL_TOUCH: i32 = 0x02;
    const NID_INTEGRATED_PEN: i32 = 0x04;
    const NID_EXTERNAL_PEN: i32 = 0x08;

    let mut flags = 0u16;
    let size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0u32;
    // SAFETY: a null list with a valid count pointer only asks for the count.
    let first = unsafe { GetRawInputDeviceList(std::ptr::null_mut(), &mut count, size) };
    if first != u32::MAX && count > 0 {
        // SAFETY: RAWINPUTDEVICELIST is plain data; all-zero is a valid value.
        let mut list: Vec<RAWINPUTDEVICELIST> = vec![unsafe { std::mem::zeroed() }; count as usize];
        // SAFETY: `list` has room for `count` entries of `size` bytes.
        let got = unsafe { GetRawInputDeviceList(list.as_mut_ptr(), &mut count, size) };
        if got != u32::MAX {
            for device in list.iter().take(got as usize) {
                if device.dwType == RIM_TYPEKEYBOARD {
                    flags |= INPUT_KEYBOARD;
                } else if device.dwType == RIM_TYPEMOUSE {
                    flags |= INPUT_MOUSE;
                }
            }
        }
    }
    // SAFETY: GetSystemMetrics takes no pointers.
    let digitizer = unsafe { GetSystemMetrics(SM_DIGITIZER) };
    if digitizer & (NID_INTEGRATED_TOUCH | NID_EXTERNAL_TOUCH) != 0 {
        flags |= INPUT_TOUCH;
    }
    if digitizer & (NID_INTEGRATED_PEN | NID_EXTERNAL_PEN) != 0 {
        flags |= INPUT_PEN;
    }
    flags
}
