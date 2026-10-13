//! Minimal Ogg Opus demuxer and muxer (RFC 3533 and RFC 7845).
//!
//! Enough to turn an `.opus` file into packets and write those packets back
//! as a playable Ogg Opus file. Comment tags are not preserved: LOC carries
//! the identification header and the packets, and the muxer writes a new
//! vendor string.

use crate::opus::{self, OpusPacket, OpusStream};
use crate::Error;

const CONTINUED: u8 = 0x01;
const BOS: u8 = 0x02;
const EOS: u8 = 0x04;
const SERIAL: u32 = 0x6571_7569;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04c1_1db7;
            } else {
                crc <<= 1;
            }
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = crc_table();

fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in data {
        let index = ((crc >> 24) as u8) ^ byte;
        crc = (crc << 8) ^ CRC_TABLE[index as usize];
    }
    crc
}

struct Page {
    header_type: u8,
    granule: Option<u64>,
    serial: u32,
    sequence: u32,
    segments: Vec<u8>,
    data: Vec<u8>,
}

fn write_page(
    out: &mut Vec<u8>,
    header_type: u8,
    granule: u64,
    sequence: u32,
    segments: &[u8],
    data: &[u8],
) -> Result<(), Error> {
    if segments.len() > 255 {
        return Err(Error::Ogg("page has more than 255 segments".into()));
    }
    let start = out.len();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(header_type);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&SERIAL.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.push(segments.len() as u8);
    out.extend_from_slice(segments);
    out.extend_from_slice(data);
    let crc = ogg_crc(&out[start..]);
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

fn read_page(input: &[u8]) -> Result<(Page, usize), Error> {
    if input.len() < 27 {
        return Err(Error::Ogg("truncated Ogg page header".into()));
    }
    if &input[..4] != b"OggS" {
        return Err(Error::Ogg("missing OggS capture pattern".into()));
    }
    if input[4] != 0 {
        return Err(Error::Ogg("unsupported Ogg version".into()));
    }
    let segment_count = input[26] as usize;
    let header_len = 27 + segment_count;
    if input.len() < header_len {
        return Err(Error::Ogg("truncated Ogg segment table".into()));
    }
    let segments = input[27..header_len].to_vec();
    let data_len: usize = segments.iter().map(|segment| *segment as usize).sum();
    let total = header_len + data_len;
    if input.len() < total {
        return Err(Error::Ogg("truncated Ogg page body".into()));
    }
    let mut for_crc = input[..total].to_vec();
    let stored = u32::from_le_bytes(input[22..26].try_into().expect("4 bytes"));
    for_crc[22..26].fill(0);
    if ogg_crc(&for_crc) != stored {
        return Err(Error::Ogg("Ogg page checksum mismatch".into()));
    }
    let granule = u64::from_le_bytes(input[6..14].try_into().expect("8 bytes"));
    Ok((
        Page {
            header_type: input[5],
            granule: if granule == u64::MAX {
                None
            } else {
                Some(granule)
            },
            serial: u32::from_le_bytes(input[14..18].try_into().expect("4 bytes")),
            sequence: u32::from_le_bytes(input[18..22].try_into().expect("4 bytes")),
            segments,
            data: input[header_len..total].to_vec(),
        },
        total,
    ))
}

fn read_pages(input: &[u8]) -> Result<Vec<Page>, Error> {
    if input.is_empty() {
        return Err(Error::Ogg("empty Ogg file".into()));
    }
    let mut pages = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        let (page, len) = read_page(&input[offset..])?;
        offset += len;
        pages.push(page);
    }
    Ok(pages)
}

struct Completion {
    granule: u64,
    eos: bool,
    packets: Vec<Vec<u8>>,
}

fn segment_lengths(len: usize) -> Vec<usize> {
    let mut lengths = Vec::new();
    let mut left = len;
    if left == 0 {
        lengths.push(0);
        return lengths;
    }
    while left >= 255 {
        lengths.push(255);
        left -= 255;
    }
    lengths.push(left);
    lengths
}

