// SPDX-License-Identifier: AGPL-3.0-only
//! Audio packet handling. Each source unit is one Opus packet.

use std::time::Duration;

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::packet_stats::PacketStats;
use crate::takion::packet::AvPacket;
use crate::takion::seq16_gt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub channels: u8,
    pub bits: u8,
    pub sample_rate: u32,
    pub frame_size: u32,
}

impl AudioFormat {
    pub fn parse(header: &[u8]) -> Result<Self> {
        if header.len() != 0xe {
            return Err(Error::InvalidData);
        }
        Ok(Self {
            channels: header[0],
            bits: header[1],
            sample_rate: u32::from_be_bytes([header[2], header[3], header[4], header[5]]),
            frame_size: u32::from_be_bytes([header[6], header[7], header[8], header[9]]),
        })
    }

    pub fn frame_duration(&self) -> Duration {
        if self.sample_rate == 0 || self.frame_size == 0 {
            return Duration::from_millis(10);
        }
        Duration::from_secs_f64(self.frame_size as f64 / self.sample_rate as f64)
    }
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub data: Bytes,
    pub frame_index: u16,
    pub duration: Duration,
    pub is_haptics: bool,
}

#[derive(Debug)]
pub struct AudioReceiver {
    format: AudioFormat,
    frame_index_prev: u16,
    startup: bool,
}

impl AudioReceiver {
    pub fn new(format: AudioFormat) -> Self {
        Self {
            format,
            frame_index_prev: 0,
            startup: true,
        }
    }

    pub fn push(
        &mut self,
        packet: &AvPacket,
        data: &Bytes,
        stats: Option<&PacketStats>,
    ) -> Vec<AudioFrame> {
        if packet.codec != 5 {
            return Vec::new();
        }
        let source_count = packet.audio_source_units_count() as usize;
        let fec_count = packet.audio_fec_units_count() as usize;
        let unit_size = packet.audio_unit_size() as usize;
        if data.is_empty() || unit_size == 0 {
            return Vec::new();
        }
        if (fec_count + source_count) != packet.units_in_frame_total as usize {
            return Vec::new();
        }
        if data.len() != unit_size * packet.units_in_frame_total as usize {
            return Vec::new();
        }
        if packet.frame_index > (1 << 15) {
            self.startup = false;
        }
        let duration = self.format.frame_duration();
        let mut frames = Vec::with_capacity(source_count + fec_count);
        for i in 0..source_count + fec_count {
            let frame_index = if i < source_count {
                packet.frame_index.wrapping_add(i as u16)
            } else {
                let fec_index = i - source_count;
                if self.startup && (packet.frame_index as usize + fec_index) < fec_count + 1 {
                    continue;
                }
                packet
                    .frame_index
                    .wrapping_sub(fec_count as u16)
                    .wrapping_add(fec_index as u16)
            };
            if !seq16_gt(frame_index, self.frame_index_prev) {
                continue;
            }
            self.frame_index_prev = frame_index;
            let off = i * unit_size;
            frames.push(AudioFrame {
                // Slicing the shared packet buffer avoids one copy per Opus
                // packet; the underlying bytes live as long as any frame.
                data: data.slice(off..off + unit_size),
                frame_index,
                duration,
                is_haptics: packet.is_haptics,
            });
        }
        if let Some(stats) = stats {
            stats.push_seq(packet.frame_index);
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(frame_index: u16, source: u8, fec: u8, unit: u8) -> AvPacket {
        AvPacket {
            packet_index: 0,
            frame_index,
            uses_nalu_info_structs: false,
            is_video: false,
            is_haptics: false,
            unit_index: 0,
            units_in_frame_total: (source + fec) as u16,
            units_in_frame_fec: ((unit as u16) << 8) | ((fec as u16) << 4) | source as u16,
            codec: 5,
            word_at_0x18: 0,
            adaptive_stream_index: 0,
            byte_at_0x2c: 0,
            key_pos: 0,
            data_offset: 0,
            data_len: 0,
        }
    }

    #[test]
    fn splits_units() {
        let format = AudioFormat {
            channels: 2,
            bits: 16,
            sample_rate: 48000,
            frame_size: 480,
        };
        let mut r = AudioReceiver::new(format);
        let p = packet(100, 2, 1, 4);
        let data = Bytes::from_static(&[1u8, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);
        let frames = r.push(&p, &data, None);
        // Source units 100,101; FEC unit decodes to 99 (startup skipped).
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].frame_index, 100);
        assert_eq!(frames[1].frame_index, 101);
        assert_eq!(frames[0].data.as_ref(), &[1, 1, 1, 1]);
    }

    #[test]
    fn deduplicates_old_frames() {
        let format = AudioFormat {
            channels: 2,
            bits: 16,
            sample_rate: 48000,
            frame_size: 480,
        };
        let mut r = AudioReceiver::new(format);
        let data = Bytes::from_static(&[0u8; 8]);
        let p = packet(10, 2, 0, 4);
        let _ = r.push(&p, &data, None);
        let frames = r.push(&p, &data, None);
        assert!(frames.is_empty());
    }
}
