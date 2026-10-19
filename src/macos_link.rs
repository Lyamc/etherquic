//! Raw Ethernet on macOS: BPF receive and AF_NDRV send.
//!
//! libpcap can capture on Darwin, but injects through BPF with a 2048-byte
//! ceiling and without `BIOCSHDRCMPLT`, so the kernel rewrites the source MAC.
//! VpnCloud's TAP path (tun-rs `feth`) uses this split instead: BPF reads
//! frames, `AF_NDRV` writes them. Same sockets on a real `en*` adapter carry
//! EtherType 0x88B6.

#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::OnceLock;

use crate::pcap_link::{LinkError, Result};

const BPF_BUFFER: usize = 131_072;
const BPF_LD: u16 = 0x00;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_H: u16 = 0x08;
const BPF_ABS: u16 = 0x20;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
/// `_IOW('B', 119, u_int)` — not always re-exported by libc.
const BIOCSSEESENT: libc::c_ulong = 0x8004_4277;

#[repr(C)]
struct BpfInsn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct BpfProgram {
    bf_len: u32,
    bf_insns: *mut BpfInsn,
}

/// BPF receive + NDRV send on `iface`.
pub struct MacLink {
    bpf: std::fs::File,
    ndrv: std::fs::File,
    pending: VecDeque<Vec<u8>>,
    ethertype: u16,
    scratch: Vec<u8>,
}

impl MacLink {
    pub fn open(iface: &str, ethertype: u16) -> Result<Self> {
        let bpf = open_bpf()?;
        let ndrv = open_ndrv(iface)?;
        configure_bpf(bpf.as_raw_fd(), iface, ethertype)?;
        set_nonblocking(bpf.as_raw_fd())?;
        set_nonblocking(ndrv.as_raw_fd())?;
        Ok(Self {
            bpf,
            ndrv,
            pending: VecDeque::new(),
            ethertype,
            scratch: vec![0; BPF_BUFFER],
        })
    }

    pub fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.ndrv.write_all(frame).map_err(map_io)
    }

    pub fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        match self.bpf.read(&mut self.scratch) {
            Ok(0) => Ok(None),
            Ok(n) => {
                take_bpf_frames(&self.scratch[..n], self.ethertype, &mut self.pending);
                Ok(self.pending.pop_front())
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(None),
            // tun-rs TAP: a BPF wakeup with no frames is UnexpectedEof.
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(err) => Err(map_io(err)),
        }
    }
}

/// True when this process can open a BPF device.
pub fn bpf_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| open_bpf().is_ok())
}

fn open_bpf() -> Result<std::fs::File> {
    for i in 0..256 {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(format!("/dev/bpf{i}"))
        {
            Ok(file) => return Ok(file),
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                return Err(LinkError::Permission);
            }
            Err(_) => {}
        }
    }
    Err(LinkError::Pcap("no free /dev/bpf device".into()))
}

fn open_ndrv(iface: &str) -> Result<std::fs::File> {
    let fd = unsafe { libc::socket(libc::AF_NDRV, libc::SOCK_RAW, 0) };
    if fd < 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    set_cloexec(file.as_raw_fd())?;
    let mut addr = ndrv_addr(iface)?;
    let addr_ptr = std::ptr::addr_of_mut!(addr) as *mut libc::sockaddr;
    let addr_len = mem::size_of::<libc::sockaddr_ndrv>() as u32;
    if unsafe { libc::bind(file.as_raw_fd(), addr_ptr, addr_len) } != 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    if unsafe { libc::connect(file.as_raw_fd(), addr_ptr, addr_len) } != 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    Ok(file)
}

fn ndrv_addr(iface: &str) -> Result<libc::sockaddr_ndrv> {
    let mut addr: libc::sockaddr_ndrv = unsafe { mem::zeroed() };
    addr.snd_len = mem::size_of::<libc::sockaddr_ndrv>() as u8;
    addr.snd_family = libc::AF_NDRV as u8;
    let bytes = iface.as_bytes();
    if bytes.len() >= addr.snd_name.len() {
        return Err(LinkError::NoDevice(iface.into()));
    }
    addr.snd_name[..bytes.len()].copy_from_slice(bytes);
    Ok(addr)
}

