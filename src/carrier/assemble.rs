//! Shared slice header for QUIC datagrams, UDP/RTP, and AVB.

use std::collections::HashMap;
use std::time::Instant;

pub const HEADER: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Lane {
    Picture = 1,
    Audio = 2,
    Input = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Deadline,
    Retry,
    Latest,
}

pub fn class_for(lane: Lane) -> Class {
    match lane {
        Lane::Picture | Lane::Audio => Class::Deadline,
        Lane::Input => Class::Retry,
    }
}

/// Picture and audio are deadline. Keys are retried in order. Pointer and
/// pad datagrams are latest-only.
pub fn record_class(kind: &str) -> Class {
    match kind {
        "key" => Class::Retry,
        "pointer" | "pad" => Class::Latest,
        _ => Class::Deadline,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slice {
    pub record_id: u16,
    pub index: u16,
    pub count: u16,
    pub bytes: Vec<u8>,
}

impl Slice {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + self.bytes.len());
        out.extend(self.record_id.to_le_bytes());
        out.extend(self.index.to_le_bytes());
        out.extend(self.count.to_le_bytes());
        out.extend_from_slice(&self.bytes);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER {
            return None;
        }
        let record_id = u16::from_le_bytes([bytes[0], bytes[1]]);
        let index = u16::from_le_bytes([bytes[2], bytes[3]]);
        let count = u16::from_le_bytes([bytes[4], bytes[5]]);
        if count == 0 || index >= count {
            return None;
        }
        Some(Self {
            record_id,
            index,
            count,
            bytes: bytes[HEADER..].to_vec(),
        })
    }
}

pub fn split_record(record_id: u16, bytes: &[u8], budget: usize) -> Vec<Slice> {
    let budget = budget.max(1);
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![b""]
    } else {
        bytes.chunks(budget).collect()
    };
    let count = chunks.len() as u16;
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| Slice {
            record_id,
            index: index as u16,
            count,
            bytes: chunk.to_vec(),
        })
        .collect()
}

#[derive(Clone, Debug)]
struct Open {
    parts: Vec<Option<Vec<u8>>>,
    deadline: Instant,
}

/// Delivers a record when every slice index is present. A hole is dropped
/// when its deadline passes or a newer `record_id` arrives.
#[derive(Debug, Default)]
pub struct Assembler {
    open: HashMap<u16, Open>,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every open record. A later config starts from an empty assembler.
    pub fn reset(&mut self) {
        self.open.clear();
    }

    pub fn push(&mut self, slice: Slice, now: Instant, deadline: Instant) -> Option<Vec<u8>> {
        self.drop_expired(now);
        let newer: Vec<u16> = self
            .open
            .keys()
            .copied()
            .filter(|id| is_newer(slice.record_id, *id))
            .collect();
        for id in newer {
            self.open.remove(&id);
        }
        if self
            .open
            .keys()
            .any(|id| *id != slice.record_id && is_newer(*id, slice.record_id))
        {
            return None;
        }
        let slot = self.open.entry(slice.record_id).or_insert_with(|| Open {
            parts: vec![None; slice.count as usize],
            deadline,
        });
        // The deadline is how long a quiet hole may sit. A slice that arrives
        // inside the window means the record is still moving, so the interval
        // starts again. A burst that takes longer than one frame to show up
        // is not dropped between its own packets.
        if deadline > slot.deadline {
            slot.deadline = deadline;
        }
        if slice.count as usize != slot.parts.len() {
            return None;
        }
        slot.parts[slice.index as usize] = Some(slice.bytes);
        if slot.parts.iter().all(|part| part.is_some()) {
            let parts = self.open.remove(&slice.record_id).unwrap().parts;
            let mut out = Vec::new();
            for part in parts.into_iter().flatten() {
                out.extend(part);
            }
            return Some(out);
        }
        None
    }

    fn drop_expired(&mut self, now: Instant) {
        self.open.retain(|_, open| open.deadline > now);
    }
}

