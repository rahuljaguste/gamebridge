// SPDX-License-Identifier: AGPL-3.0-only
//! H.264 access-unit inspection for keyframe/profile-change decisions.

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NalScan {
    pub is_idr: bool,
    pub has_sps: bool,
    pub has_pps: bool,
}

/// Scan an Annex-B access unit for H.264 NAL types. Also accepts a bare NAL
/// without a start code.
pub fn scan_annex_b(data: &[u8]) -> NalScan {
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
        if let Some(&header) = data.get(off) {
            let ty = header & 0x1f;
            match ty {
                5 => out.is_idr = true,
                7 => out.has_sps = true,
                8 => out.has_pps = true,
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_idr_and_params() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 4, 0, 0, 1, 0x65, 5, 6,
        ];
        let scan = scan_annex_b(&data);
        assert!(scan.is_idr);
        assert!(scan.has_sps);
        assert!(scan.has_pps);
    }

    #[test]
    fn detects_non_idr() {
        let data = [0, 0, 0, 1, 0x41, 5, 6];
        assert!(!scan_annex_b(&data).is_idr);
    }
}
