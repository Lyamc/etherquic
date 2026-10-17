//! Primary L2 path on Windows: the in-tree Rust NDIS filter (`membrane_ndis`).
//!
//! Opens `\\.\MembraneNdis` and exchanges raw Ethernet frames through
//! `IOCTL_L2_READ_PACKETS` / `IOCTL_L2_WRITE_PACKET`. If the device is not
//! there, callers fall back to pcap.

#![cfg(windows)]
#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security: *mut core::ffi::c_void,
        creation: u32,
        flags: u32,
        template: HANDLE,
    ) -> HANDLE;
}
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl};

const FILE_DEVICE_L2_CAP: u32 = 0x0000_8888;
const METHOD_BUFFERED: u32 = 0;
const FILE_READ_ACCESS: u32 = 1;
const FILE_WRITE_ACCESS: u32 = 2;

const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

const IOCTL_L2_READ_PACKETS: u32 =
    ctl_code(FILE_DEVICE_L2_CAP, 0x804, METHOD_BUFFERED, FILE_READ_ACCESS);
const IOCTL_L2_WRITE_PACKET: u32 = ctl_code(
    FILE_DEVICE_L2_CAP,
    0x805,
    METHOD_BUFFERED,
    FILE_WRITE_ACCESS,
);

pub struct NdisLink {
    handle: HANDLE,
}

// The handle is used only on the L2 IO thread that owns this value.
unsafe impl Send for NdisLink {}

impl NdisLink {
    /// `Ok(None)` when the Rust NDIS device is not loaded. Any other open
    /// failure is returned so a permissions problem is not hidden by pcap.
    pub fn try_open() -> io::Result<Option<Self>> {
        let name: Vec<u16> = r"\\.\MembraneNdis".encode_utf16().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            // 2 = file not found, 3 = path not found: driver device is absent.
            if matches!(err.raw_os_error(), Some(2) | Some(3)) {
                return Ok(None);
            }
            return Err(err);
        }
        Ok(Some(Self { handle }))
    }

    pub(crate) fn raw_handle(&self) -> HANDLE {
        self.handle
    }

    /// Unblock a read that is waiting on this handle, including one issued by
    /// another thread. A null overlapped pointer cancels every pending call.
    pub(crate) fn cancel_io(handle: HANDLE) {
        unsafe {
            CancelIoEx(handle, std::ptr::null());
        }
    }

    pub fn send(&self, frame: &[u8]) -> io::Result<()> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_L2_WRITE_PACKET,
                frame.as_ptr() as *mut _,
                frame.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        crate::capture::note_ether(crate::capture::DIR_SEND, frame);
        Ok(())
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_L2_READ_PACKETS,
                std::ptr::null_mut(),
                0,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // 259 = ERROR_NO_MORE_ITEMS, an empty queue. The same call can
            // also wait inside the driver until a frame arrives. The send
            // thread must not be the one inside this call.
            if err.raw_os_error() == Some(259) {
                return Ok(None);
            }
            return Err(err);
        }
        if returned == 0 {
            Ok(None)
        } else {
            let n = returned as usize;
            let noted = n.min(buf.len());
            crate::capture::note_ether(crate::capture::DIR_RECV, &buf[..noted]);
            Ok(Some(n))
        }
    }
}

impl Drop for NdisLink {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
