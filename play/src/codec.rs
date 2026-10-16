//! Codecs a play picture can use, slowest last.
//!
//! The list is ordered by codec latency. Compression and hardware notes sit
//! on each entry. [`recommend`] applies the hardware-encode and CPU rules.

/// Identity in [`catalog`], lowest latency first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecId {
    Pyrowave,
    Mjpeg,
    H264,
    H265,
    Av1,
}

/// One row in the picture preference list.
///
/// H.264 is two rows: hardware encode, then a CPU encode. AV1 and H.265 are
/// hardware-only. Pyrowave and MJPEG have no second path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PicturePref {
    Av1Hw,
    H265Hw,
    H264Hw,
    Pyrowave,
    Mjpeg,
    H264Sw,
    /// PyroWave on a CPU Vulkan driver (lavapipe, SwiftShader).
    PyrowaveCpu,
}

impl PicturePref {
    pub fn codec(self) -> CodecId {
        match self {
            Self::Av1Hw => CodecId::Av1,
            Self::H265Hw => CodecId::H265,
            Self::H264Hw | Self::H264Sw => CodecId::H264,
            Self::Pyrowave | Self::PyrowaveCpu => CodecId::Pyrowave,
            Self::Mjpeg => CodecId::Mjpeg,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Av1Hw => "AV1 (HW)",
            Self::H265Hw => "H.265 (HW)",
            Self::H264Hw => "H.264 (HW)",
            Self::Pyrowave => "PyroWave (GPU)",
            Self::Mjpeg => "MJPEG",
            Self::H264Sw => "H.264 (SW)",
            Self::PyrowaveCpu => "PyroWave (CPU)",
        }
    }

    /// Default preference order.
    /// Default preference order. CPU PyroWave is last, below MJPEG and x264.
    pub fn order() -> [Self; 7] {
        [
            Self::Av1Hw,
            Self::H265Hw,
            Self::H264Hw,
            Self::Pyrowave,
            Self::Mjpeg,
            Self::H264Sw,
            Self::PyrowaveCpu,
        ]
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Av1Hw => "av1-hw",
            Self::H265Hw => "h265-hw",
            Self::H264Hw => "h264-hw",
            Self::Pyrowave => "pyrowave",
            Self::Mjpeg => "mjpeg",
            Self::H264Sw => "h264-sw",
            Self::PyrowaveCpu => "pyrowave-cpu",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::order().into_iter().find(|pref| pref.token() == token)
    }
}

/// What this machine can do, and the latency profile of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecSupport {
    /// A video-encode ASIC is available on the sender (NVENC, Quick Sync, AMF, VideoToolbox).
    pub hardware_encode: bool,
    /// A video-decode ASIC is available on the receiver.
    pub hardware_decode: bool,
    /// The sender CPU can sustain this codec at the session rate.
    pub cpu_can_sustain: bool,
    /// The session wants the least codec delay it can get.
    pub very_low_latency: bool,
    /// Vulkan compute is available on both ends. This is not an encode ASIC.
    pub gpu_compute: bool,
}

impl Default for CodecSupport {
    fn default() -> Self {
        Self {
            hardware_encode: false,
            hardware_decode: false,
            cpu_can_sustain: false,
            very_low_latency: false,
            gpu_compute: false,
        }
    }
}

/// One row of [`catalog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecInfo {
    pub id: CodecId,
    pub name: &'static str,
    /// Smaller means less codec delay. [`catalog`] sorts by this.
    pub latency_rank: u8,
    /// How the codec spends bits.
    pub compression: &'static str,
    /// Dedicated video-encode ASIC required on the sender.
    pub hardware_encode_required: bool,
    /// Dedicated video-decode ASIC required on the receiver.
    pub hardware_decode_required: bool,
    /// Vulkan compute on both ends, not a video-encode ASIC.
    pub gpu_compute_both_ends: bool,
    /// Hardware rule in words. Encode is the requirement that matters most.
    pub hardware: &'static str,
}

