// SPDX-License-Identifier: AGPL-3.0-only
//! Video frame assembly with Cauchy FEC, ported from `frameprocessor.c`.

use crate::error::{Error, Result};
use crate::media::fec;
use crate::packet_stats::PacketStats;
use crate::takion::packet::AvPacket;

pub const UNIT_SLOTS_MAX: usize = 256;
pub const VIDEO_BUFFER_PADDING: usize = 64;
/// Hard ceiling for one assembled frame. A legitimate console frame is far
/// smaller (256 MTU-sized units is well under 1 MiB); this stops a
/// GMAC-authenticated but hostile packet from pinning tens of megabytes with a
/// forged padding field.
pub const MAX_FRAME_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Bytes needed for `slots` units of `stride` bytes plus padding, capped by
/// the hostile-packet ceiling.
fn frame_buffer_size(stride: usize, slots: usize) -> Result<usize> {
    stride
        .checked_mul(slots)
        .and_then(|n| n.checked_add(VIDEO_BUFFER_PADDING))
        .filter(|n| *n <= MAX_FRAME_BUFFER_BYTES)
        .ok_or(Error::InvalidData)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushResult {
    Success,
    FecSuccess,
    FecFailed,
    Failed,
}

#[derive(Debug, Default)]
pub struct FrameProcessor {
    frame_buf: Vec<u8>,
    buf_size_per_unit: usize,
    stride: usize,
    units_source_expected: usize,
    units_fec_expected: usize,
    units_source_received: usize,
    units_fec_received: usize,
    unit_slots: Vec<usize>,
    flushed: bool,
}

impl FrameProcessor {
    pub fn new() -> Self {
        Self {
            flushed: true,
            ..Default::default()
        }
    }

    pub fn alloc_frame(&mut self, packet: &AvPacket, data: &[u8]) -> Result<()> {
        if packet.units_in_frame_total < packet.units_in_frame_fec {
            return Err(Error::InvalidData);
        }
        // Compute and validate everything into locals first. A malformed
        // packet must not leave stale geometry behind: subsequent packets for
        // the same frame would otherwise be assembled against the bad header.
        let units_source_expected =
            (packet.units_in_frame_total - packet.units_in_frame_fec) as usize;
        let units_fec_expected = packet.units_in_frame_fec.max(1) as usize;
        let mut buf_size_per_unit = packet.data_len;
        if packet.is_video && (packet.unit_index as usize) < units_source_expected {
            if data.len() < 2 {
                return Err(Error::BufferTooSmall);
            }
            let padding = u16::from_be_bytes([data[0], data[1]]) as usize;
            buf_size_per_unit = buf_size_per_unit
                .checked_add(padding)
                .ok_or(Error::InvalidData)?;
        }
        if buf_size_per_unit == 0 {
            return Err(Error::BufferTooSmall);
        }
        let stride = buf_size_per_unit.div_ceil(16) * 16;
        let slots = units_source_expected
            .checked_add(units_fec_expected)
            .ok_or(Error::InvalidData)?;
        if slots > UNIT_SLOTS_MAX {
            return Err(Error::InvalidData);
        }
        let required = frame_buffer_size(stride, slots)?;

        let mut unit_slots = if slots != self.unit_slots.len() {
            vec![0; slots]
        } else {
            std::mem::take(&mut self.unit_slots)
        };
        unit_slots.fill(0);
        let mut frame_buf = std::mem::take(&mut self.frame_buf);
        if frame_buf.len() < required {
            frame_buf.resize(required, 0);
        } else {
            frame_buf[..required].fill(0);
        }

        // Commit only after all fallible work is complete.
        self.frame_buf = frame_buf;
        self.unit_slots = unit_slots;
        self.units_source_expected = units_source_expected;
        self.units_fec_expected = units_fec_expected;
        self.buf_size_per_unit = buf_size_per_unit;
        self.stride = stride;
        self.units_source_received = 0;
        self.units_fec_received = 0;
        self.flushed = false;
        Ok(())
    }

    pub fn put_unit(&mut self, packet: &AvPacket, data: &[u8]) -> Result<()> {
        if self.flushed {
            return Ok(());
        }
        let idx = packet.unit_index as usize;
        if idx >= self.unit_slots.len() {
            return Err(Error::InvalidData);
        }
        if data.is_empty() {
            return Err(Error::InvalidData);
        }
        if self.unit_slots[idx] != 0 {
            return Err(Error::InvalidData);
        }
        // The first packet of a frame is normally unit 0 and carries the
        // padding extension that sets the symbol size. UDP reordering can
        // deliver a different unit first; grow the frame buffer if a later
        // (for example FEC) unit is larger instead of rejecting every such
        // unit for the rest of the frame.
        if data.len() > self.buf_size_per_unit {
            self.grow_unit_size(data.len())?;
        }
        self.unit_slots[idx] = data.len();
        let off = idx * self.stride;
        self.frame_buf[off..off + data.len()].copy_from_slice(data);
        if idx < self.units_source_expected {
            self.units_source_received += 1;
        } else {
            self.units_fec_received += 1;
        }
        Ok(())
    }

    /// True once `flush` has consumed the current frame. Late units are
    /// expected (the frame may flush as soon as all source units arrive) and
    /// should be ignored by the ingest loop.
    pub fn is_flushed(&self) -> bool {
        self.flushed
    }

    /// Record a unit that arrived after the frame flushed. It must not touch
    /// frame data, but it still counts as received for packet statistics and
    /// congestion feedback. Without this, every trailing FEC unit looks lost
    /// even when the console delivered it.
    pub fn note_late_unit(&mut self, packet: &AvPacket) {
        if !self.flushed {
            return;
        }
        let idx = packet.unit_index as usize;
        if idx >= self.unit_slots.len() || self.unit_slots[idx] != 0 {
            return;
        }
        // The frame is already flushed, so a non-zero marker is enough; it
        // only prevents counting a duplicate retransmission twice.
        self.unit_slots[idx] = 1;
        if idx < self.units_source_expected {
            self.units_source_received += 1;
        } else {
            self.units_fec_received += 1;
        }
    }

    fn grow_unit_size(&mut self, size: usize) -> Result<()> {
        let stride = size.div_ceil(16) * 16;
        let slots = self.unit_slots.len();
        let required = frame_buffer_size(stride, slots)?;
        let mut frame_buf = vec![0u8; required];
        for (i, &len) in self.unit_slots.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let old_off = i * self.stride;
            let new_off = i * stride;
            frame_buf[new_off..new_off + len]
                .copy_from_slice(&self.frame_buf[old_off..old_off + len]);
        }
        self.frame_buf = frame_buf;
        self.buf_size_per_unit = size;
        self.stride = stride;
        Ok(())
    }

    pub fn flush_possible(&self) -> bool {
        self.units_source_received + self.units_fec_received >= self.units_source_expected
    }

    pub fn report_packet_stats(&self, stats: &PacketStats) {
        let received = (self.units_source_received + self.units_fec_received) as u64;
        let expected = (self.units_source_expected + self.units_fec_expected) as u64;
        stats.push_generation(received, expected.saturating_sub(received));
    }

    /// Flush the current frame. On `FecFailed`, no frame bytes are returned
    /// (the caller should request a keyframe).
    pub fn flush(&mut self) -> (FlushResult, Vec<u8>) {
        if self.units_source_expected == 0 || self.flushed {
            return (FlushResult::Failed, Vec::new());
        }
        let mut result = FlushResult::Success;
        if self.units_source_received < self.units_source_expected {
            match self.fec_decode() {
                Ok(()) => result = FlushResult::FecSuccess,
                Err(_) => result = FlushResult::FecFailed,
            }
        }
        self.flushed = true;
        if result == FlushResult::FecFailed {
            return (result, Vec::new());
        }

        let mut out = Vec::with_capacity(self.buf_size_per_unit * self.units_source_expected);
        for i in 0..self.units_source_expected {
            let data_size = self.unit_slots[i];
            if data_size == 0 {
                continue;
            }
            if data_size < 2 {
                continue;
            }
            let off = i * self.stride;
            out.extend_from_slice(&self.frame_buf[off + 2..off + data_size]);
        }
        (result, out)
    }

    fn fec_decode(&mut self) -> Result<()> {
        let total = self.units_source_expected + self.units_fec_expected;
        let mut erasures = Vec::new();
        for (i, size) in self.unit_slots.iter().enumerate().take(total) {
            if *size == 0 {
                erasures.push(i);
            }
        }
        fec::decode(
            &mut self.frame_buf,
            self.buf_size_per_unit,
            self.stride,
            self.units_source_expected,
            self.units_fec_expected,
            &erasures,
        )?;
        // Recovered units carry a 2-byte padding count at the front.
        for i in 0..self.units_source_expected {
            if self.unit_slots[i] != 0 {
                continue;
            }
            let off = i * self.stride;
            let padding =
                u16::from_be_bytes([self.frame_buf[off], self.frame_buf[off + 1]]) as usize;
            if padding >= self.buf_size_per_unit {
                continue;
            }
            self.unit_slots[i] = self.buf_size_per_unit - padding;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::av_packet as packet;

    #[test]
    fn assembles_without_fec() {
        let mut fp = FrameProcessor::new();
        let unit0 = [0u8, 3u8, 0xaa, 0xbb, 0xcc];
        let unit1 = [0u8, 0u8, 0xdd];
        let p0 = packet(0, 0, 3, 1, unit0.len());
        let p1 = packet(0, 1, 3, 1, unit1.len());
        fp.alloc_frame(&p0, &unit0).unwrap();
        fp.put_unit(&p0, &unit0).unwrap();
        // Total 3 with 1 FEC => 2 source units.
        fp.put_unit(&p1, &unit1).unwrap();
        assert!(fp.flush_possible());
        let (result, frame) = fp.flush();
        assert_eq!(result, FlushResult::Success);
        assert_eq!(frame, vec![0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[test]
    fn malformed_allocation_does_not_poison_state() {
        let mut fp = FrameProcessor::new();
        // More FEC units than total units is rejected before any state changes.
        let bad = packet(0, 0, 1, 2, 4);
        assert!(fp.alloc_frame(&bad, &[0, 0, 0, 0]).is_err());

        let unit0 = [0u8, 3u8, 0xaa, 0xbb, 0xcc];
        let unit1 = [0u8, 0u8, 0xdd];
        let p0 = packet(0, 0, 3, 1, unit0.len());
        let p1 = packet(0, 1, 3, 1, unit1.len());
        fp.alloc_frame(&p0, &unit0).unwrap();
        fp.put_unit(&p0, &unit0).unwrap();
        fp.put_unit(&p1, &unit1).unwrap();
        let (result, frame) = fp.flush();
        assert_eq!(result, FlushResult::Success);
        assert_eq!(frame, vec![0xaa, 0xbb, 0xcc, 0xdd]);

        // A late unit for a flushed frame is expected and ignored.
        assert!(fp.is_flushed());
        assert!(fp.put_unit(&p1, &unit1).is_ok());
    }

    #[test]
    fn late_fec_units_count_as_received_for_packet_stats() {
        let mut fp = FrameProcessor::new();
        let unit0 = [0u8, 0u8, 0xaa, 0xbb];
        let unit1 = [0u8, 0u8, 0xcc];
        let p0 = packet(0, 0, 3, 1, unit0.len());
        let p1 = packet(0, 1, 3, 1, unit1.len());
        fp.alloc_frame(&p0, &unit0).unwrap();
        fp.put_unit(&p0, &unit0).unwrap();
        fp.put_unit(&p1, &unit1).unwrap();
        assert!(fp.flush_possible());
        let (result, _) = fp.flush();
        assert_eq!(result, FlushResult::Success);
        assert_eq!(fp.units_source_received, 2);
        assert_eq!(fp.units_fec_received, 0);

        // The trailing FEC unit arrives after the frame is complete.
        let late = packet(0, 2, 3, 1, 3);
        fp.note_late_unit(&late);
        assert_eq!(fp.units_fec_received, 1);
        // A retransmission of the same late unit is not counted twice.
        fp.note_late_unit(&late);
        assert_eq!(fp.units_fec_received, 1);

        let stats = PacketStats::new();
        fp.report_packet_stats(&stats);
        assert_eq!(stats.get(false), (3, 0));
    }

    #[test]
    fn rejects_a_padding_field_that_would_pin_a_huge_frame_buffer() {
        let mut fp = FrameProcessor::new();
        let mut data = vec![0u8; 65535];
        data[0] = 0xff;
        data[1] = 0xff;
        // 65 KiB payload + 65 KiB padding across 256 slots would otherwise
        // reserve over 33 MiB from one authenticated packet.
        let oversized = packet(0, 0, 256, 1, data.len());
        assert!(fp.alloc_frame(&oversized, &data).is_err());
    }

    #[test]
    fn grows_when_a_later_unit_is_larger_than_the_first() {
        let mut fp = FrameProcessor::new();
        // The first packet is a small FEC unit, so there is no padding
        // extension to size the source units.
        let fec = packet(0, 3, 4, 1, 10);
        fp.alloc_frame(&fec, &[0; 10]).unwrap();
        fp.put_unit(&fec, &[0; 10]).unwrap();

        let mut source = [0u8; 20];
        source[1] = 2; // two-byte padding count
        for unit in 0..3 {
            let p = packet(0, unit, 4, 1, source.len());
            fp.put_unit(&p, &source).unwrap();
        }
        assert!(fp.flush_possible());
        let (result, frame) = fp.flush();
        assert_eq!(result, FlushResult::Success);
        assert_eq!(frame.len(), 3 * (source.len() - 2));
    }
}
