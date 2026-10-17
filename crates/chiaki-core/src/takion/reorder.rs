// SPDX-License-Identifier: AGPL-3.0-only
//! Fixed-size reorder queue for Takion DATA chunks (RFC 1982 sequence space).

use crate::takion::{seq32_gt, seq32_lt};

#[derive(Debug, Clone)]
pub struct ReorderQueue<T> {
    size_exp: u8,
    begin: u32,
    count: usize,
    entries: Vec<Option<T>>,
}

impl<T> ReorderQueue<T> {
    pub fn new(size_exp: u8, seq_num_start: u32) -> Self {
        let size = 1usize << size_exp;
        Self {
            size_exp,
            begin: seq_num_start,
            count: 0,
            entries: (0..size).map(|_| None).collect(),
        }
    }

    fn size(&self) -> usize {
        1usize << self.size_exp
    }

    fn idx(&self, seq: u32) -> usize {
        (seq as usize) & (self.size() - 1)
    }

    fn add(&self, a: u32, b: u32) -> u32 {
        a.wrapping_add(b)
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Push an entry. Returns any entry dropped to make room (or the pushed
    /// entry itself when it falls outside the window).
    pub fn push(&mut self, seq_num: u32, value: T) -> Option<T> {
        let end = self.add(self.begin, self.count as u32);
        let ge_begin = seq_num == self.begin || seq32_gt(seq_num, self.begin);
        let lt_end = seq32_lt(seq_num, end);
        if ge_begin && lt_end {
            let idx = self.idx(seq_num);
            if self.entries[idx].is_some() {
                return Some(value); // duplicate
            }
            // The window already counts this (previously missing) slot.
            self.entries[idx] = Some(value);
            return None;
        }
        if seq32_lt(seq_num, self.begin) {
            return Some(value); // old
        }

        // seq >= end: grow the window, dropping nothing (DROP_STRATEGY_END).
        let free = self.size() - self.count;
        let total_end = self.add(end, free as u32);
        let new_end = self.add(seq_num, 1);
        if seq32_lt(total_end, new_end) {
            return Some(value); // no room
        }
        let mut end = end;
        while seq32_lt(end, new_end) {
            self.count += 1;
            let idx = self.idx(end);
            self.entries[idx] = None;
            end = self.add(self.begin, self.count as u32);
        }
        let idx = self.idx(seq_num);
        self.entries[idx] = Some(value);
        None
    }

    pub fn pull(&mut self) -> Option<(u32, T)> {
        if self.count == 0 {
            return None;
        }
        let idx = self.idx(self.begin);
        let value = self.entries[idx].take()?;
        let seq = self.begin;
        self.begin = self.add(self.begin, 1);
        self.count -= 1;
        Some((seq, value))
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_and_gap() {
        let mut q = ReorderQueue::new(4, 100);
        assert!(q.push(102, 102).is_none());
        assert_eq!(q.count(), 3);
        assert!(q.pull().is_none());
        assert!(q.push(100, 100).is_none());
        assert_eq!(q.count(), 3);
        assert_eq!(q.pull(), Some((100, 100)));
        assert!(q.pull().is_none());
        assert!(q.push(101, 101).is_none());
        assert_eq!(q.count(), 2);
        assert_eq!(q.pull(), Some((101, 101)));
        assert_eq!(q.pull(), Some((102, 102)));
        assert_eq!(q.count(), 0);
    }

    #[test]
    fn duplicate_dropped() {
        let mut q = ReorderQueue::new(4, 0);
        assert!(q.push(0, 1).is_none());
        assert_eq!(q.push(0, 2), Some(2));
        assert_eq!(q.pull(), Some((0, 1)));
    }

    #[test]
    fn wraps() {
        let mut q = ReorderQueue::new(4, 0xffff_fffe);
        assert!(q.push(0xffff_fffe, 1).is_none());
        assert!(q.push(0xffff_ffff, 2).is_none());
        assert!(q.push(0, 3).is_none());
        assert_eq!(q.pull().map(|x| x.0), Some(0xffff_fffe));
        assert_eq!(q.pull().map(|x| x.0), Some(0xffff_ffff));
        assert_eq!(q.pull().map(|x| x.0), Some(0));
    }
}