fn configure_bpf(fd: RawFd, iface: &str, ethertype: u16) -> Result<()> {
    let mut blen = BPF_BUFFER as libc::c_uint;
    ioctl(fd, libc::BIOCSBLEN, std::ptr::addr_of_mut!(blen))?;
    let mut enable = 1i32;
    let mut disable = 0i32;
    ioctl(fd, libc::BIOCIMMEDIATE, std::ptr::addr_of_mut!(enable))?;
    ioctl(fd, BIOCSSEESENT, std::ptr::addr_of_mut!(disable))?;
    ioctl(fd, libc::BIOCSHDRCMPLT, std::ptr::addr_of_mut!(enable))?;
    ioctl(
        fd,
        libc::BIOCPROMISC as libc::c_ulong,
        std::ptr::addr_of_mut!(enable),
    )?;
    let mut ifr = ifreq_name(iface)?;
    ioctl(fd, libc::BIOCSETIF, std::ptr::addr_of_mut!(ifr))?;
    attach_ethertype(fd, ethertype)
}

fn attach_ethertype(fd: RawFd, ethertype: u16) -> Result<()> {
    let mut insns = [
        BpfInsn {
            code: BPF_LD | BPF_H | BPF_ABS,
            jt: 0,
            jf: 0,
            k: 12,
        },
        BpfInsn {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: u32::from(ethertype),
        },
        BpfInsn {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: u32::MAX,
        },
        BpfInsn {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: 0,
        },
    ];
    let mut prog = BpfProgram {
        bf_len: insns.len() as u32,
        bf_insns: insns.as_mut_ptr(),
    };
    ioctl(fd, libc::BIOCSETF, std::ptr::addr_of_mut!(prog))
}

fn ifreq_name(iface: &str) -> Result<libc::ifreq> {
    let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
    let bytes = iface.as_bytes();
    if bytes.len() >= ifr.ifr_name.len() {
        return Err(LinkError::NoDevice(iface.into()));
    }
    for (dst, src) in ifr.ifr_name.iter_mut().zip(bytes.iter()) {
        *dst = *src as libc::c_char;
    }
    Ok(ifr)
}

fn ioctl<T>(fd: RawFd, req: libc::c_ulong, arg: *mut T) -> Result<()> {
    let rc = unsafe { libc::ioctl(fd, req, arg) };
    if rc == -1 {
        Err(map_io(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(map_io(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn set_cloexec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD, 0) };
    if flags < 0 {
        return Err(map_io(io::Error::last_os_error()));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        Err(map_io(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn map_io(err: io::Error) -> LinkError {
    match err.kind() {
        io::ErrorKind::PermissionDenied => LinkError::Permission,
        _ => LinkError::Pcap(err.to_string()),
    }
}

/// Split a BPF read into Ethernet frames. `bh_hdrlen` includes padding.
fn take_bpf_frames(buffer: &[u8], ethertype: u16, out: &mut VecDeque<Vec<u8>>) {
    let hdr_size = mem::size_of::<libc::bpf_hdr>();
    let mut p = 0;
    while p < buffer.len() {
        if buffer.len() - p < hdr_size {
            break;
        }
        let hdr: libc::bpf_hdr = unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(p).cast()) };
        let hdrlen = hdr.bh_hdrlen as usize;
        let caplen = hdr.bh_caplen as usize;
        if hdrlen == 0 {
            break;
        }
        let end = p.saturating_add(hdrlen).saturating_add(caplen);
        if caplen > 0 && end <= buffer.len() {
            let frame = &buffer[p + hdrlen..end];
            if frame_ethertype(frame) == Some(ethertype) {
                out.push_back(frame.to_vec());
            }
        }
        let Some(step) = next_bpf_step(hdrlen, caplen) else {
            break;
        };
        p += step;
    }
}

fn frame_ethertype(frame: &[u8]) -> Option<u16> {
    (frame.len() >= 14).then(|| u16::from_be_bytes([frame[12], frame[13]]))
}

fn next_bpf_step(bh_hdrlen: usize, bh_caplen: usize) -> Option<usize> {
    let step = (bh_hdrlen + bh_caplen + 3) & !3;
    (step > 0).then_some(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpf_padding_is_four_byte_aligned() {
        assert_eq!(next_bpf_step(18, 60), Some(80));
        assert_eq!(next_bpf_step(18, 61), Some(80));
        assert_eq!(next_bpf_step(0, 0), None);
    }
}
