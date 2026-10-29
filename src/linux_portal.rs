//! Keep this process visible to xdg-desktop-portal on Linux.
//!
//! `CAP_NET_RAW` / `CAP_NET_ADMIN` (file capabilities or systemd
//! `AmbientCapabilities`) make the process undumpable and fail the portal's
//! `cap_ptrace_access_check` against `/proc/<pid>/root`. Screenshot is then
//! denied with "Portal operation not allowed". Plasma has no wlroots
//! screencopy fallback, so the host never captures.
//!
//! After the L2/pcap FD is open, clear those capabilities and restore
//! dumpable. The open socket keeps working; the portal can identify us.

#![cfg(target_os = "linux")]
#![allow(unsafe_code)]

use std::io;

/// Capability version 3 header / data used by `capset(2)`.
#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

// linux/capability.h
const _LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const CAP_NET_ADMIN: u32 = 12;
const CAP_NET_RAW: u32 = 13;

fn cap_mask(cap: u32) -> (usize, u32) {
    let index = (cap / 32) as usize;
    let bit = 1u32 << (cap % 32);
    (index, bit)
}

/// Drop `CAP_NET_*` and set dumpable so xdg-desktop-portal can inspect us.
///
/// Call once the pcap/AF_PACKET FD is already open. Safe to call more than
/// once. No-op when the process never held those capabilities.
pub fn release_net_caps() {
    if let Err(error) = clear_net_caps() {
        eprintln!("etherquic: could not drop CAP_NET_*: {error}");
    }
    // PR_SET_DUMPABLE = 4, SUID_DUMP_USER = 1
    let _ = unsafe { libc::prctl(4, 1isize, 0, 0, 0) };
}

fn clear_net_caps() -> io::Result<()> {
    let mut header = CapHeader {
        version: _LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];

    let rc = unsafe {
        libc::syscall(
            libc::SYS_capget,
            std::ptr::addr_of_mut!(header),
            data.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    for cap in [CAP_NET_ADMIN, CAP_NET_RAW] {
        let (index, bit) = cap_mask(cap);
        if index < data.len() {
            data[index].effective &= !bit;
            data[index].permitted &= !bit;
            data[index].inheritable &= !bit;
        }
    }

    // Also drop from the ambient set so they cannot come back on exec.
    // PR_CAP_AMBIENT = 47, PR_CAP_AMBIENT_LOWER = 3
    for cap in [CAP_NET_ADMIN as i32, CAP_NET_RAW as i32] {
        let _ = unsafe { libc::prctl(47, 3, cap, 0, 0) };
    }

    header = CapHeader {
        version: _LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let rc = unsafe {
        libc::syscall(
            libc::SYS_capset,
            std::ptr::addr_of_mut!(header),
            data.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
