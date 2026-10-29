// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Codec-aware Annex-B access-unit inspection for keyframe/profile decisions.

use crate::target::Codec;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NalScan {
    pub is_idr: bool,
    pub has_vps: bool,
    pub has_sps: bool,
    pub has_pps: bool,
}

/// One NAL unit in an Annex-B byte stream. `start` is the start-code offset
/// (so `start..end` is the whole unit), `header` is the first NAL byte, and
/// `end` is the next start code or the end of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalUnit {
    pub start: usize,
    pub header: usize,
    pub end: usize,
}

fn start_code_len(data: &[u8], i: usize) -> Option<usize> {
    if i + 3 <= data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
        Some(3)
    } else if i + 4 <= data.len()
        && data[i] == 0
        && data[i + 1] == 0
        && data[i + 2] == 0
        && data[i + 3] == 1
    {
        Some(4)
    } else {
        None
    }
}

/// Split an Annex-B byte stream into NAL units. A bare NAL without a start
/// code is treated as a single unit.
pub fn annex_b_units(data: &[u8]) -> Vec<NalUnit> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if let Some(len) = start_code_len(data, i) {
            starts.push((i, i + len));
            i += len;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        return match data.first() {
            Some(header) if header & 0x80 == 0 => vec![NalUnit {
                start: 0,
                header: 0,
                end: data.len(),
            }],
            _ => Vec::new(),
        };
    }
    let mut units = Vec::with_capacity(starts.len());
    for (index, &(start, header)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(data.len(), |&(next, _)| next);
        units.push(NalUnit { start, header, end });
    }
    units
}

/// Scan an Annex-B access unit for the negotiated codec's NAL types. Also
/// accepts a bare NAL without a start code.
///
/// H.264 keyframes are IDR NAL type 5. H.265 keyframes are any IRAP type
/// (BLA/IDR/CRA, 16..=21); HEVC additionally has a VPS alongside SPS/PPS.
pub fn scan_annex_b(data: &[u8], codec: Codec) -> NalScan {
    let mut out = NalScan::default();
    for unit in annex_b_units(data) {
        let Some(&header) = data.get(unit.header) else {
            continue;
        };
        match codec {
            Codec::H264 => match header & 0x1f {
                5 => out.is_idr = true,
                7 => out.has_sps = true,
                8 => out.has_pps = true,
                _ => {}
            },
            Codec::H265 | Codec::H265Hdr => {
                // Two-byte NAL header: forbidden_zero_bit, nal_unit_type (6),
                // nuh_layer_id (6), nuh_temporal_id_plus1 (3).
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

    #[test]
    fn annex_b_units_cover_start_codes_and_bare_nals() {
        let data = [0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2];
        let units = annex_b_units(&data);
        assert_eq!(units.len(), 2);
        assert_eq!((units[0].start, units[0].header, units[0].end), (0, 3, 5));
        assert_eq!((units[1].start, units[1].header, units[1].end), (5, 9, 11));

        // A trailing start code yields a unit whose header is out of range.
        let trailing = annex_b_units(&[0, 0, 1]);
        assert_eq!(trailing.len(), 1);
        assert_eq!(trailing[0].header, 3);

        // A bare NAL is one unit starting at the first byte.
        let bare = annex_b_units(&[0x65, 1, 2]);
        assert_eq!(bare.len(), 1);
        assert_eq!((bare[0].start, bare[0].header, bare[0].end), (0, 0, 3));

        // A leading zero byte is part of the four-byte start code.
        let leading = annex_b_units(&[0, 0, 0, 1, 0x67]);
        assert_eq!(leading[0].header, 4);
    }
}
