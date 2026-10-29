// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! RFC 1982 sequence-number comparisons and the 32/64-bit Takion key state.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyState {
    prev: u64,
}

impl KeyState {
    pub fn new() -> Self {
        Self { prev: 0 }
    }

    /// Reconstruct the 64-bit key position from the 32-bit low word carried
    /// in a packet. Only commit after the packet is authenticated.
    pub fn request_pos(&mut self, low: u32, commit: bool) -> u64 {
        let prev_low = self.prev as u32;
        let mut high = (self.prev >> 32) as u32;
        if seq32_gt(low, prev_low) && low < prev_low {
            high = high.wrapping_add(1);
        } else if seq32_lt(low, prev_low) && low > prev_low && high > 0 {
            high -= 1;
        }
        let res = ((high as u64) << 32) | low as u64;
        if commit {
            self.prev = res;
        }
        res
    }

    pub fn commit(&mut self, pos: u64) {
        self.prev = pos;
    }
}

#[inline]
pub fn seq16_lt(a: u16, b: u16) -> bool {
    if a == b {
        return false;
    }
    let d = b as i32 - a as i32;
    (a < b && d < 1 << 15) || (a > b && -d > 1 << 15)
}

#[inline]
pub fn seq16_gt(a: u16, b: u16) -> bool {
    if a == b {
        return false;
    }
    let d = b as i32 - a as i32;
    (a < b && d > 1 << 15) || (a > b && -d < 1 << 15)
}

#[inline]
pub fn seq32_lt(a: u32, b: u32) -> bool {
    if a == b {
        return false;
    }
    let d = b as i64 - a as i64;
    (a < b && d < 1 << 31) || (a > b && -d > 1 << 31)
}

#[inline]
pub fn seq32_gt(a: u32, b: u32) -> bool {
    if a == b {
        return false;
    }
    let d = b as i64 - a as i64;
    (a < b && d > 1 << 31) || (a > b && -d < 1 << 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_state_commit_vector() {
        let mut s = KeyState::new();
        assert_eq!(s.request_pos(0, true), 0);
        assert_eq!(s.request_pos(0x1337, true), 0x1337);
        assert_eq!(s.request_pos(0xffff0000, true), 0xffff0000);
        assert_eq!(s.request_pos(0x1337, true), 0x1_0000_1337);
        assert_eq!(s.request_pos(0xffff1337, true), 0xffff1337);
        assert_eq!(s.request_pos(0x50000000, true), 0x1_5000_0000);
        assert_eq!(s.request_pos(0xb0000000, true), 0x1_b000_0000);
        assert_eq!(s.request_pos(0, true), 0x2_0000_0000);
    }

    #[test]
    fn key_state_no_commit_vector() {
        let mut s = KeyState::new();
        assert_eq!(s.request_pos(0, false), 0);
        assert_eq!(s.request_pos(0x1337, false), 0x1337);
        assert_eq!(s.request_pos(0xffff0000, false), 0xffff0000);
        assert_eq!(s.request_pos(0x1337, false), 0x1337);
        assert_eq!(s.request_pos(0xffff1337, false), 0xffff1337);
        assert_eq!(s.request_pos(0x50000000, false), 0x50000000);
        assert_eq!(s.request_pos(0xb0000000, false), 0xb0000000);
        assert_eq!(s.request_pos(0, false), 0);
    }

    #[test]
    fn seq16_edges() {
        assert!(seq16_gt(0x7fff, 0));
        assert!(!seq16_gt(0x8000, 0));
        assert!(!seq16_gt(0x8001, 0));
        assert!(seq16_lt(0, 0x7fff));
        assert!(!seq16_lt(0, 0x8000));
        assert!(seq16_gt(0, 0xffff));
        assert!(seq16_lt(0xffff, 0));
    }

    #[test]
    fn seq32_edges() {
        assert!(seq32_gt(0x7fff_ffff, 0));
        assert!(!seq32_gt(0x8000_0000, 0));
        assert!(!seq32_gt(0x8000_0001, 0));
        assert!(seq32_lt(0, 0x7fff_ffff));
        assert!(!seq32_lt(0, 0x8000_0000));
        assert!(seq32_gt(0, 0xffff_ffff));
        assert!(seq32_lt(0xffff_ffff, 0));
    }
}
