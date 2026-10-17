// SPDX-License-Identifier: AGPL-3.0-only
//! Small randomness helpers. All protocol randomness comes from the OS CSPRNG.

use rand::RngCore;

pub fn fill(buf: &mut [u8]) {
    rand::rngs::OsRng.fill_bytes(buf);
}

pub fn bytes(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    fill(&mut v);
    v
}

pub fn array16() -> [u8; 16] {
    let mut v = [0u8; 16];
    fill(&mut v);
    v
}

pub fn array32() -> [u8; 32] {
    let mut v = [0u8; 32];
    fill(&mut v);
    v
}

pub fn u32() -> u32 {
    let mut v = [0u8; 4];
    fill(&mut v);
    u32::from_be_bytes(v)
}