/// Packets that completed on pages of the first logical stream.
fn assemble(pages: &[Page]) -> Result<Vec<Completion>, Error> {
    let serial = pages
        .first()
        .ok_or_else(|| Error::Ogg("no Ogg pages".into()))?
        .serial;
    let mut partial: Option<Vec<u8>> = None;
    let mut expected_sequence: Option<u32> = None;
    let mut completions = Vec::new();

    for page in pages {
        if page.serial != serial {
            continue;
        }
        if let Some(expected) = expected_sequence {
            if page.sequence != expected {
                partial = None;
            }
        }
        expected_sequence = Some(page.sequence.wrapping_add(1));

        let (segments, data) = if page.header_type & CONTINUED != 0 && partial.is_none() {
            match drop_ghost(&page.segments, &page.data) {
                Some(kept) => kept,
                None => continue,
            }
        } else {
            if page.header_type & CONTINUED == 0 {
                partial = None;
            }
            (page.segments.clone(), page.data.clone())
        };

        let mut current = partial.take().unwrap_or_default();
        let mut completed = Vec::new();
        let mut offset = 0usize;
        for segment in &segments {
            let len = *segment as usize;
            let end = offset + len;
            if end > data.len() {
                return Err(Error::Ogg("segment runs past the page body".into()));
            }
            current.extend_from_slice(&data[offset..end]);
            offset = end;
            if *segment < 255 {
                completed.push(std::mem::take(&mut current));
            }
        }
        if offset != data.len() {
            return Err(Error::Ogg(
                "page body does not match the segment table".into(),
            ));
        }
        partial = if current.is_empty() {
            None
        } else {
            Some(current)
        };

        if completed.is_empty() {
            continue;
        }
        let granule = page.granule.ok_or_else(|| {
            Error::Ogg("a packet completed on a page with no granule position".into())
        })?;
        completions.push(Completion {
            granule,
            eos: page.header_type & EOS != 0,
            packets: completed,
        });
    }
    Ok(completions)
}

/// Drop a continued packet whose beginning is missing. `None` means the whole
/// page was that unfinished packet.
fn drop_ghost(segments: &[u8], data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut offset = 0usize;
    let mut dropping = true;
    let mut kept_segments = Vec::new();
    let mut kept = Vec::new();
    for segment in segments {
        let len = *segment as usize;
        if offset + len > data.len() {
            return None;
        }
        if dropping {
            offset += len;
            if *segment < 255 {
                dropping = false;
            }
            continue;
        }
        kept.extend_from_slice(&data[offset..offset + len]);
        kept_segments.push(*segment);
        offset += len;
    }
    if dropping {
        None
    } else {
        Some((kept_segments, kept))
    }
}

fn comment_packet() -> Vec<u8> {
    let vendor = b"etherquic-audio";
    let mut packet = Vec::with_capacity(16 + vendor.len());
    packet.extend_from_slice(b"OpusTags");
    packet.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    packet.extend_from_slice(vendor);
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet
}

struct PageBuild {
    segments: Vec<u8>,
    data: Vec<u8>,
    continued: bool,
    bos: bool,
    eos: bool,
    granule: u64,
    completed: bool,
    sequence: u32,
}

impl PageBuild {
    fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    fn flush(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.is_empty() {
            return Ok(());
        }
        let mut header_type = 0u8;
        if self.continued {
            header_type |= CONTINUED;
        }
        if self.bos {
            header_type |= BOS;
        }
        let wrote_eos = self.eos && self.completed;
        if wrote_eos {
            header_type |= EOS;
        }
        let granule = if self.completed {
            self.granule
        } else {
            u64::MAX
        };
        write_page(
            out,
            header_type,
            granule,
            self.sequence,
            &self.segments,
            &self.data,
        )?;
        self.sequence = self.sequence.wrapping_add(1);
        self.segments.clear();
        self.data.clear();
        self.continued = false;
        self.bos = false;
        self.completed = false;
        if wrote_eos {
            self.eos = false;
        }
        Ok(())
    }

    fn add_packet(
        &mut self,
        out: &mut Vec<u8>,
        packet: &[u8],
        granule: u64,
        eos: bool,
        max_segments: usize,
        max_payload: usize,
    ) -> Result<(), Error> {
        let lengths = segment_lengths(packet.len());
        let segment_count = lengths.len();
        let mut offset = 0usize;
        // Set once this packet has placed a segment. A page break before that
        // starts a new packet; a page break after it continues this packet.
        let mut started = false;
        for (index, len) in lengths.into_iter().enumerate() {
            let finishes = index + 1 == segment_count;
            loop {
                let full = self.segments.len() >= max_segments
                    || (!self.data.is_empty() && self.data.len() + len > max_payload);
                if !full {
                    break;
                }
                self.flush(out)?;
                self.continued = started;
            }
            self.segments.push(len as u8);
            self.data.extend_from_slice(&packet[offset..offset + len]);
            offset += len;
            started = true;
            if finishes {
                self.completed = true;
                self.granule = granule;
                if eos {
                    self.eos = true;
                }
            }
        }
        Ok(())
    }
}

