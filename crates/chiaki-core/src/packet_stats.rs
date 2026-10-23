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
    seq_initialized: bool,
    /// Cumulative counters for the UI; `get(true)` only resets the window
    /// counters used by congestion control.
    total_received: u64,
    total_lost: u64,
}

#[derive(Debug, Default)]
pub struct PacketStats {
    inner: Mutex<Inner>,
}

impl PacketStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_generation(&self, received: u64, lost: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.gen_received = inner.gen_received.saturating_add(received);
        inner.gen_lost = inner.gen_lost.saturating_add(lost);
    }

    pub fn push_seq(&self, seq_num: u16) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !inner.seq_initialized {
            // The first packet has no predecessor to compare against; make it
            // the first interval instead of treating the sequence from zero as
            // a huge loss.
            inner.seq_initialized = true;
            inner.seq_min = seq_num.wrapping_sub(1);
            inner.seq_max = seq_num;
        } else if crate::takion::seq16_gt(seq_num, inner.seq_max) {
            inner.seq_max = seq_num;
        }
        inner.seq_received += 1;
    }

    fn window(inner: &Inner) -> (u64, u64) {
        let seq_diff = inner.seq_max.wrapping_sub(inner.seq_min) as u64;
        let seq_lost = if inner.seq_received > seq_diff {
            seq_diff
        } else {
            seq_diff - inner.seq_received
        };
        (
            inner.gen_received + inner.seq_received,
            inner.gen_lost + seq_lost,
        )
    }

    /// Read (received, lost) for the current congestion window and optionally
    /// reset it. Resetting folds the window into the cumulative totals.
    pub fn get(&self, reset: bool) -> (u64, u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let (received, lost) = Self::window(&inner);
        if reset {
            inner.total_received = inner.total_received.saturating_add(received);
            inner.total_lost = inner.total_lost.saturating_add(lost);
            inner.gen_received = 0;
            inner.gen_lost = 0;
            inner.seq_min = inner.seq_max;
            inner.seq_received = 0;
        }
        (received, lost)
    }

    /// Cumulative counters for display, including the current window.
    pub fn totals(&self) -> (u64, u64) {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let (received, lost) = Self::window(&inner);
        (
            inner.total_received.saturating_add(received),
            inner.total_lost.saturating_add(lost),
        )
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

    #[test]
    fn totals_survive_congestion_resets() {
        let stats = PacketStats::new();
        stats.push_generation(10, 2);
        assert_eq!(stats.get(true), (10, 2));
        stats.push_generation(5, 1);
        assert_eq!(stats.totals(), (15, 3));
        assert_eq!(stats.get(true), (5, 1));
        assert_eq!(stats.totals(), (15, 3));
    }

    #[test]
    fn sequence_totals_survive_congestion_resets() {
        let stats = PacketStats::new();
        stats.get(true);
        for seq in 0..=4 {
            stats.push_seq(seq);
        }
        assert_eq!(stats.get(true), (5, 0));
        assert_eq!(stats.totals(), (5, 0));
        for seq in 5..=9 {
            stats.push_seq(seq);
        }
        assert_eq!(stats.totals(), (10, 0));
    }
}
