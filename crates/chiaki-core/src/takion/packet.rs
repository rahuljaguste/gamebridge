// SPDX-License-Identifier: AGPL-3.0-only
//! Takion packet formats and AV packet parsing.
//!
//! Layouts and offsets mirror `lib/src/takion.c` exactly.

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::takion::KeyState;

pub const PACKET_TYPE_CONTROL: u8 = 0;
pub const PACKET_TYPE_FEEDBACK_HISTORY: u8 = 1;
pub const PACKET_TYPE_VIDEO: u8 = 2;
pub const PACKET_TYPE_AUDIO: u8 = 3;
pub const PACKET_TYPE_HANDSHAKE: u8 = 4;
pub const PACKET_TYPE_CONGESTION: u8 = 5;
pub const PACKET_TYPE_FEEDBACK_STATE: u8 = 6;
pub const PACKET_TYPE_CLIENT_INFO: u8 = 8;
pub const PACKET_TYPE_PAD_INFO_EVENT: u8 = 9;
pub const PACKET_TYPE_PAD_ADAPTIVE_TRIGGERS: u8 = 11;

pub const PACKET_BASE_TYPE_MASK: u8 = 0xf;
pub const MESSAGE_HEADER_SIZE: usize = 0x10;
pub const CONGESTION_PACKET_SIZE: usize = 0xf;

pub const CHUNK_TYPE_DATA: u8 = 0;
pub const CHUNK_TYPE_INIT: u8 = 1;
pub const CHUNK_TYPE_INIT_ACK: u8 = 2;
pub const CHUNK_TYPE_DATA_ACK: u8 = 3;
pub const CHUNK_TYPE_COOKIE: u8 = 0xa;
pub const CHUNK_TYPE_COOKIE_ACK: u8 = 0xb;

pub const A_RWND: u32 = 0x19000;
pub const OUTBOUND_STREAMS: u16 = 0x64;
pub const INBOUND_STREAMS: u16 = 0x64;
pub const COOKIE_SIZE: usize = 0x20;

pub fn base_type(buf: &[u8]) -> Result<u8> {
    buf.first()
        .map(|b| b & PACKET_BASE_TYPE_MASK)
        .ok_or(Error::BufferTooSmall)
}

/// Offset of the 4-byte GMAC for a packet type.
pub fn mac_offset(base: u8) -> Option<usize> {
    match base {
        PACKET_TYPE_CONTROL => Some(5),
        PACKET_TYPE_VIDEO | PACKET_TYPE_AUDIO => Some(0xa),
        PACKET_TYPE_CONGESTION => Some(7),
        _ => None,
    }
}