const TABLE: &[CodecInfo] = &[
    CodecInfo {
        id: CodecId::Pyrowave,
        name: "Pyrowave",
        latency_rank: 0,
        compression: "spends bandwidth, about 5–10× H.264/HEVC/AV1",
        hardware_encode_required: false,
        hardware_decode_required: false,
        gpu_compute_both_ends: true,
        hardware: "GPU compute on both ends. Direct3D 11 feature level 11_0 counts as hardware encode, not a video-encode ASIC",
    },
    CodecInfo {
        id: CodecId::Mjpeg,
        name: "MJPEG",
        latency_rank: 1,
        compression: "intra, weak compression",
        hardware_encode_required: false,
        hardware_decode_required: false,
        gpu_compute_both_ends: false,
        hardware: "does not require dedicated encode hardware",
    },
    CodecInfo {
        id: CodecId::H264,
        name: "H.264",
        latency_rank: 2,
        compression: "compresses well",
        hardware_encode_required: false,
        hardware_decode_required: false,
        gpu_compute_both_ends: false,
        hardware: "hardware encode and hardware decode on both ends are the capable path; hardware encode matters most. A CPU encode is acceptable only when that CPU can sustain it. Not used at a very-low-latency profile",
    },
    CodecInfo {
        id: CodecId::H265,
        name: "H.265",
        latency_rank: 3,
        compression: "compresses better than H.264",
        hardware_encode_required: true,
        hardware_decode_required: true,
        gpu_compute_both_ends: false,
        hardware: "hardware encode and hardware decode are required on both ends. Hardware encode is the requirement that matters most",
    },
    CodecInfo {
        id: CodecId::Av1,
        name: "AV1",
        latency_rank: 4,
        compression: "compresses best",
        hardware_encode_required: true,
        hardware_decode_required: true,
        gpu_compute_both_ends: false,
        hardware: "hardware encode and hardware decode are required on both ends. Hardware encode is the requirement that matters most",
    },
];

/// Catalog sorted by codec latency, lowest first.
pub fn catalog() -> Vec<CodecInfo> {
    let mut rows = TABLE.to_vec();
    rows.sort_by_key(|row| row.latency_rank);
    rows
}

/// Whether this session should offer `codec`.
///
/// Rows that require an encode ASIC, a decode ASIC, or Vulkan compute on both
/// ends are refused when that support is missing. Hardware encode is the
/// requirement that matters most for H.265 and AV1: without it they are not
/// offered. H.264 is offered when an encode ASIC is present or the CPU can
/// sustain it, except at a very-low-latency profile. MJPEG stays offered there.
pub fn recommend(codec: CodecId, support: &CodecSupport) -> bool {
    let Some(info) = catalog().into_iter().find(|row| row.id == codec) else {
        return false;
    };
    if info.hardware_encode_required && !support.hardware_encode {
        return false;
    }
    if info.hardware_decode_required && !support.hardware_decode {
        return false;
    }
    if info.gpu_compute_both_ends && !support.gpu_compute {
        return false;
    }
    match codec {
        CodecId::Pyrowave | CodecId::Mjpeg => true,
        CodecId::H264 => {
            if support.very_low_latency {
                return false;
            }
            support.hardware_encode || support.cpu_can_sustain
        }
        CodecId::H265 | CodecId::Av1 => true,
    }
}

/// What one end can do for a single codec.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndSupport {
    pub hardware_encode: bool,
    pub hardware_decode: bool,
    pub cpu_can_sustain: bool,
    pub gpu_compute: bool,
}

/// Hardware and CPU support reported by one computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineCodecs {
    ends: Vec<(CodecId, EndSupport)>,
}

impl MachineCodecs {
    pub fn from_rows(ends: Vec<(CodecId, EndSupport)>) -> Self {
        Self { ends }
    }

    pub fn get(&self, id: CodecId) -> EndSupport {
        self.ends
            .iter()
            .find(|(codec, _)| *codec == id)
            .map(|(_, end)| *end)
            .unwrap_or_default()
    }

    pub fn rows(&self) -> &[(CodecId, EndSupport)] {
        &self.ends
    }
}

/// A preference that lost, and the reason the next one is tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecSkip {
    pub pref: PicturePref,
    pub reason: &'static str,
}

/// Desktop audio identities. PCM is what the play session sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioId {
    Pcm,
    Opus,
    Aac,
}

