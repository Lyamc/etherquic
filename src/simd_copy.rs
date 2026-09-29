//! Datagram body copies through nightly `std::simd`.
//!
//! Each QUIC packet is copied when it is framed and again when it is handed
//! back to Quinn. The header is 20 bytes; the body is the part wide enough
//! for SIMD. Chunks of 64 bytes use `Simd<u8, 64>`. The tail is a normal copy.

use std::simd::prelude::Simd;

const LANES: usize = 64;

/// Copy `src` into `dst`. The slices must be the same length.
pub(crate) fn copy_bytes(dst: &mut [u8], src: &[u8]) {
    assert_eq!(
        dst.len(),
        src.len(),
        "simd copy length mismatch: dst {} src {}",
        dst.len(),
        src.len()
    );
    let mut off = 0;
    while off + LANES <= src.len() {
        let chunk = Simd::<u8, LANES>::from_slice(&src[off..off + LANES]);
        chunk.copy_to_slice(&mut dst[off..off + LANES]);
        off += LANES;
    }
    if off < src.len() {
        dst[off..].copy_from_slice(&src[off..]);
    }
}

#[cfg(test)]
mod tests {
    use super::copy_bytes;

    #[test]
    fn copies_every_length_around_the_lane_size() {
        for len in [0, 1, 63, 64, 65, 127, 128, 1494, 4096] {
            let src: Vec<u8> = (0..len).map(|i| (i * 17 + 3) as u8).collect();
            let mut dst = vec![0xA5; len];
            copy_bytes(&mut dst, &src);
            assert_eq!(dst, src, "len {len}");
        }
    }
}
