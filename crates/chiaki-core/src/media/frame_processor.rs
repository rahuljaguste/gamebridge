// SPDX-License-Identifier: AGPL-3.0-only
//! Video frame assembly with Cauchy FEC, ported from `frameprocessor.c`.

use crate::error::{Error, Result};
use crate::media::fec;
use crate::packet_stats::PacketStats;
use crate::takion::packet::AvPacket;

pub const UNIT_SLOTS_MAX: usize = 256;
pub const VIDEO_BUFFER_PADDING: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushResult {
    Success,
    FecSuccess,
    FecFailed,
    Failed,
}

#[derive(Debug, Default)]
pub struct StreamStats {
    pub frames: u64,
    pub bytes: u64,
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
    pub stream_stats: StreamStats,
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
        self.flushed = false;
        self.units_source_expected =
            (packet.units_in_frame_total - packet.units_in_frame_fec) as usize;
        self.units_fec_expected = packet.units_in_frame_fec.max(1) as usize;
        self.buf_size_per_unit = packet.data_len;
        if packet.is_video && (packet.unit_index as usize) < self.units_source_expected {
            if data.len() < 2 {
                return Err(Error::BufferTooSmall);
            }
            let padding = u16::from_be_bytes([data[0], data[1]]) as usize;
            self.buf_size_per_unit += padding;
        }
        self.stride = self.buf_size_per_unit.div_ceil(16) * 16;
        if self.buf_size_per_unit == 0 {
            return Err(Error::BufferTooSmall);
        }
        self.units_source_received = 0;
        self.units_fec_received = 0;
        let slots = self.units_source_expected + self.units_fec_expected;
        if slots > UNIT_SLOTS_MAX {
            return Err(Error::InvalidData);
        }
        if slots != self.unit_slots.len() {
            self.unit_slots = vec![0; slots];
        } else {
            self.unit_slots.fill(0);
        }
        let required = self.stride * slots + VIDEO_BUFFER_PADDING;
        if self.frame_buf.len() < required {
            self.frame_buf = vec![0; required];
        } else {
            self.frame_buf[..required].fill(0);
        }
        Ok(())
    }

    pub fn put_unit(&mut self, packet: &AvPacket, data: &[u8]) -> Result<()> {
        let idx = packet.unit_index as usize;
        if idx >= self.unit_slots.len() {
            return Err(Error::InvalidData);
        }
        if data.is_empty() {
            return Err(Error::InvalidData);
        }
        if data.len() > self.buf_size_per_unit {
            return Err(Error::InvalidData);
        }
        if self.unit_slots[idx] != 0 {
            return Err(Error::InvalidData);
        }
        self.unit_slots[idx] = data.len();
        if !self.flushed {
            let off = idx * self.stride;
            self.frame_buf[off..off + data.len()].copy_from_slice(data);
        }
        if idx < self.units_source_expected {
            self.units_source_received += 1;
        } else {
            self.units_fec_received += 1;
        }
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
        self.stream_stats.frames += 1;
        self.stream_stats.bytes += out.len() as u64;
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

    fn packet(frame: u16, unit: u16, total: u16, fec: u16, data_len: usize) -> AvPacket {
        AvPacket {
            packet_index: 0,
            frame_index: frame,
            uses_nalu_info_structs: false,
            is_video: true,
            is_haptics: false,
            unit_index: unit,
            units_in_frame_total: total,
            units_in_frame_fec: fec,
            codec: 3,
            word_at_0x18: 0,
            adaptive_stream_index: 0,
            byte_at_0x2c: 0,
            key_pos: 0,
            data_offset: 0,
            data_len,
        }
    }

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
}
