//! QUIC variable-length integers (RFC 9000, section 16).

use crate::Error;

/// Append the shortest encoding of `value`. Values of 2^62 or more cannot be encoded.
pub fn encode(value: u64, out: &mut Vec<u8>) -> Result<(), Error> {
    if value < 1 << 6 {
        out.push(value as u8);
    } else if value < 1 << 14 {
        out.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes());
    } else if value < 1 << 30 {
        out.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes());
    } else if value < 1 << 62 {
        out.extend_from_slice(&(value | 0xc000_0000_0000_0000).to_be_bytes());
    } else {
        return Err(Error::VarintOverflow);
    }
    Ok(())
}

/// Read one varint. A short buffer is [`Error::NeedMore`]. A longer-than-needed
/// encoding is [`Error::NonMinimalVarint`].
pub fn decode(input: &[u8]) -> Result<(u64, usize), Error> {
    if input.is_empty() {
        return Err(Error::NeedMore);
    }
    let len = 1usize << (input[0] >> 6);
    if input.len() < len {
        return Err(Error::NeedMore);
    }
    let mut val = (input[0] & 0x3f) as u64;
    for &byte in &input[1..len] {
        val = (val << 8) | u64::from(byte);
    }
    let min = match len {
        1 => 0,
        2 => 1u64 << 6,
        4 => 1u64 << 14,
        8 => 1u64 << 30,
        _ => unreachable!("varint length is 1, 2, 4, or 8"),
    };
    if val < min {
        return Err(Error::NonMinimalVarint);
    }
    Ok((val, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: u64) {
        let mut buf = Vec::new();
        encode(value, &mut buf).unwrap();
        let (got, n) = decode(&buf).unwrap();
        assert_eq!(got, value);
        assert_eq!(n, buf.len());
    }

    #[test]
    fn boundaries() {
        for value in [
            0,
            63,
            64,
            16383,
            16384,
            (1 << 30) - 1,
            1 << 30,
            (1 << 62) - 1,
        ] {
            roundtrip(value);
        }
        let mut buf = Vec::new();
        assert_eq!(encode(1 << 62, &mut buf), Err(Error::VarintOverflow));
    }

    #[test]
    fn shortest_widths() {
        let mut buf = Vec::new();
        encode(63, &mut buf).unwrap();
        assert_eq!(buf, [63]);
        buf.clear();
        encode(64, &mut buf).unwrap();
        assert_eq!(buf.len(), 2);
        buf.clear();
        encode(16384, &mut buf).unwrap();
        assert_eq!(buf.len(), 4);
        buf.clear();
        encode(1 << 30, &mut buf).unwrap();
        assert_eq!(buf.len(), 8);
    }

    #[test]
    fn rejects_non_minimal_and_short_buffers() {
        assert_eq!(decode(&[0x40, 0x3f]), Err(Error::NonMinimalVarint));
        assert_eq!(decode(&[0x40]), Err(Error::NeedMore));
        assert_eq!(decode(&[]), Err(Error::NeedMore));
    }
}
