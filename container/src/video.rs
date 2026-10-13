//! Video frame marks (RFC 9626) and a helper for one encoded video frame.

use crate::{Error, Object, Property};

/// RFC 9626 video frame mark, without the RTP one-byte or two-byte header.
///
/// One data byte is `S|E|I|D|B|TID`. A second byte is the spatial or quality
/// layer id. A third byte is `TL0PICIDX`. A fourth byte is kept so a 4-byte
/// LOC value still round-trips; RFC 9626 itself stops at three data bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMark {
    pub start: bool,
    pub end: bool,
    pub independent: bool,
    pub discardable: bool,
    pub base_layer_sync: bool,
    /// Temporal layer id, 0..=7.
    pub temporal_id: u8,
    pub spatial_id: Option<u8>,
    pub tl0_pic_idx: Option<u8>,
    pub extra: Option<u8>,
}

impl FrameMark {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > 4 {
            return Err(Error::BadFrameMark);
        }
        let flags = bytes[0];
        Ok(Self {
            start: flags & 0x80 != 0,
            end: flags & 0x40 != 0,
            independent: flags & 0x20 != 0,
            discardable: flags & 0x10 != 0,
            base_layer_sync: flags & 0x08 != 0,
            temporal_id: flags & 0x07,
            spatial_id: bytes.get(1).copied(),
            tl0_pic_idx: bytes.get(2).copied(),
            extra: bytes.get(3).copied(),
        })
    }

    pub fn to_bytes(self) -> Vec<u8> {
        let mut flags = self.temporal_id & 0x07;
        if self.base_layer_sync {
            flags |= 0x08;
        }
        if self.discardable {
            flags |= 0x10;
        }
        if self.independent {
            flags |= 0x20;
        }
        if self.end {
            flags |= 0x40;
        }
        if self.start {
            flags |= 0x80;
        }
        let mut out = vec![flags];
        if self.spatial_id.is_some() || self.tl0_pic_idx.is_some() || self.extra.is_some() {
            out.push(self.spatial_id.unwrap_or(0));
        }
        if self.tl0_pic_idx.is_some() || self.extra.is_some() {
            out.push(self.tl0_pic_idx.unwrap_or(0));
        }
        if let Some(extra) = self.extra {
            out.push(extra);
        }
        out
    }
}

/// One encoded video frame as a LOC object.
///
/// `group_id` increments at an independent frame. `object_id` increments for
/// each frame inside the group and is 0 for the independent frame. That is
/// the single-track mapping in section 4.2 of the LOC draft.
pub fn video_frame(
    group_id: u64,
    object_id: u64,
    timestamp: u64,
    timescale: Option<u64>,
    marking: Option<&FrameMark>,
    config: Option<&[u8]>,
    payload: Vec<u8>,
) -> Object {
    let mut public = Vec::new();
    if let Some(scale) = timescale {
        public.push(Property::Timescale(scale));
    }
    if let Some(cfg) = config {
        public.push(Property::VideoConfig(cfg.to_vec()));
    }
    if let Some(mark) = marking {
        public.push(Property::VideoFrameMarking(mark.to_bytes()));
    }
    public.push(Property::Timestamp(timestamp));
    Object {
        group_id,
        subgroup_id: 0,
        object_id,
        public,
        private: Vec::new(),
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_mark_bits() {
        let mark = FrameMark {
            start: true,
            end: true,
            independent: true,
            discardable: false,
            base_layer_sync: true,
            temporal_id: 2,
            spatial_id: None,
            tl0_pic_idx: None,
            extra: None,
        };
        let bytes = mark.to_bytes();
        assert_eq!(bytes, vec![0xe0 | 0x08 | 2]);
        assert_eq!(FrameMark::parse(&bytes).unwrap(), mark);
    }

    #[test]
    fn long_mark_roundtrip() {
        let mark = FrameMark {
            start: true,
            end: false,
            independent: false,
            discardable: true,
            base_layer_sync: false,
            temporal_id: 1,
            spatial_id: Some(3),
            tl0_pic_idx: Some(9),
            extra: None,
        };
        assert_eq!(FrameMark::parse(&mark.to_bytes()).unwrap(), mark);
        assert!(FrameMark::parse(&[]).is_err());
        assert!(FrameMark::parse(&[0, 1, 2, 3, 4]).is_err());
    }

    #[test]
    fn idr_then_delta() {
        let idr_mark = FrameMark {
            start: true,
            end: true,
            independent: true,
            discardable: false,
            base_layer_sync: false,
            temporal_id: 0,
            spatial_id: None,
            tl0_pic_idx: None,
            extra: None,
        };
        let idr = video_frame(
            7,
            0,
            0,
            Some(90000),
            Some(&idr_mark),
            Some(&[1, 2, 3]),
            vec![9],
        );
        let delta = video_frame(7, 1, 3000, None, None, None, vec![8]);
        assert_eq!(idr.object_id, 0);
        assert_eq!(delta.group_id, idr.group_id);
        assert_eq!(idr.video_config(), Some(&[1, 2, 3][..]));
        assert!(idr.video_frame_marking().unwrap()[0] & 0x20 != 0);
        assert!(delta.video_config().is_none());
        assert_eq!(delta.timestamp(), Some(3000));
    }
}
