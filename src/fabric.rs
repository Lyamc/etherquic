//! In-memory Ethernet segment: unicast by destination MAC, pure Rust, no sockets.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crate::frame::{Frame, DEFAULT_ETHER_TYPE};
use crate::Mac;

const BROADCAST: Mac = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

/// Why a frame is not delivered. Computed without touching the fabric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    EtherType,
    TooBig,
}

/// Who should receive a frame. Pure; the fabric applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Unicast(Mac),
    Broadcast { except_src: Mac },
}

/// Decide delivery from frame contents and path limits. No I/O, no locks.
pub fn plan_delivery(
    expected_ether_type: u16,
    max_payload: usize,
    frame: &Frame,
) -> Result<Delivery, DropReason> {
    if frame.ether_type != expected_ether_type {
        return Err(DropReason::EtherType);
    }
    if frame.payload.len() > max_payload {
        return Err(DropReason::TooBig);
    }
    if frame.dst == BROADCAST {
        Ok(Delivery::Broadcast {
            except_src: frame.src,
        })
    } else {
        Ok(Delivery::Unicast(frame.dst))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Inbox {
    queue: VecDequeFrame,
    waker: Option<Waker>,
}

// Keep the queue type local so Debug stays trivial.
#[derive(Debug, Default)]
struct VecDequeFrame(std::collections::VecDeque<Frame>);

/// Shared L2 fabric. Attach one [`crate::socket::EtherSocket`] per MAC.
#[derive(Debug)]
pub struct Fabric {
    inner: Mutex<HashMap<Mac, Arc<Mutex<Inbox>>>>,
    ether_type: u16,
    interface_mtu: usize,
    pub stats: FabricStats,
}

#[derive(Debug, Default)]
pub struct FabricStats {
    pub sent: AtomicU64,
    pub delivered: AtomicU64,
    pub dropped_no_dst: AtomicU64,
    pub dropped_too_big: AtomicU64,
    pub dropped_filter: AtomicU64,
}

impl Fabric {
    pub fn new(interface_mtu: usize) -> Arc<Self> {
        Self::with_ether_type(interface_mtu, DEFAULT_ETHER_TYPE)
    }

    pub fn with_ether_type(interface_mtu: usize, ether_type: u16) -> Arc<Self> {
        assert!(
            interface_mtu >= 1200 + crate::frame::SERVICE_HEADER_LEN,
            "interface MTU must fit a 1200-byte QUIC Initial plus service_id"
        );
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            ether_type,
            interface_mtu,
            stats: FabricStats::default(),
        })
    }

    pub fn ether_type(&self) -> u16 {
        self.ether_type
    }

    pub fn interface_mtu(&self) -> usize {
        self.interface_mtu
    }

    pub fn max_quic_datagram(&self) -> usize {
        crate::frame::max_quic_datagram(self.interface_mtu)
    }

    pub(crate) fn attach(self: &Arc<Self>, mac: Mac) -> Arc<Mutex<Inbox>> {
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let mut map = self.inner.lock().expect("fabric lock");
        map.insert(mac, inbox.clone());
        inbox
    }

    pub(crate) fn detach(&self, mac: Mac) {
        let mut map = self.inner.lock().expect("fabric lock");
        map.remove(&mac);
    }

    pub(crate) fn inject(&self, frame: Frame) {
        let plan = plan_delivery(self.ether_type, self.max_quic_datagram(), &frame);
        match plan {
            Err(DropReason::EtherType) => {
                self.stats.dropped_filter.fetch_add(1, Ordering::Relaxed);
            }
            Err(DropReason::TooBig) => {
                self.stats.dropped_too_big.fetch_add(1, Ordering::Relaxed);
            }
            Ok(delivery) => self.apply(delivery, frame),
        }
    }

    fn apply(&self, delivery: Delivery, frame: Frame) {
        self.stats.sent.fetch_add(1, Ordering::Relaxed);
        let map = self.inner.lock().expect("fabric lock");
        let targets: Vec<_> = match delivery {
            Delivery::Unicast(mac) => map.get(&mac).cloned().into_iter().collect(),
            Delivery::Broadcast { except_src } => map
                .iter()
                .filter(|(mac, _)| **mac != except_src)
                .map(|(_, inbox)| inbox.clone())
                .collect(),
        };
        if targets.is_empty() {
            self.stats.dropped_no_dst.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let n = targets.len() as u64;
        targets
            .into_iter()
            .for_each(|inbox| push(&inbox, frame.clone()));
        self.stats.delivered.fetch_add(n, Ordering::Relaxed);
    }
}

fn push(inbox: &Arc<Mutex<Inbox>>, frame: Frame) {
    let mut guard = inbox.lock().expect("inbox lock");
    guard.queue.0.push_back(frame);
    if let Some(waker) = guard.waker.take() {
        waker.wake();
    }
}

pub(crate) fn pop_frame(inbox: &Arc<Mutex<Inbox>>) -> Option<Frame> {
    let mut guard = inbox.lock().expect("inbox lock");
    guard.queue.0.pop_front()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(dst: Mac) -> Frame {
        Frame {
            dst,
            src: [2, 0, 0, 0, 0, 1],
            ether_type: DEFAULT_ETHER_TYPE,
            dst_service: 1,
            src_service: 1,
            payload: vec![1, 2, 3],
        }
    }

    #[test]
    fn plan_is_pure_unicast_or_drop() {
        let ok = frame([2, 0, 0, 0, 0, 2]);
        assert_eq!(
            plan_delivery(DEFAULT_ETHER_TYPE, 1494, &ok),
            Ok(Delivery::Unicast([2, 0, 0, 0, 0, 2]))
        );
        let mut wrong = ok.clone();
        wrong.ether_type = 0x0800;
        assert_eq!(
            plan_delivery(DEFAULT_ETHER_TYPE, 1494, &wrong),
            Err(DropReason::EtherType)
        );
        let mut big = ok.clone();
        big.payload = vec![0; 2000];
        assert_eq!(
            plan_delivery(DEFAULT_ETHER_TYPE, 1494, &big),
            Err(DropReason::TooBig)
        );
        let bcast = frame(BROADCAST);
        assert_eq!(
            plan_delivery(DEFAULT_ETHER_TYPE, 1494, &bcast),
            Ok(Delivery::Broadcast {
                except_src: bcast.src
            })
        );
    }
}

pub(crate) fn arm_waker(inbox: &Arc<Mutex<Inbox>>, waker: &std::task::Waker) {
    let mut guard = inbox.lock().expect("inbox lock");
    if guard.waker.as_ref().is_none_or(|w| !w.will_wake(waker)) {
        guard.waker = Some(waker.clone());
    }
}