impl AudioId {
    pub fn name(self) -> &'static str {
        match self {
            AudioId::Pcm => "PCM",
            AudioId::Opus => "Opus",
            AudioId::Aac => "AAC",
        }
    }

    /// Default preference order.
    pub fn order() -> [Self; 3] {
        [Self::Opus, Self::Aac, Self::Pcm]
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Aac => "aac",
            Self::Pcm => "pcm",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::order()
            .into_iter()
            .find(|codec| codec.token() == token)
    }
}

/// Whether a computer can run each audio codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioMachine {
    pub pcm: bool,
    pub opus: bool,
    pub aac: bool,
}

impl Default for AudioMachine {
    fn default() -> Self {
        Self {
            pcm: true,
            opus: false,
            aac: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSkip {
    pub codec: AudioId,
    pub reason: &'static str,
}

/// First preference both ends can run.
///
/// `local_encodes` is true when this PC sends the picture. H.265 (HW) and
/// AV1 (HW) are skipped when the remote computer has no hardware decode for
/// them, or when the sender has no hardware encode. H.264 (HW) needs hardware
/// encode on the sender. H.264 (SW) needs a CPU that can sustain it, and it is
/// skipped when `skip_without_hardware_encode` is set. That switch also skips
/// CPU PyroWave. GPU PyroWave stays when the sender can encode it on a GPU.
/// MJPEG stays.
pub fn choose_codec(
    preference: &[PicturePref],
    local: &MachineCodecs,
    remote: &MachineCodecs,
    local_encodes: bool,
    skip_without_hardware_encode: bool,
) -> (Option<PicturePref>, Vec<CodecSkip>) {
    let mut skipped = Vec::new();
    for pref in preference {
        let codec = pref.codec();
        match codec_gate(
            *pref,
            &local.get(codec),
            &remote.get(codec),
            local_encodes,
            skip_without_hardware_encode,
        ) {
            Ok(()) => return (Some(*pref), skipped),
            Err(reason) => skipped.push(CodecSkip {
                pref: *pref,
                reason,
            }),
        }
    }
    (None, skipped)
}

fn codec_gate(
    pref: PicturePref,
    local: &EndSupport,
    remote: &EndSupport,
    local_encodes: bool,
    skip_without_hardware_encode: bool,
) -> Result<(), &'static str> {
    let encoder = if local_encodes { local } else { remote };
    let decoder = if local_encodes { remote } else { local };
    match pref {
        PicturePref::Pyrowave => {
            // Direct3D 11 and a non-CPU Vulkan device are hardware encode.
            // The skip-without-hardware switch keeps this row. CPU PyroWave
            // is the other preference.
            if !(encoder.hardware_encode || encoder.gpu_compute) {
                return Err("no hardware encode");
            }
            if decoder.gpu_compute || decoder.hardware_decode {
                Ok(())
            } else {
                Err("no gpu compute on the receiver")
            }
        }
        PicturePref::PyrowaveCpu => {
            if skip_without_hardware_encode {
                Err("no hardware encode")
            } else if decoder.gpu_compute {
                Ok(())
            } else {
                Err("no gpu compute on the receiver")
            }
        }
        PicturePref::Mjpeg => Ok(()),
        PicturePref::H264Hw => {
            if encoder.hardware_encode {
                Ok(())
            } else {
                Err("no hardware encode")
            }
        }
        PicturePref::H264Sw => {
            if skip_without_hardware_encode {
                Err("no hardware encode")
            } else if encoder.cpu_can_sustain {
                Ok(())
            } else {
                Err("the cpu cannot sustain it")
            }
        }
        PicturePref::H265Hw | PicturePref::Av1Hw => {
            if !remote.hardware_decode {
                return Err("remote has no hardware decode");
            }
            if !encoder.hardware_encode {
                return Err("no hardware encode");
            }
            if !decoder.hardware_decode {
                return Err("no hardware decode");
            }
            Ok(())
        }
    }
}

/// First audio preference both computers can run. PCM is always available.
/// Opus and AAC are used only when both computers can run them.
pub fn choose_audio(
    preference: &[AudioId],
    local: &AudioMachine,
    remote: &AudioMachine,
) -> (Option<AudioId>, Vec<AudioSkip>) {
    let mut skipped = Vec::new();
    for codec in preference {
        match codec {
            AudioId::Pcm => {
                if local.pcm && remote.pcm {
                    return (Some(AudioId::Pcm), skipped);
                }
                skipped.push(AudioSkip {
                    codec: AudioId::Pcm,
                    reason: "pcm is missing",
                });
            }
            AudioId::Opus => {
                if local.opus && remote.opus {
                    return (Some(AudioId::Opus), skipped);
                }
                let reason = if !remote.opus {
                    "remote has no opus"
                } else {
                    "this computer has no opus"
                };
                skipped.push(AudioSkip {
                    codec: AudioId::Opus,
                    reason,
                });
            }
            AudioId::Aac => {
                if local.aac && remote.aac {
                    return (Some(AudioId::Aac), skipped);
                }
                let reason = if !remote.aac {
                    "remote has no aac"
                } else {
                    "this computer has no aac"
                };
                skipped.push(AudioSkip {
                    codec: AudioId::Aac,
                    reason,
                });
            }
        }
    }
    (None, skipped)
}

pub fn codec_name(id: CodecId) -> &'static str {
    catalog()
        .into_iter()
        .find(|row| row.id == id)
        .map(|row| row.name)
        .unwrap_or("codec")
}

/// Support this process can see. An encode ASIC is recorded only for NVIDIA,
/// which exposes NVENC and NVDEC. AV1 is left unset. CPU encode is not assumed.
pub fn this_computer() -> MachineCodecs {
    let nvidia = etherquic::nvidia_present();
    let gpu = etherquic::pyrowave_ready(
        etherquic::gpu_compute_present(),
        etherquic::pyrowave_library_present(),
    );
    let asic = EndSupport {
        hardware_encode: nvidia,
        hardware_decode: nvidia,
        cpu_can_sustain: false,
        gpu_compute: false,
    };
    MachineCodecs::from_rows(vec![
        (
            CodecId::Pyrowave,
            EndSupport {
                hardware_encode: etherquic::pyrowave_hardware_encode(),
                gpu_compute: gpu,
                ..EndSupport::default()
            },
        ),
        (CodecId::Mjpeg, EndSupport::default()),
        (CodecId::H264, asic),
        (CodecId::H265, asic),
        (CodecId::Av1, EndSupport::default()),
    ])
}

/// Opus is set when libopus, ffmpeg, or GStreamer is present. The window's
/// preference narrows that to the selected tool.
pub fn this_audio() -> AudioMachine {
    let probe = etherquic::probe_backends();
    AudioMachine {
        pcm: true,
        opus: probe.libopus || probe.ffmpeg || probe.gstreamer,
        aac: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support() -> CodecSupport {
        CodecSupport::default()
    }

    #[test]
    fn catalog_is_latency_order_with_compression_and_hardware() {
        let rows = catalog();
        for row in &rows {
            println!(
                "codec {} rank {} compression [{}] encode_asic {} decode_asic {} gpu_compute {} hardware [{}]",
                row.name,
                row.latency_rank,
                row.compression,
                row.hardware_encode_required,
                row.hardware_decode_required,
                row.gpu_compute_both_ends,
                row.hardware
            );
        }
        let ids: Vec<_> = rows.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![
                CodecId::Pyrowave,
                CodecId::Mjpeg,
                CodecId::H264,
                CodecId::H265,
                CodecId::Av1,
            ]
        );
        for pair in rows.windows(2) {
            assert!(pair[0].latency_rank < pair[1].latency_rank);
        }

        let pyro = &rows[0];
        assert!(pyro.gpu_compute_both_ends);
        assert!(!pyro.hardware_encode_required);
        assert!(!pyro.hardware_decode_required);
        assert!(pyro.hardware.contains("GPU compute"));
        assert!(pyro.hardware.contains("both ends"));
        assert!(pyro.hardware.contains("not a video-encode ASIC"));
        assert!(pyro.compression.contains("5–10×"));

        let mjpeg = &rows[1];
        assert!(mjpeg.compression.contains("intra"));
        assert!(mjpeg.compression.contains("weak compression"));
        assert!(!mjpeg.hardware_encode_required);
        assert!(!mjpeg.hardware_decode_required);
        assert!(mjpeg
            .hardware
            .contains("does not require dedicated encode hardware"));

        assert!(rows[2].compression.contains("compresses well"));
        assert!(rows[3].compression.contains("better than H.264"));
        assert!(rows[3].hardware_encode_required && rows[3].hardware_decode_required);
        assert!(rows[3]
            .hardware
            .contains("Hardware encode is the requirement that matters most"));
        assert!(rows[4].compression.contains("compresses best"));
        assert!(rows[4].hardware_encode_required && rows[4].hardware_decode_required);
    }

    #[test]
    fn recommend_follows_encode_cpu_and_latency_profile() {
        let mut bare = support();
        println!(
            "h265 encode {} decode {} recommended {}",
            bare.hardware_encode,
            bare.hardware_decode,
            recommend(CodecId::H265, &bare)
        );
        println!(
            "av1 encode {} decode {} recommended {}",
            bare.hardware_encode,
            bare.hardware_decode,
            recommend(CodecId::Av1, &bare)
        );
        assert!(!recommend(CodecId::H265, &bare));
        assert!(!recommend(CodecId::Av1, &bare));
        bare.hardware_decode = true;
        assert!(!recommend(CodecId::H265, &bare));
        assert!(!recommend(CodecId::Av1, &bare));

        let mut encoded = support();
        encoded.hardware_encode = true;
        encoded.hardware_decode = true;
        assert!(recommend(CodecId::H265, &encoded));
        assert!(recommend(CodecId::Av1, &encoded));

        let mut h264 = support();
        println!(
            "h264 encode {} cpu {} very_low {} recommended {}",
            h264.hardware_encode,
            h264.cpu_can_sustain,
            h264.very_low_latency,
            recommend(CodecId::H264, &h264)
        );
        assert!(!recommend(CodecId::H264, &h264));
        h264.cpu_can_sustain = true;
        assert!(recommend(CodecId::H264, &h264));
        h264.cpu_can_sustain = false;
        h264.hardware_encode = true;
        assert!(recommend(CodecId::H264, &h264));

        h264.very_low_latency = true;
        h264.cpu_can_sustain = true;
        println!(
            "h264 encode {} cpu {} very_low {} recommended {}",
            h264.hardware_encode,
            h264.cpu_can_sustain,
            h264.very_low_latency,
            recommend(CodecId::H264, &h264)
        );
        println!(
            "mjpeg very_low {} recommended {}",
            h264.very_low_latency,
            recommend(CodecId::Mjpeg, &h264)
        );
        assert!(!recommend(CodecId::H264, &h264));
        assert!(recommend(CodecId::Mjpeg, &h264));
    }

    fn machine(rows: &[(CodecId, EndSupport)]) -> MachineCodecs {
        MachineCodecs::from_rows(rows.to_vec())
    }

    #[test]
    fn h265_without_remote_hardware_decode_uses_mjpeg() {
        let local = machine(&[(
            CodecId::H265,
            EndSupport {
                hardware_encode: true,
                hardware_decode: true,
                ..EndSupport::default()
            },
        )]);
        let remote = machine(&[(
            CodecId::H265,
            EndSupport {
                hardware_encode: true,
                hardware_decode: false,
                ..EndSupport::default()
            },
        )]);
        let (chosen, skipped) = choose_codec(
            &[PicturePref::H265Hw, PicturePref::Mjpeg],
            &local,
            &remote,
            false,
            false,
        );
        assert_eq!(chosen, Some(PicturePref::Mjpeg));
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].pref, PicturePref::H265Hw);
        assert_eq!(skipped[0].reason, "remote has no hardware decode");
    }

    #[test]
    fn h265_is_kept_when_the_remote_can_decode_and_the_encoder_has_hardware() {
        let local = machine(&[(
            CodecId::H265,
            EndSupport {
                hardware_decode: true,
                ..EndSupport::default()
            },
        )]);
        let remote = machine(&[(
            CodecId::H265,
            EndSupport {
                hardware_encode: true,
                hardware_decode: true,
                ..EndSupport::default()
            },
        )]);
        let (chosen, skipped) = choose_codec(
            &[PicturePref::H265Hw, PicturePref::Mjpeg],
            &local,
            &remote,
            false,
            false,
        );
        assert_eq!(chosen, Some(PicturePref::H265Hw));
        assert!(skipped.is_empty());
    }

    #[test]
    fn h264_hardware_wins_and_software_is_skipped_when_asked() {
        let local = MachineCodecs::default();
        let cpu_only = machine(&[(
            CodecId::H264,
            EndSupport {
                hardware_encode: false,
                cpu_can_sustain: true,
                ..EndSupport::default()
            },
        )]);
        let order = [PicturePref::H264Hw, PicturePref::H264Sw, PicturePref::Mjpeg];
        let (kept, skipped) = choose_codec(&order, &local, &cpu_only, false, false);
        assert_eq!(kept, Some(PicturePref::H264Sw));
        assert_eq!(skipped[0].pref, PicturePref::H264Hw);
        assert_eq!(skipped[0].reason, "no hardware encode");
        let (chosen, skipped) = choose_codec(&order, &local, &cpu_only, false, true);
        assert_eq!(chosen, Some(PicturePref::Mjpeg));
        assert_eq!(skipped[1].pref, PicturePref::H264Sw);
        assert_eq!(skipped[1].reason, "no hardware encode");

        let hardware = machine(&[(
            CodecId::H264,
            EndSupport {
                hardware_encode: true,
                cpu_can_sustain: true,
                ..EndSupport::default()
            },
        )]);
        let (chosen, _) = choose_codec(&order, &local, &hardware, false, false);
        assert_eq!(chosen, Some(PicturePref::H264Hw));
    }

    #[test]
    fn opus_without_remote_support_uses_pcm() {
        let (chosen, skipped) = choose_audio(
            &[AudioId::Opus, AudioId::Pcm],
            &AudioMachine {
                pcm: true,
                opus: true,
                aac: false,
            },
            &AudioMachine {
                pcm: true,
                opus: false,
                aac: false,
            },
        );
        assert_eq!(chosen, Some(AudioId::Pcm));
        assert_eq!(skipped[0].codec, AudioId::Opus);
        assert_eq!(skipped[0].reason, "remote has no opus");
    }

    #[test]
    fn opus_without_a_local_tool_names_this_computer() {
        let (chosen, skipped) = choose_audio(
            &[AudioId::Opus, AudioId::Pcm],
            &AudioMachine {
                pcm: true,
                opus: false,
                aac: false,
            },
            &AudioMachine {
                pcm: true,
                opus: true,
                aac: true,
            },
        );
        assert_eq!(chosen, Some(AudioId::Pcm));
        assert_eq!(skipped[0].reason, "this computer has no opus");
    }

    #[test]
    fn gpu_pyrowave_wins_the_default_order_when_asked_to_skip_software() {
        let order = PicturePref::order();
        let local = machine(&[(
            CodecId::Pyrowave,
            EndSupport {
                gpu_compute: true,
                ..EndSupport::default()
            },
        )]);
        let remote = machine(&[(
            CodecId::Pyrowave,
            EndSupport {
                hardware_encode: true,
                gpu_compute: true,
                ..EndSupport::default()
            },
        )]);
        let (chosen, skipped) = choose_codec(&order, &local, &remote, false, true);
        assert_eq!(chosen, Some(PicturePref::Pyrowave));
        assert!(skipped.iter().any(|skip| skip.pref == PicturePref::H264Hw));
        assert!(skipped
            .iter()
            .all(|skip| skip.pref != PicturePref::Pyrowave));

        let cpu_remote = machine(&[(
            CodecId::Pyrowave,
            EndSupport {
                gpu_compute: true,
                ..EndSupport::default()
            },
        )]);
        let (chosen, _) = choose_codec(
            &[PicturePref::PyrowaveCpu, PicturePref::Mjpeg],
            &local,
            &cpu_remote,
            false,
            true,
        );
        assert_eq!(chosen, Some(PicturePref::Mjpeg));
    }

    #[test]
    fn this_computer_lists_the_catalog_and_does_not_invent_av1_or_mjpeg_asics() {
        let machine = this_computer();
        for row in catalog() {
            let _ = machine.get(row.id);
        }
        let mjpeg = machine.get(CodecId::Mjpeg);
        assert!(!mjpeg.hardware_encode && !mjpeg.hardware_decode);
        let av1 = machine.get(CodecId::Av1);
        assert!(!av1.hardware_encode && !av1.hardware_decode);
    }
}
