// SPDX-License-Identifier: AGPL-3.0-only
//! Packet statistics used by congestion control.

use std::sync::Mutex;

#[derive(Debug, Default)]
struct Inner {
    gen_received: u64,
    gen_lost: u64,
    seq_min: u16,
    seq_max: u16,
    seq_received: u64,
}

#[derive(Debug, Default)]
pub struct PacketStats {
    inner: Mutex<Inner>,
}

pub const PACKET_STATS_INTERVAL_DEFAULT: u64 = 200;

impl PacketStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_generation(&self, received: u64, lost: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.gen_received = inner.gen_received.saturating_add(received);
        inner.gen_lost = inner.gen_lost.saturating_add(lost);
    }

    pub fn push_seq(&self, seq_num: u16) {
        let mut inner = self.inner.lock().unwrap();
        inner.seq_received += 1;
        if crate::takion::seq16_gt(seq_num, inner.seq_max) {
            inner.seq_max = seq_num;
        }
    }

    /// Read (received, lost) and optionally reset the counters.
    pub fn get(&self, reset: bool) -> (u64, u64) {
        let mut inner = self.inner.lock().unwrap();
        let mut received = inner.gen_received;
        let mut lost = inner.gen_lost;

        let seq_diff = inner.seq_max.wrapping_sub(inner.seq_min) as u64;
        let seq_lost = if inner.seq_received > seq_diff {
            seq_diff
        } else {
            seq_diff - inner.seq_received
        };
        received += inner.seq_received;
        lost += seq_lost;

        if reset {
            inner.gen_received = 0;
            inner.gen_lost = 0;
            inner.seq_min = inner.seq_max;
            inner.seq_received = 0;
        }
        (received, lost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_counts() {
        let stats = PacketStats::new();
        stats.push_generation(10, 2);
        stats.push_generation(5, 1);
        assert_eq!(stats.get(false), (15, 3));
        assert_eq!(stats.get(true), (15, 3));
        assert_eq!(stats.get(false), (0, 0));
    }

    #[test]
    fn sequence_counts() {
        let stats = PacketStats::new();
        // The first interval includes the range from zero; reset it.
        for seq in 100..=104 {
            stats.push_seq(seq);
        }
        stats.get(true);
        for seq in 105..=109 {
            stats.push_seq(seq);
        }
        let (received, lost) = stats.get(false);
        assert_eq!(received, 5);
        assert_eq!(lost, 0);
    }
}
