// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Shared fixtures for unit tests.

use crate::takion::packet::AvPacket;

/// A video AV packet with the fields frame assembly cares about.
pub(crate) fn av_packet(frame: u16, unit: u16, total: u16, fec: u16, data_len: usize) -> AvPacket {
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