/// `incoming` is newer when it is inside the forward half of the `u16` space,
/// or when it jumps forward by more than half the space. That jump starts a
/// new epoch and drops a holed record. A small step backward stays older.
/// Shared with the datagram demux. `pulse` makes both assemblers start empty.
#[derive(Clone, Debug)]
pub struct AssemblerReset {
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl AssemblerReset {
    pub fn new() -> Self {
        Self {
            generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn pulse(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The first config leaves a partial record in place. The next one drops it.
pub fn pulse_on_later_config(configs: &mut u32, reset: Option<&AssemblerReset>) {
    *configs += 1;
    if *configs > 1 {
        if let Some(reset) = reset {
            reset.pulse();
        }
    }
}

impl Default for AssemblerReset {
    fn default() -> Self {
        Self::new()
    }
}

/// Replace `asm` when `generation` has moved. Returns the generation now in force.
pub fn apply_reset(asm: &mut Assembler, generation: u64, seen: u64) -> u64 {
    if generation != seen {
        asm.reset();
        generation
    } else {
        seen
    }
}

fn is_newer(incoming: u16, current: u16) -> bool {
    let forward = incoming.wrapping_sub(current);
    if forward > 0 && forward < 32_768 {
        return true;
    }
    incoming > current && incoming.wrapping_sub(current) > 32_768
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn reliability_classes_match_the_lane() {
        assert_eq!(record_class("picture"), Class::Deadline);
        assert_eq!(record_class("audio"), Class::Deadline);
        assert_eq!(record_class("key"), Class::Retry);
        assert_eq!(record_class("pointer"), Class::Latest);
        assert_eq!(record_class("pad"), Class::Latest);
        assert_eq!(class_for(Lane::Picture), Class::Deadline);
        assert_eq!(class_for(Lane::Input), Class::Retry);
    }

    #[test]
    fn a_later_config_drops_a_partial_record() {
        let mut pictures = Assembler::new();
        let mut audios = Assembler::new();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let partial = split_record(1, b"partial-picture", 4);
        assert!(pictures.push(partial[0].clone(), now, deadline).is_none());
        let reset = AssemblerReset::new();
        let mut seen = reset.get();
        reset.pulse();
        seen = apply_reset(&mut pictures, reset.get(), seen);
        seen = apply_reset(&mut audios, reset.get(), seen);
        assert_eq!(seen, 1);
        let fresh = split_record(2, b"new", 8);
        assert_eq!(
            pictures.push(fresh[0].clone(), now, deadline).as_deref(),
            Some(b"new".as_slice())
        );
        assert!(pictures.push(partial[1].clone(), now, deadline).is_none());
    }

    #[test]
    fn the_first_config_does_not_reset_and_the_next_one_does() {
        let reset = AssemblerReset::new();
        let mut configs = 0;
        pulse_on_later_config(&mut configs, Some(&reset));
        assert_eq!(reset.get(), 0);
        pulse_on_later_config(&mut configs, Some(&reset));
        assert_eq!(reset.get(), 1);
        pulse_on_later_config(&mut configs, None);
        assert_eq!(reset.get(), 1);
    }

    #[test]
    fn a_holed_record_does_not_block_the_next() {
        let mut asm = Assembler::new();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let first = split_record(1, b"abcdefghij", 4);
        assert!(asm.push(first[1].clone(), now, deadline).is_none());
        let next = split_record(2, b"next", 4);
        assert_eq!(
            asm.push(next[0].clone(), now, deadline).as_deref(),
            Some(b"next".as_slice())
        );
        assert!(asm.push(first[0].clone(), now, deadline).is_none());
    }

    #[test]
    fn dropping_the_last_slice_yields_nothing_until_the_next_record() {
        let mut asm = Assembler::new();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let slices = split_record(7, &[9u8; 20], 8);
        assert!(slices.len() > 1);
        for slice in &slices[..slices.len() - 1] {
            assert!(asm.push(slice.clone(), now, deadline).is_none());
        }
        let next = split_record(8, b"after", 8);
        assert_eq!(
            asm.push(next[0].clone(), now, deadline).as_deref(),
            Some(b"after".as_slice())
        );
    }

    #[test]
    fn an_older_id_does_not_replace_a_hole() {
        let mut asm = Assembler::new();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let hole = split_record(5, b"abcdefghij", 4);
        assert!(hole.len() > 1);
        assert!(asm.push(hole[0].clone(), now, deadline).is_none());
        let older = split_record(4, b"old!", 8);
        assert!(asm.push(older[0].clone(), now, deadline).is_none());
        let mut done = None;
        for slice in hole.iter().skip(1) {
            done = asm.push(slice.clone(), now, deadline).or(done);
        }
        assert_eq!(done.as_deref(), Some(b"abcdefghij".as_slice()));
    }

    #[test]
    fn a_jump_past_half_the_space_starts_a_new_epoch() {
        let mut asm = Assembler::new();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let hole = split_record(1, b"abcdefghij", 4);
        assert!(asm.push(hole[0].clone(), now, deadline).is_none());
        let epoch = split_record(40_000, b"epoch", 8);
        assert_eq!(epoch.len(), 1);
        assert_eq!(
            asm.push(epoch[0].clone(), now, deadline).as_deref(),
            Some(b"epoch".as_slice())
        );
    }

    #[test]
    fn a_deadline_drops_the_hole() {
        let mut asm = Assembler::new();
        let now = Instant::now();
        let slices = split_record(1, b"abcdef", 3);
        assert!(asm
            .push(slices[0].clone(), now, now + Duration::from_millis(1))
            .is_none());
        let later = now + Duration::from_secs(2);
        let rest = split_record(1, b"abcdef", 3);
        assert!(asm
            .push(rest[1].clone(), later, later + Duration::from_secs(1))
            .is_none());
    }

    #[test]
    fn a_slice_inside_the_window_keeps_the_hole_open() {
        let mut asm = Assembler::new();
        let now = Instant::now();
        let slices = split_record(1, b"abcdefghi", 3);
        assert_eq!(slices.len(), 3);
        assert!(asm
            .push(slices[0].clone(), now, now + Duration::from_millis(10))
            .is_none());
        let mid = now + Duration::from_millis(8);
        assert!(asm
            .push(slices[1].clone(), mid, mid + Duration::from_millis(10))
            .is_none());
        // 16 ms is past the first slice's deadline and inside the refreshed one.
        let later = now + Duration::from_millis(16);
        assert_eq!(
            asm.push(slices[2].clone(), later, later + Duration::from_millis(10))
                .as_deref(),
            Some(b"abcdefghi".as_slice())
        );
    }
}