fn packet_at(completions: &[Completion], index: usize) -> Option<(&[u8], u64)> {
    let mut left = index;
    for completion in completions {
        if left < completion.packets.len() {
            return Some((&completion.packets[left], completion.granule));
        }
        left -= completion.packets.len();
    }
    None
}

pub fn demux(input: &[u8]) -> Result<OpusStream, Error> {
    let pages = read_pages(input)?;
    let completions = assemble(&pages)?;
    let (id_packet, id_granule) = packet_at(&completions, 0)
        .ok_or_else(|| Error::Ogg("missing Opus identification header".into()))?;
    let (comment, comment_granule) = packet_at(&completions, 1)
        .ok_or_else(|| Error::Ogg("missing Opus comment header".into()))?;
    let id = opus::parse_id(id_packet)?;
    if comment.len() < 8 || &comment[..8] != b"OpusTags" {
        return Err(Error::Ogg(
            "second Opus packet is not the comment header".into(),
        ));
    }
    if id_granule != 0 || comment_granule != 0 {
        return Err(Error::Ogg(
            "Opus header pages must use granule position 0".into(),
        ));
    }

    let mut skip = 2usize;
    let mut audio_groups = Vec::new();
    for completion in completions {
        if skip >= completion.packets.len() {
            skip -= completion.packets.len();
            continue;
        }
        let packets = completion.packets[skip..].to_vec();
        skip = 0;
        audio_groups.push(Completion {
            granule: completion.granule,
            eos: completion.eos,
            packets,
        });
    }

    let (packets, end_trim) = assign_timestamps(&audio_groups, id.pre_skip)?;
    Ok(OpusStream {
        id,
        packets,
        end_trim,
    })
}

fn assign_timestamps(
    groups: &[Completion],
    pre_skip: u16,
) -> Result<(Vec<OpusPacket>, u64), Error> {
    let mut previous_end = None;
    let mut packets = Vec::new();
    let mut end_trim = 0u64;

    for (index, group) in groups.iter().enumerate() {
        let mut durations = Vec::with_capacity(group.packets.len());
        for packet in &group.packets {
            durations.push(opus::packet_samples(packet)?);
        }
        let sum: u64 = durations.iter().map(|duration| u64::from(*duration)).sum();
        let start = if let Some(previous) = previous_end {
            previous
        } else if group.granule >= sum {
            group.granule - sum
        } else if group.eos {
            0
        } else {
            return Err(Error::Ogg(
                "first audio page granule is smaller than the samples on that page".into(),
            ));
        };
        if index == 0 && group.eos && group.granule < u64::from(pre_skip) {
            return Err(Error::Ogg(
                "end granule is smaller than the pre-skip".into(),
            ));
        }

        let mut cursor = start;
        for (packet, duration) in group.packets.iter().zip(durations) {
            packets.push(OpusPacket {
                data: packet.clone(),
                start_granule: cursor,
                samples: duration,
            });
            cursor += u64::from(duration);
        }

        if group.eos && group.granule < cursor {
            if group.granule < start {
                return Err(Error::Ogg(
                    "end granule precedes the packets on its page".into(),
                ));
            }
            end_trim = cursor - group.granule;
            previous_end = Some(group.granule);
        } else if group.granule != cursor {
            return Err(Error::Ogg(format!(
                "granule {} does not match decoded sample count {}",
                group.granule, cursor
            )));
        } else {
            previous_end = Some(cursor);
        }
    }
    Ok((packets, end_trim))
}

pub fn mux(stream: &OpusStream) -> Result<Vec<u8>, Error> {
    mux_limited(stream, 255, 4096)
}

