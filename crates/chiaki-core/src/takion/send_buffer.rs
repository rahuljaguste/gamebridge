// SPDX-License-Identifier: AGPL-3.0-only
//! Retransmission buffer for sent Takion control DATA packets.

use std::time::{Duration, Instant};

use crate::takion::seq32_lt;

pub const RESEND_TIMEOUT: Duration = Duration::from_millis(200);
/// Drop a control packet after this many retransmissions. At the default
/// 200 ms timeout this is roughly five seconds. A lost ACK does not mean the
/// console is gone: media may still be flowing, so the Takion driver keeps
/// running and uses an inbound-silence watchdog to detect a dead peer.
pub const RESEND_TRIES_MAX: u64 = 25;

#[derive(Debug)]
struct Entry {
    seq_num: u32,
    buf: Vec<u8>,
    last_sent: Instant,
    tries: u64,
}

#[derive(Debug)]
pub struct SendBuffer {
    capacity: usize,
    entries: Vec<Entry>,
}

impl SendBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Store a sent packet. Returns the oldest packet if the buffer overflowed.
    pub fn push(&mut self, seq_num: u32, buf: Vec<u8>, now: Instant) -> Option<Vec<u8>> {
        let dropped = if self.entries.len() >= self.capacity {
            Some(self.entries.remove(0).buf)
        } else {
            None
        };
        self.entries.push(Entry {
            seq_num,
            buf,
            last_sent: now,
            tries: 0,
        });
        dropped
    }

    /// Remove packets acknowledged by a cumulative ACK.
    pub fn ack(&mut self, seq_num: u32) {
        self.entries
            .retain(|e| !(e.seq_num == seq_num || seq32_lt(e.seq_num, seq_num)));
    }

    /// Mark all due packets as retransmitted and return their bytes. The
    /// second value is true when at least one packet exceeded
    /// `RESEND_TRIES_MAX` and was dropped. Other pending packets are kept.
    pub fn take_due(&mut self, now: Instant) -> (Vec<Vec<u8>>, bool) {
        let mut out = Vec::new();
        let mut gave_up = false;
        for entry in &mut self.entries {
            if now.saturating_duration_since(entry.last_sent) > RESEND_TIMEOUT {
                if entry.tries >= RESEND_TRIES_MAX {
                    gave_up = true;
                    continue;
                }
                entry.last_sent = now;
                entry.tries += 1;
                out.push(entry.buf.clone());
            }
        }
        if gave_up {
            self.entries.retain(|entry| entry.tries < RESEND_TRIES_MAX);
        }
        (out, gave_up)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_cumulative() {
        let now = Instant::now();
        let mut b = SendBuffer::new(4);
        b.push(10, vec![10], now);
        b.push(11, vec![11], now);
        b.push(12, vec![12], now);
        b.ack(11);
        assert_eq!(b.len(), 1);
        b.ack(12);
        assert!(b.is_empty());
    }

    #[test]
    fn overflow_drops_oldest() {
        let now = Instant::now();
        let mut b = SendBuffer::new(2);
        assert!(b.push(1, vec![1], now).is_none());
        assert!(b.push(2, vec![2], now).is_none());
        assert_eq!(b.push(3, vec![3], now), Some(vec![1]));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn resend_after_timeout() {
        let now = Instant::now();
        let mut b = SendBuffer::new(2);
        b.push(1, vec![1], now);
        assert!(b.take_due(now).0.is_empty());
        assert_eq!(
            b.take_due(now + Duration::from_millis(300)).0,
            vec![vec![1]]
        );
    }

    #[test]
    fn unacked_packets_are_dropped_without_dropping_younger_ones() {
        let now = Instant::now();
        let mut b = SendBuffer::new(2);
        b.push(1, vec![1], now);
        let mut at = now;
        for _ in 0..RESEND_TRIES_MAX {
            at += Duration::from_millis(300);
            let (due, gave_up) = b.take_due(at);
            assert!(!gave_up);
            assert_eq!(due, vec![vec![1]]);
        }
        // A younger packet, queued just before the old one exhausts.
        b.push(2, vec![2], at);
        at += Duration::from_millis(300);
        let (due, gave_up) = b.take_due(at);
        assert_eq!(due, vec![vec![2]]);
        assert!(gave_up);
        assert_eq!(b.len(), 1, "the younger packet must stay queued");
    }
}
