//! Stored (uncompressed) ZIP writer. Android requires `AndroidManifest.xml`
//! and `classes.dex` to be stored, and their payloads 4-byte aligned.

use crate::hash::crc32;

const DOS_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;

pub fn store_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut local = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let name_bytes = name.as_bytes();
        assert!(name_bytes.len() <= u16::MAX as usize, "zip name too long");
        let data_at = local.len() + 30 + name_bytes.len();
        let extra_len = (4 - (data_at % 4)) % 4;
        let offset = local.len() as u32;
        let checksum = crc32(data);
        write_local(&mut local, name_bytes, data, checksum, extra_len);
        write_central(&mut central, name_bytes, data, checksum, extra_len, offset);
        debug_assert_eq!((offset as usize + 30 + name_bytes.len() + extra_len) % 4, 0);
    }
    let central_at = local.len() as u32;
    let central_len = central.len() as u32;
    local.extend_from_slice(&central);
    write_eocd(&mut local, files.len() as u16, central_len, central_at);
    local
}

fn write_local(out: &mut Vec<u8>, name: &[u8], data: &[u8], checksum: u32, extra_len: usize) {
    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&DOS_DATE.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&(extra_len as u16).to_le_bytes());
    out.extend_from_slice(name);
    out.extend(std::iter::repeat(0u8).take(extra_len));
    out.extend_from_slice(data);
}

fn write_central(
    out: &mut Vec<u8>,
    name: &[u8],
    data: &[u8],
    checksum: u32,
    extra_len: usize,
    offset: u32,
) {
    out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&DOS_DATE.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&(extra_len as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(name);
    out.extend(std::iter::repeat(0u8).take(extra_len));
}

fn write_eocd(out: &mut Vec<u8>, count: u16, central_len: u32, central_at: u32) {
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_len.to_le_bytes());
    out.extend_from_slice(&central_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::store_zip;

    #[test]
    fn stored_payloads_are_four_byte_aligned() {
        let zip = store_zip(&[
            ("AndroidManifest.xml", b"abcd"),
            ("classes.dex", b"dex-bytes"),
        ]);
        assert_eq!(&zip[..4], b"PK\x03\x04");
        let mut offset = 0usize;
        for expected in [b"abcd".as_slice(), b"dex-bytes".as_slice()] {
            assert_eq!(&zip[offset..offset + 4], b"PK\x03\x04");
            let name_len = u16::from_le_bytes([zip[offset + 26], zip[offset + 27]]) as usize;
            let extra_len = u16::from_le_bytes([zip[offset + 28], zip[offset + 29]]) as usize;
            let data_at = offset + 30 + name_len + extra_len;
            assert_eq!(data_at % 4, 0, "payload alignment");
            assert_eq!(&zip[data_at..data_at + expected.len()], expected);
            offset = data_at + expected.len();
        }
    }
}
