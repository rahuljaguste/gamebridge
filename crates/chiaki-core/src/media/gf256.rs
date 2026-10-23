// SPDX-License-Identifier: AGPL-3.0-only
//! GF(2^8) arithmetic with the primitive polynomial 0x11d, matching
//! gf-complete/jerasure's `galois_single_multiply` for w=8.

use std::sync::OnceLock;

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct Gf256 {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Gf256 {
    fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for (i, e) in exp.iter_mut().take(255).enumerate() {
            *e = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11d;
            }
        }
        let (head, tail) = exp.split_at_mut(255);
        for (i, e) in tail.iter_mut().enumerate() {
            *e = head[i % 255];
        }
        Self { exp, log }
    }

    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            let sum = self.log[a as usize] as usize + self.log[b as usize] as usize;
            self.exp[sum]
        }
    }

    pub fn inv(&self, a: u8) -> Result<u8> {
        if a == 0 {
            return Err(Error::InvalidData);
        }
        Ok(self.exp[255 - self.log[a as usize] as usize])
    }

    /// dst ^= coeff * src
    pub fn mul_add(&self, src: &[u8], coeff: u8, dst: &mut [u8]) {
        match coeff {
            0 => {}
            1 => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d ^= *s;
                }
            }
            _ => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d ^= self.mul(*s, coeff);
                }
            }
        }
    }
}

pub fn gf() -> &'static Gf256 {
    static GF: OnceLock<Gf256> = OnceLock::new();
    GF.get_or_init(Gf256::new)
}

/// Invert a square matrix in place. Returns `Err` if singular.
pub fn invert(matrix: &mut [u8], n: usize) -> Result<Vec<u8>> {
    let gf = gf();
    let mut inv = vec![0u8; n * n];
    for i in 0..n {
        inv[i * n + i] = 1;
    }
    for col in 0..n {
        if matrix[col * n + col] == 0 {
            let mut swap = None;
            for row in col + 1..n {
                if matrix[row * n + col] != 0 {
                    swap = Some(row);
                    break;
                }
            }
            let Some(row) = swap else {
                return Err(Error::FecFailed);
            };
            for k in 0..n {
                matrix.swap(col * n + k, row * n + k);
                inv.swap(col * n + k, row * n + k);
            }
        }
        let pivot = matrix[col * n + col];
        if pivot != 1 {
            let p_inv = gf.inv(pivot)?;
            for k in 0..n {
                matrix[col * n + k] = gf.mul(matrix[col * n + k], p_inv);
                inv[col * n + k] = gf.mul(inv[col * n + k], p_inv);
            }
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = matrix[row * n + col];
            if factor == 0 {
                continue;
            }
            for k in 0..n {
                let a = gf.mul(factor, matrix[col * n + k]);
                matrix[row * n + k] ^= a;
                let b = gf.mul(factor, inv[col * n + k]);
                inv[row * n + k] ^= b;
            }
        }
    }
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_known() {
        let gf = gf();
        assert_eq!(gf.mul(0x01, 0x53), 0x53);
        assert_eq!(gf.mul(0x02, 0x80), 0x1d);
        let inv = gf.inv(0x53).unwrap();
        assert_eq!(gf.mul(0x53, inv), 1);
    }

    #[test]
    fn invert_round_trip() {
        let gf = gf();
        let mut m = vec![0x01, 0x02, 0x03, 0x04];
        let inv = invert(&mut m, 2).unwrap();
        // Multiply original by inverse and check identity.
        let orig = [0x01u8, 0x02, 0x03, 0x04];
        let mut prod = vec![0u8; 4];
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    prod[i * 2 + j] ^= gf.mul(orig[i * 2 + k], inv[k * 2 + j]);
                }
            }
        }
        assert_eq!(prod, vec![1, 0, 0, 1]);
    }
}
