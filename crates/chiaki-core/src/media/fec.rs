// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Cauchy Reed-Solomon erasure decoding, matching jerasure's
//! `cauchy_original_coding_matrix` with w=8 and `jerasure_matrix_decode`.

use crate::error::{Error, Result};
use crate::media::gf256::{gf, invert};

/// cauchy_original_coding_matrix(k, m, 8): element (i, j) = 1 / (i ^ (m+j)).
fn cauchy_row(i: usize, k: usize, m: usize) -> Vec<u8> {
    let gf = gf();
    let mut row = vec![0u8; k];
    for (j, out) in row.iter_mut().enumerate() {
        let denom = (i ^ (m + j)) as u8;
        *out = gf.inv(denom).unwrap_or(0);
    }
    row
}

/// Decode missing symbols in a k+m slot buffer.
///
/// Each slot is `stride` bytes; only the first `unit_size` bytes are a symbol.
/// The function recovers erased source symbols in place.
pub fn decode(
    frame_buf: &mut [u8],
    unit_size: usize,
    stride: usize,
    k: usize,
    m: usize,
    erasures: &[usize],
) -> Result<()> {
    if stride < unit_size || k == 0 || m == 0 || k + m > 256 {
        return Err(Error::InvalidData);
    }
    if frame_buf.len() < stride * (k + m) {
        return Err(Error::BufferTooSmall);
    }
    if erasures.len() > m {
        return Err(Error::FecFailed);
    }
    let mut erased = vec![false; k + m];
    for &e in erasures {
        if e >= k + m {
            return Err(Error::InvalidData);
        }
        erased[e] = true;
    }

    // Select the first k surviving symbols and build the encoding matrix.
    let mut survivors = Vec::with_capacity(k);
    for (i, &is_erased) in erased.iter().enumerate() {
        if !is_erased {
            survivors.push(i);
            if survivors.len() == k {
                break;
            }
        }
    }
    if survivors.len() < k {
        return Err(Error::FecFailed);
    }

    let gf = gf();
    let mut matrix = vec![0u8; k * k];
    for (row, &sym) in survivors.iter().enumerate() {
        if sym < k {
            matrix[row * k + sym] = 1;
        } else {
            let c = cauchy_row(sym - k, k, m);
            matrix[row * k..row * k + k].copy_from_slice(&c);
        }
    }
    let inverse = invert(&mut matrix, k)?;

    // Recover each erased source symbol.
    let mut tmp = vec![0u8; unit_size];
    for e in 0..k {
        if !erased[e] {
            continue;
        }
        tmp.fill(0);
        for (j, &sym) in survivors.iter().enumerate() {
            let coeff = inverse[e * k + j];
            if coeff == 0 {
                continue;
            }
            let src = &frame_buf[sym * stride..sym * stride + unit_size];
            gf.mul_add(src, coeff, &mut tmp);
        }
        // Write after all reads: an erased source slot is never a survivor.
        frame_buf[e * stride..e * stride + unit_size].copy_from_slice(&tmp);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode random source symbols and verify erasure recovery.
    #[test]
    fn round_trip_random_erasures() {
        let gf = gf();
        let k = 6;
        let m = 2;
        let unit = 64;
        let stride = 80;
        let source: Vec<Vec<u8>> = (0..k)
            .map(|i| (0..unit).map(|j| (i * 31 + j * 7 + 1) as u8).collect())
            .collect();
        let mut buf = vec![0u8; stride * (k + m)];
        for i in 0..k {
            buf[i * stride..i * stride + unit].copy_from_slice(&source[i]);
        }
        for p in 0..m {
            let row = cauchy_row(p, k, m);
            for j in 0..k {
                let src = buf[j * stride..j * stride + unit].to_vec();
                gf.mul_add(
                    &src,
                    row[j],
                    &mut buf[(k + p) * stride..(k + p) * stride + unit],
                );
            }
        }
        let reference = buf.clone();
        // Erase two source symbols (the parity capacity is m=2).
        let erasures = [1usize, 4];
        for &e in &erasures {
            buf[e * stride..e * stride + unit].fill(0x42);
        }
        decode(&mut buf, unit, stride, k, m, &erasures).unwrap();
        for i in 0..k {
            assert_eq!(
                &buf[i * stride..i * stride + unit],
                &reference[i * stride..i * stride + unit]
            );
        }
    }

    #[test]
    fn too_many_erasures_fails() {
        let mut buf = vec![0u8; 16 * 8];
        assert!(decode(&mut buf, 16, 16, 6, 1, &[0, 1, 2]).is_err());
    }

    mod vector {
        include!("../../tests/vectors/fec_case0.rs");
    }

    #[test]
    fn reference_vector_case0() {
        use crate::util::base64;
        let reference = base64::decode(vector::FRAME_B64).unwrap();
        let stride = vector::UNIT_SIZE.div_ceil(16) * 16;
        assert_eq!(reference.len(), vector::UNIT_SIZE * (vector::K + vector::M));
        let mut buf = vec![0u8; stride * (vector::K + vector::M)];
        for i in 0..vector::K + vector::M {
            buf[i * stride..i * stride + vector::UNIT_SIZE]
                .copy_from_slice(&reference[i * vector::UNIT_SIZE..(i + 1) * vector::UNIT_SIZE]);
        }
        let reference_source = buf.clone();
        for &e in &vector::ERASURES {
            buf[e * stride..e * stride + vector::UNIT_SIZE].fill(0x42);
        }
        decode(
            &mut buf,
            vector::UNIT_SIZE,
            stride,
            vector::K,
            vector::M,
            &vector::ERASURES,
        )
        .unwrap();
        for i in 0..vector::K {
            assert_eq!(
                &buf[i * stride..i * stride + vector::UNIT_SIZE],
                &reference_source[i * stride..i * stride + vector::UNIT_SIZE],
                "unit {i} mismatch"
            );
        }
    }
}
