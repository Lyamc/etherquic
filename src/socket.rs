//! Quinn [`AsyncUdpSocket`] backed by the in-memory Ethernet fabric.

use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpSender};

use crate::addr::{peer_to_socket_addr, socket_addr_to_peer};
use crate::fabric::{arm_waker, pop_frame, Fabric};
use crate::frame::Frame;
use crate::Mac;

/// Linux `EMSGSIZE` — Quinn treats this as a failed MTU probe, not a hard I/O error.
const EMSGSIZE: i32 = 90;

pub struct EtherSocket {
    fabric: Arc<Fabric>,
    mac: Mac,
    service_id: u16,
    local_addr: SocketAddr,
    inbox: Arc<Mutex<crate::fabric::Inbox>>,
}

// Inbox is pub(crate) in fabric — need to make the type accessible. I'll use the attach return type.
// Actually Inbox is private. The attach returns Arc<Mutex<Inbox>> but Inbox isn't pub.
// Fix fabric to export a type alias or make Inbox pub(crate) and use it here.

impl fmt::Debug for EtherSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EtherSocket")
            .field("mac", &self.mac)
            .field("service_id", &self.service_id)
            .field("local_addr", &self.local_addr)
            .field("max_datagram", &self.fabric.max_quic_datagram())
            .finish()
    }
}

impl EtherSocket {
    pub fn open(fabric: Arc<Fabric>, mac: Mac, service_id: u16) -> Box<Self> {
        let inbox = fabric.attach(mac);
        Box::new(Self {
            fabric,
            mac,
            service_id,
            local_addr: peer_to_socket_addr(mac, service_id),
            inbox,
        })
    }

    pub fn mac(&self) -> Mac {
        self.mac
    }

    pub fn service_id(&self) -> u16 {
        self.service_id
    }

    pub fn max_datagram(&self) -> usize {
        self.fabric.max_quic_datagram()
    }
}

impl Drop for EtherSocket {
    fn drop(&mut self) {
        self.fabric.detach(self.mac);
    }
}

impl AsyncUdpSocket for EtherSocket {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        Box::pin(EtherSender {
            fabric: self.fabric.clone(),
            mac: self.mac,
            service_id: self.service_id,
            max_datagram: self.fabric.max_quic_datagram(),
            ether_type: self.fabric.ether_type(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        if bufs.is_empty() || meta.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let mut count = 0usize;
        while count < bufs.len() && count < meta.len() {
            let Some(frame) = pop_frame(&self.inbox) else {
                break;
            };
            // Destination service id is the UDP-port analogue for this listener.
            if frame.dst_service != self.service_id {
                continue;
            }
            let src_addr = peer_to_socket_addr(frame.src, frame.src_service);
            let len = frame.payload.len().min(bufs[count].len());
            bufs[count][..len].copy_from_slice(&frame.payload[..len]);
            meta[count] = recv_meta(src_addr, len, self.local_addr.ip());
            count += 1;
        }

        if count > 0 {
            return Poll::Ready(Ok(count));
        }

        arm_waker(&self.inbox, cx.waker());
        // Recheck after arming to avoid a lost wakeup.
        if let Some(frame) = pop_frame(&self.inbox) {
            if frame.dst_service == self.service_id {
                let src_addr = peer_to_socket_addr(frame.src, frame.src_service);
                let len = frame.payload.len().min(bufs[0].len());
                bufs[0][..len].copy_from_slice(&frame.payload[..len]);
                meta[0] = recv_meta(src_addr, len, self.local_addr.ip());
                return Poll::Ready(Ok(1));
            }
        }
        Poll::Pending
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn max_receive_segments(&self) -> usize {
        1
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

fn recv_meta(addr: SocketAddr, len: usize, dst_ip: IpAddr) -> RecvMeta {
    let mut meta = RecvMeta::default();
    meta.addr = addr;
    meta.len = len;
    meta.stride = len;
    meta.ecn = None;
    meta.dst_ip = Some(dst_ip);
    meta
}

struct EtherSender {
    fabric: Arc<Fabric>,
    mac: Mac,
    service_id: u16,
    max_datagram: usize,
    ether_type: u16,
}

impl fmt::Debug for EtherSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EtherSender")
            .field("mac", &self.mac)
            .field("max_datagram", &self.max_datagram)
            .finish()
    }
}

impl UdpSender for EtherSender {
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        _cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(self.try_send_inner(transmit))
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }
}

impl EtherSender {
    fn try_send_inner(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        let (dst_mac, dst_service) = socket_addr_to_peer(transmit.destination).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination is not an etherquic peer address",
            )
        })?;

        // GSO is unsupported; reject multi-segment transmits.
        if let Some(seg) = transmit.segment_size {
            if seg != transmit.contents.len() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "GSO is not supported on the Ethernet fabric",
                ));
            }
        }

        if transmit.contents.len() > self.max_datagram {
            self.fabric
                .stats
                .dropped_too_big
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(io::Error::from_raw_os_error(EMSGSIZE));
        }

        let frame = Frame {
            dst: dst_mac,
            src: self.mac,
            ether_type: self.ether_type,
            dst_service: dst_service,
            src_service: self.service_id,
            payload: transmit.contents.to_vec(),
        };
        self.fabric.inject(frame);
        Ok(())
    }
}