pub fn mux_limited(
    stream: &OpusStream,
    max_segments: usize,
    max_payload: usize,
) -> Result<Vec<u8>, Error> {
    if max_segments == 0 || max_segments > 255 || max_payload == 0 {
        return Err(Error::Ogg("invalid Ogg page limit".into()));
    }
    if stream.packets.is_empty() {
        return Err(Error::Opus("no audio packets to mux".into()));
    }
    let last_samples = u64::from(stream.packets.last().expect("non-empty").samples);
    if stream.end_trim > last_samples {
        return Err(Error::Ogg("end trim is larger than the last packet".into()));
    }

    let mut out = Vec::new();
    let mut page = PageBuild {
        segments: Vec::new(),
        data: Vec::new(),
        continued: false,
        bos: true,
        eos: false,
        granule: 0,
        completed: false,
        sequence: 0,
    };
    page.add_packet(
        &mut out,
        &stream.id.raw,
        0,
        false,
        max_segments,
        max_payload,
    )?;
    page.flush(&mut out)?;
    page.add_packet(
        &mut out,
        &comment_packet(),
        0,
        false,
        max_segments,
        max_payload,
    )?;
    page.flush(&mut out)?;

    let last = stream.packets.len() - 1;
    for (index, packet) in stream.packets.iter().enumerate() {
        if index == last && stream.end_trim > 0 && !page.is_empty() {
            page.flush(&mut out)?;
        }
        let mut granule = packet.start_granule + u64::from(packet.samples);
        if index == last {
            granule -= stream.end_trim;
        }
        page.add_packet(
            &mut out,
            &packet.data,
            granule,
            index == last,
            max_segments,
            max_payload,
        )?;
    }
    page.flush(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus::{id_header, parse_id, OpusId};

    fn stream(packets: Vec<OpusPacket>, end_trim: u64) -> OpusStream {
        OpusStream {
            id: parse_id(&id_header(2, 312, 48_000)).unwrap(),
            packets,
            end_trim,
        }
    }

    fn packet(byte: u8, start: u64) -> OpusPacket {
        OpusPacket {
            data: vec![byte],
            start_granule: start,
            samples: 960,
        }
    }

    #[test]
    fn crc_table_matches_the_ogg_polynomial() {
        assert_eq!(CRC_TABLE[1], 0x04c1_1db7);
    }

    #[test]
    fn roundtrip_packets_and_granules() {
        let original = stream(vec![packet(0x98, 0), packet(0x98, 960)], 0);
        let decoded = demux(&mux(&original).unwrap()).unwrap();
        assert_eq!(decoded.packets, original.packets);
        assert_eq!(decoded.id.raw, original.id.raw);
        assert_eq!(decoded.end_trim, 0);
    }

    #[test]
    fn page_break_between_packets_is_not_a_continuation() {
        let original = stream(vec![packet(0x98, 0), packet(0x98, 960)], 0);
        let decoded = demux(&mux_limited(&original, 1, 4096).unwrap()).unwrap();
        assert_eq!(decoded.packets, original.packets);
    }

    #[test]
    fn packet_that_spans_pages_roundtrips() {
        let mut data = vec![0x98];
        data.resize(300, 0xab);
        let original = stream(
            vec![OpusPacket {
                data,
                start_granule: 1_000,
                samples: 960,
            }],
            0,
        );
        let decoded = demux(&mux_limited(&original, 1, 4096).unwrap()).unwrap();
        assert_eq!(decoded.packets, original.packets);
    }

    #[test]
    fn end_trim_roundtrips_when_a_previous_page_exists() {
        let original = stream(vec![packet(0x98, 0), packet(0x98, 960)], 100);
        let decoded = demux(&mux(&original).unwrap()).unwrap();
        assert_eq!(decoded.packets, original.packets);
        assert_eq!(decoded.end_trim, 100);
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let bytes = mux(&stream(vec![packet(0x98, 0)], 0)).unwrap();
        let mut broken = bytes.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(demux(&broken).unwrap_err().to_string().contains("checksum"));
    }

    #[test]
    fn rejects_a_non_opus_ogg() {
        let mut out = Vec::new();
        write_page(&mut out, BOS, 0, 0, &[4], b"test").unwrap();
        write_page(&mut out, 0, 0, 1, &[4], b"test").unwrap();
        assert!(demux(&out).is_err());
    }

    #[test]
    fn id_header_fields() {
        let id: OpusId = parse_id(&id_header(1, 312, 48_000)).unwrap();
        assert_eq!(id.channels, 1);
        assert_eq!(id.pre_skip, 312);
        assert_eq!(id.input_sample_rate, 48_000);
    }
}