/// Offset of the 4-byte key position for a packet type.
pub fn key_pos_offset(base: u8) -> Option<usize> {
    match base {
        PACKET_TYPE_CONTROL => Some(9),
        PACKET_TYPE_VIDEO | PACKET_TYPE_AUDIO => Some(0xe),
        PACKET_TYPE_CONGESTION => Some(0xb),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MessageHeader {
    pub tag: u32,
    pub key_pos: u64,
    pub chunk_type: u8,
    pub chunk_flags: u8,
    pub payload_size: usize,
    pub payload_offset: usize,
}

/// Parse the message header that follows the base-type byte.
pub fn parse_message(
    buf: &[u8],
    tag_local: u32,
    key_state: &mut KeyState,
) -> Result<MessageHeader> {
    if buf.len() < MESSAGE_HEADER_SIZE {
        return Err(Error::InvalidData);
    }
    let tag = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if tag != tag_local {
        return Err(Error::InvalidData);
    }
    let key_pos_low = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let key_pos = key_state.request_pos(key_pos_low, true);
    let chunk_type = buf[12];
    let chunk_flags = buf[13];
    let payload_size_field = u16::from_be_bytes([buf[14], buf[15]]) as usize;
    if buf.len() != payload_size_field + 0xc {
        return Err(Error::InvalidData);
    }
    let payload_size = payload_size_field
        .checked_sub(4)
        .ok_or(Error::InvalidData)?;
    Ok(MessageHeader {
        tag,
        key_pos,
        chunk_type,
        chunk_flags,
        payload_size,
        payload_offset: MESSAGE_HEADER_SIZE,
    })
}

pub fn write_message_header(
    buf: &mut [u8],
    tag: u32,
    key_pos: u64,
    chunk_type: u8,
    chunk_flags: u8,
    payload_data_size: usize,
) {
    buf[0..4].copy_from_slice(&tag.to_be_bytes());
    buf[4..8].fill(0);
    buf[8..12].copy_from_slice(&(key_pos as u32).to_be_bytes());
    buf[12] = chunk_type;
    buf[13] = chunk_flags;
    buf[14..16].copy_from_slice(&((payload_data_size as u16).wrapping_add(4)).to_be_bytes());
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CongestionPacket {
    pub word_0: u16,
    pub received: u16,
    pub lost: u16,
}

pub fn format_congestion(
    buf: &mut [u8; CONGESTION_PACKET_SIZE],
    packet: CongestionPacket,
    key_pos: u64,
) {
    buf[0] = PACKET_TYPE_CONGESTION;
    buf[1..3].copy_from_slice(&packet.word_0.to_be_bytes());
    buf[3..5].copy_from_slice(&packet.received.to_be_bytes());
    buf[5..7].copy_from_slice(&packet.lost.to_be_bytes());
    buf[7..11].fill(0);
    buf[11..15].copy_from_slice(&(key_pos as u32).to_be_bytes());
}

#[derive(Debug, Clone)]
pub struct AvPacket {
    pub packet_index: u16,
    pub frame_index: u16,
    pub uses_nalu_info_structs: bool,
    pub is_video: bool,
    pub is_haptics: bool,
    pub unit_index: u16,
    pub units_in_frame_total: u16,
    pub units_in_frame_fec: u16,
    pub codec: u8,
    pub word_at_0x18: u16,
    pub adaptive_stream_index: u8,
    pub byte_at_0x2c: u8,
    pub key_pos: u64,
    pub data_offset: usize,
    pub data_len: usize,
}

impl AvPacket {
    /// Copy the decrypted payload out of the UDP buffer.
    pub fn data(&self, packet: &[u8]) -> Bytes {
        Bytes::copy_from_slice(&packet[self.data_offset..self.data_offset + self.data_len])
    }

    pub fn audio_unit_size(&self) -> u8 {
        (self.units_in_frame_fec >> 8) as u8
    }

    pub fn audio_source_units_count(&self) -> u8 {
        (self.units_in_frame_fec & 0xf) as u8
    }

    pub fn audio_fec_units_count(&self) -> u8 {
        ((self.units_in_frame_fec >> 4) & 0xf) as u8
    }
}

/// Parse a v9 or v12 AV packet. `key_state` is committed for this packet.
pub fn parse_av(packet: &[u8], v12: bool, key_state: &mut KeyState) -> Result<AvPacket> {
    if packet.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    let base = packet[0] & PACKET_BASE_TYPE_MASK;
    if base != PACKET_TYPE_VIDEO && base != PACKET_TYPE_AUDIO {
        return Err(Error::InvalidData);
    }
    let is_video = base == PACKET_TYPE_VIDEO;
    let uses_nalu_info_structs = (packet[0] >> 4) & 1 != 0;

    let av = &packet[1..];
    let header_size = if v12 {
        if is_video { 0x17 } else { 0x13 }
    } else if is_video {
        0x17
    } else {
        0x12
    };
    if av.len() < header_size + 1 {
        return Err(Error::BufferTooSmall);
    }

    let packet_index = u16::from_be_bytes([av[0], av[1]]);
    let frame_index = u16::from_be_bytes([av[2], av[3]]);
    let dword2 = u32::from_be_bytes([av[4], av[5], av[6], av[7]]);
    let (unit_index, units_in_frame_total, units_in_frame_fec) = if is_video {
        (
            ((dword2 >> 0x15) & 0x7ff) as u16,
            (((dword2 >> 0xa) & 0x7ff) + 1) as u16,
            (dword2 & 0x3ff) as u16,
        )
    } else {
        (
            ((dword2 >> 0x18) & 0xff) as u16,
            (((dword2 >> 0x10) & 0xff) + 1) as u16,
            (dword2 & 0xffff) as u16,
        )
    };
    let codec = av[8];
    let key_pos_low = u32::from_be_bytes([av[0xd], av[0xe], av[0xf], av[0x10]]);
    let key_pos = key_state.request_pos(key_pos_low, true);

    let mut cur = 0x11usize;
    if is_video {
        if av.len() < cur + 3 {
            return Err(Error::BufferTooSmall);
        }
        let word_at_0x18 = u16::from_be_bytes([av[cur], av[cur + 1]]);
        let adaptive_stream_index = av[cur + 2] >> 5;
        cur += 3;
        let byte_at_0x2c = *av.get(cur).ok_or(Error::BufferTooSmall)?;
        if uses_nalu_info_structs {
            cur += 3;
        }
        if av.len() < cur {
            return Err(Error::BufferTooSmall);
        }
        Ok(AvPacket {
            packet_index,
            frame_index,
            uses_nalu_info_structs,
            is_video,
            is_haptics: false,
            unit_index,
            units_in_frame_total,
            units_in_frame_fec,
            codec,
            word_at_0x18,
            adaptive_stream_index,
            byte_at_0x2c,
            key_pos,
            data_offset: 1 + cur,
            data_len: av.len() - cur,
        })
    } else {
        cur += 1;
        let mut is_haptics = false;
        if v12 {
            if av.len() < cur + 1 {
                return Err(Error::BufferTooSmall);
            }
            is_haptics = av[cur] == 0x02;
            cur += 1;
        }
        if av.len() < cur {
            return Err(Error::BufferTooSmall);
        }
        Ok(AvPacket {
            packet_index,
            frame_index,
            uses_nalu_info_structs,
            is_video,
            is_haptics,
            unit_index,
            units_in_frame_total,
            units_in_frame_fec,
            codec,
            word_at_0x18: 0,
            adaptive_stream_index: 0,
            byte_at_0x2c: 0,
            key_pos,
            data_offset: 1 + cur,
            data_len: av.len() - cur,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn av_parse_vector() {
        let packet: &[u8] = &[
            0x2, 0x0, 0x2d, 0x0, 0x5, 0x0, 0xc0, 0x1c, 0x1, 0x3, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0xe4, 0x10, 0x3, 0x67, 0x0, 0x29, 0xf3, 0x2f, 0x98, 0xf6, 0x99, 0x82, 0x83, 0x78, 0xdb,
            0x29, 0x43, 0xa9, 0xe5, 0x88, 0xf2, 0x11, 0x4, 0x20, 0xe6, 0x20, 0x96, 0xe9, 0x6, 0xee,
            0xd, 0x27, 0xa1, 0x83, 0x82, 0x88, 0xe6, 0x21, 0x49, 0x2, 0x75, 0x74, 0x32, 0x5b, 0xf6,
            0xe9, 0xdc, 0x93, 0xea, 0x31, 0x88, 0xd, 0x2b, 0x4b, 0x34, 0xf9, 0xec, 0x1b, 0x26,
            0xcc, 0xbb, 0xbb, 0x81, 0xf2, 0xd9, 0x2d, 0x8e, 0xa1, 0xb9, 0xe2, 0xb3, 0xca, 0xb2,
            0x7d, 0xa3, 0x31, 0xf0, 0x42, 0xb7, 0xb6, 0x1e, 0x8f, 0x6d, 0xa2, 0x70, 0x46, 0xfd,
            0x7e, 0x9b, 0x60, 0x85, 0xb0, 0xed, 0x4f, 0x20, 0xb5, 0x1, 0x71, 0xa9, 0xaa, 0x18,
            0x6b, 0x2a, 0x90, 0xf3, 0xa7, 0x84, 0x36, 0xfd, 0x6d, 0x14, 0x83, 0x68, 0xa3, 0x9b,
            0x3a, 0xc8, 0xd4, 0x3a, 0x31, 0xa0, 0x9b, 0x61, 0xde, 0xa7, 0xed, 0x46, 0xb4, 0xa3,
            0xdf, 0x3f, 0x44, 0x8f, 0xad, 0x64, 0x9, 0xfc, 0x7a, 0xe7, 0x24, 0xf0, 0xd2, 0x42,
            0xd3, 0x57, 0x5a, 0x76, 0x0, 0xc5, 0xe0, 0x93, 0xa9, 0xf5, 0x32, 0x5d, 0xee, 0xf7,
            0x9d,
        ];
        let mut key_state = KeyState::new();
        let p = parse_av(packet, false, &mut key_state).unwrap();
        assert!(p.is_video);
        assert_eq!(p.packet_index, 45);
        assert_eq!(p.frame_index, 5);
        assert_eq!(p.unit_index, 6);
        assert_eq!(p.units_in_frame_total, 8);
        assert_eq!(p.units_in_frame_fec, 1);
        assert_eq!(p.codec, 3);
        assert_eq!(p.adaptive_stream_index, 0);
        assert_eq!(p.data_offset, 0x15);
        assert_eq!(p.data_len, 0x99);
    }
}
