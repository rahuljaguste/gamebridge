// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Small randomness helpers. All protocol randomness comes from the OS CSPRNG.

use rand::RngCore;

pub fn fill(buf: &mut [u8]) {
    rand::rngs::OsRng.fill_bytes(buf);
}

pub fn u32() -> u32 {
    let mut v = [0u8; 4];
    fill(&mut v);
    u32::from_be_bytes(v)
}
