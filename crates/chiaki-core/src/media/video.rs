// SPDX-License-Identifier: AGPL-3.0-only
//! Codec-aware Annex-B access-unit inspection for keyframe/profile decisions.

use crate::target::Codec;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NalScan {
    pub is_idr: bool,
    pub has_vps: bool,
    pub has_sps: bool,
    pub has_pps: bool,
}

/// Scan an Annex-B access unit for the negotiated codec's NAL types. Also
/// accepts a bare NAL without a start code.
///
/// H.264 keyframes are IDR NAL type 5. H.265 keyframes are any IRAP type
/// (BLA/IDR/CRA, 16..=21); HEVC additionally has a VPS alongside SPS/PPS.
pub fn scan_annex_b(data: &[u8], codec: Codec) -> NalScan {
    let mut out = NalScan::default();
    let mut offsets = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                offsets.push(i + 3);
                i += 3;
                continue;
            }
            if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                offsets.push(i + 4);
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    if offsets.is_empty()
        && let Some(first) = data.first()
        && first & 0x80 == 0
    {
        offsets.push(0);
    }
    for off in offsets {
        match codec {
            Codec::H264 => {
                if let Some(&header) = data.get(off) {
                    match header & 0x1f {
                        5 => out.is_idr = true,
                        7 => out.has_sps = true,
                        8 => out.has_pps = true,
                        _ => {}
                    }
                }
            }
            Codec::H265 | Codec::H265Hdr => {
                // Two-byte NAL header: forbidden_zero_bit, nal_unit_type (6),
                // nuh_layer_id (6), nuh_temporal_id_plus1 (3).
                if let Some(&header) = data.get(off) {
                    match (header >> 1) & 0x3f {
                        16..=21 => out.is_idr = true,
                        32 => out.has_vps = true,
                        33 => out.has_sps = true,
                        34 => out.has_pps = true,
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_h264_idr_and_params() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 4, 0, 0, 1, 0x65, 5, 6,
        ];
        let scan = scan_annex_b(&data, Codec::H264);
        assert!(scan.is_idr);
        assert!(scan.has_sps);
        assert!(scan.has_pps);
        assert!(!scan.has_vps);
    }

    #[test]
    fn detects_h264_non_idr() {
        let data = [0, 0, 0, 1, 0x41, 5, 6];
        assert!(!scan_annex_b(&data, Codec::H264).is_idr);
    }

    #[test]
    fn detects_hevc_irap_and_parameter_sets() {
        // VPS (32), SPS (33), PPS (34), IDR_W_RADL (19).
        let data = [
            0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1,
        ];
        let scan = scan_annex_b(&data, Codec::H265);
        assert!(scan.is_idr);
        assert!(scan.has_vps);
        assert!(scan.has_sps);
        assert!(scan.has_pps);
    }

    #[test]
    fn hevc_trailing_picture_is_not_a_keyframe() {
        // TRAIL_R (1): header byte 0x02.
        let data = [0, 0, 0, 1, 0x02, 1, 2];
        assert!(!scan_annex_b(&data, Codec::H265Hdr).is_idr);
    }
}
