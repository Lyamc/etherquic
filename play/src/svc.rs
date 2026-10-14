//! Scalable picture layers over an RFC 9626 frame mark.
//!
//! A jittery path keeps the independent base and drops a discardable
//! enhancement. The base payload is what the picture decoder still sees.

use etherquic_container::FrameMark;

/// One SVC layer: a container frame mark plus the encoded picture bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvcLayer {
    pub mark: FrameMark,
    pub payload: Vec<u8>,
}

/// Drop discardable enhancements. The base and any other non-discardable
/// layer stay, in the order they arrived.
pub fn drop_jitter_enhancement(layers: Vec<SvcLayer>) -> Vec<SvcLayer> {
    layers
        .into_iter()
        .filter(|layer| !layer.mark.discardable)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::{decode_picture, encode_picture, Picture, PictureCodec};

    fn mark(independent: bool, discardable: bool, temporal_id: u8) -> FrameMark {
        let built = FrameMark {
            start: true,
            end: true,
            independent,
            discardable,
            base_layer_sync: false,
            temporal_id,
            spatial_id: None,
            tl0_pic_idx: None,
            extra: None,
        };
        FrameMark::parse(&built.to_bytes()).expect("frame mark")
    }

    #[test]
    fn jitter_keeps_the_base_and_does_not_decode_the_enhancement() {
        let base = Picture {
            width: 4,
            height: 4,
            rgb: vec![40; 4 * 4 * 3],
        };
        let enhancement = Picture {
            width: 2,
            height: 2,
            rgb: vec![200; 2 * 2 * 3],
        };
        let base_payload = encode_picture(PictureCodec::Mjpeg, &base, 90).expect("base");
        let enhancement_payload =
            encode_picture(PictureCodec::Intra, &enhancement, 90).expect("enhancement");
        let layers = vec![
            SvcLayer {
                mark: mark(true, false, 0),
                payload: base_payload,
            },
            SvcLayer {
                mark: mark(false, true, 1),
                payload: enhancement_payload.clone(),
            },
        ];

        let kept = drop_jitter_enhancement(layers);
        println!(
            "svc kept {} enhancement_still_present {}",
            kept.len(),
            kept.iter()
                .any(|layer| layer.payload == enhancement_payload)
        );
        assert_eq!(kept.len(), 1);
        assert!(kept
            .iter()
            .all(|layer| layer.payload != enhancement_payload));
        assert!(!kept[0].mark.discardable);

        let picture = decode_picture(&kept[0].payload).expect("base decodes");
        println!(
            "svc base {}x{} rgb {}",
            picture.width,
            picture.height,
            picture.rgb.len()
        );
        assert!(!picture.rgb.is_empty());
        assert_eq!((picture.width, picture.height), (base.width, base.height));
    }
}
