// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Encoding/decoding of the Takion protobuf payloads we exchange.

use crate::error::{Error, Result};
use crate::proto::{Reader, Value, Writer, as_bool, as_bytes, as_string, as_u32};

pub const TYPE_BIG: u32 = 0;
pub const TYPE_BANG: u32 = 1;
pub const TYPE_HEARTBEAT: u32 = 3;
pub const TYPE_CORRUPTFRAME: u32 = 5;
pub const TYPE_DISCONNECT: u32 = 8;
pub const TYPE_STREAMINFO: u32 = 13;
pub const TYPE_STREAMINFOACK: u32 = 14;
pub const TYPE_CONNECTIONQUALITY: u32 = 16;
pub const TYPE_CONTROLLERCONNECTION: u32 = 21;
pub const TYPE_IDRREQUEST: u32 = 25;

pub const CONTROLLER_TYPE_DUALSHOCK4: u32 = 2;
pub const CONTROLLER_TYPE_DUALSENSE: u32 = 6;

#[derive(Debug, Clone)]
pub struct Bang {
    pub version_accepted: bool,
    pub encrypted_key_accepted: bool,
    pub ecdh_pub_key: Vec<u8>,
    pub ecdh_sig: Vec<u8>,
    pub server_version: Option<u32>,
    pub session_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VideoResolutionInfo {
    pub width: u32,
    pub height: u32,
    pub header: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub resolutions: Vec<VideoResolutionInfo>,
    pub audio_header: Vec<u8>,
}

pub fn encode_big(
    client_version: u32,
    session_key: &str,
    launch_spec: &str,
    ecdh_pub_key: &[u8],
    ecdh_sig: &[u8],
) -> Vec<u8> {
    let mut payload = Writer::new();
    payload.uint32(1, client_version);
    payload.string(2, session_key);
    payload.string(3, launch_spec);
    payload.bytes(4, &[0, 0, 0, 0]);
    payload.bytes(5, ecdh_pub_key);
    payload.bytes(6, ecdh_sig);

    let mut msg = Writer::new();
    msg.uint32(1, TYPE_BIG);
    msg.bytes(2, &payload.finish());
    msg.finish()
}

pub fn encode_controller_connection(dualsense: bool) -> Vec<u8> {
    let mut payload = Writer::new();
    payload.bool(2, true);
    payload.uint32(
        3,
        if dualsense {
            CONTROLLER_TYPE_DUALSENSE
        } else {
            CONTROLLER_TYPE_DUALSHOCK4
        },
    );
    let mut msg = Writer::new();
    msg.uint32(1, TYPE_CONTROLLERCONNECTION);
    msg.bytes(22, &payload.finish());
    msg.finish()
}

pub fn encode_streaminfo_ack() -> Vec<u8> {
    let mut msg = Writer::new();
    msg.uint32(1, TYPE_STREAMINFOACK);
    msg.finish()
}

pub fn encode_heartbeat() -> Vec<u8> {
    let mut msg = Writer::new();
    msg.uint32(1, TYPE_HEARTBEAT);
    msg.finish()
}

pub fn encode_disconnect(reason: &str) -> Vec<u8> {
    let mut payload = Writer::new();
    payload.string(1, reason);
    let mut msg = Writer::new();
    msg.uint32(1, TYPE_DISCONNECT);
    msg.bytes(10, &payload.finish());
    msg.finish()
}

pub fn encode_corrupt_frame(start: u16, end: u16) -> Vec<u8> {
    let mut payload = Writer::new();
    payload.uint32(1, start as u32);
    payload.uint32(2, end as u32);
    let mut msg = Writer::new();
    msg.uint32(1, TYPE_CORRUPTFRAME);
    msg.bytes(6, &payload.finish());
    msg.finish()
}

/// Ask the console to emit a fresh IDR. This is the explicit IDRREQUEST
/// payload (type 25) used by chiaki-ng; reporting a fake corrupt range of
/// 0..u16::MAX is not reliably answered by the PS5.
pub fn encode_idr_request() -> Vec<u8> {
    let mut msg = Writer::new();
    msg.uint32(1, TYPE_IDRREQUEST);
    msg.finish()
}

pub fn message_type(buf: &[u8]) -> Result<u32> {
    let mut r = Reader::new(buf);
    while let Some((field, value)) = r.next_field()? {
        if field == 1 {
            return as_u32(value);
        }
    }
    Err(Error::InvalidData)
}

/// Unwrap a Takion message envelope: validate the top-level type field and
/// return the inner payload carried by `payload_field`.
fn parse_envelope(buf: &[u8], expected_type: u32, payload_field: u32) -> Result<Vec<u8>> {
    let mut outer = Reader::new(buf);
    let mut payload = None;
    while let Some((field, value)) = outer.next_field()? {
        match field {
            1 => {
                if as_u32(value)? != expected_type {
                    return Err(Error::InvalidData);
                }
            }
            field if field == payload_field => payload = Some(as_bytes(value)?.to_vec()),
            _ => {}
        }
    }
    payload.ok_or(Error::InvalidData)
}

pub fn parse_bang(buf: &[u8]) -> Result<Bang> {
    let payload = parse_envelope(buf, TYPE_BANG, 3)?;
    let mut r = Reader::new(&payload);
    let mut bang = Bang {
        version_accepted: false,
        encrypted_key_accepted: false,
        ecdh_pub_key: Vec::new(),
        ecdh_sig: Vec::new(),
        server_version: None,
        session_key: None,
    };
    while let Some((field, value)) = r.next_field()? {
        match field {
            1 => bang.server_version = Some(as_u32(value)?),
            3 => bang.encrypted_key_accepted = as_bool(value)?,
            4 => bang.version_accepted = as_bool(value)?,
            5 => bang.session_key = Some(as_string(value)?),
            8 => bang.ecdh_pub_key = as_bytes(value)?.to_vec(),
            9 => bang.ecdh_sig = as_bytes(value)?.to_vec(),
            _ => {}
        }
    }
    Ok(bang)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectionQuality {
    /// Raw console-reported round-trip time. Firmware differs on the unit;
    /// see [`ConnectionQuality::rtt_ms`].
    pub rtt: Option<f64>,
}

impl ConnectionQuality {
    pub fn raw_rtt(self) -> Option<f64> {
        self.rtt.filter(|rtt| rtt.is_finite() && *rtt > 0.0)
    }

    /// Normalize the console's `double rtt` to milliseconds. WebRTC-style
    /// implementations report seconds; PS5 firmware has also been observed
    /// sending milliseconds. The unit is chosen from the first valid sample
    /// and kept for the session, so a later 1.5 s reading is still rendered as
    /// 1500 ms rather than mistaken for 1.5 ms.
    pub fn rtt_ms(self, seconds: &mut Option<bool>) -> Option<f64> {
        let raw = self.raw_rtt()?;
        let use_seconds = *seconds.get_or_insert(raw <= 1.0);
        Some(if use_seconds { raw * 1000.0 } else { raw })
    }
}

pub fn parse_connection_quality(buf: &[u8]) -> Result<ConnectionQuality> {
    let payload = parse_envelope(buf, TYPE_CONNECTIONQUALITY, 17)?;
    let mut r = Reader::new(&payload);
    let mut quality = ConnectionQuality::default();
    while let Some((field, value)) = r.next_field()? {
        if field == 5 {
            let Value::Fixed64(bytes) = value else {
                return Err(Error::InvalidData);
            };
            quality.rtt = Some(f64::from_le_bytes(bytes));
        }
    }
    Ok(quality)
}

pub fn parse_stream_info(buf: &[u8]) -> Result<StreamInfo> {
    let payload = parse_envelope(buf, TYPE_STREAMINFO, 15)?;
    let mut r = Reader::new(&payload);
    let mut resolutions = Vec::new();
    let mut audio_header = Vec::new();
    while let Some((field, value)) = r.next_field()? {
        match field {
            1 => {
                let bytes = as_bytes(value)?;
                resolutions.push(parse_resolution(bytes)?);
            }
            2 => audio_header = as_bytes(value)?.to_vec(),
            _ => {}
        }
    }
    if audio_header.len() != 0xe {
        return Err(Error::InvalidData);
    }
    Ok(StreamInfo {
        resolutions,
        audio_header,
    })
}

fn parse_resolution(buf: &[u8]) -> Result<VideoResolutionInfo> {
    let mut r = Reader::new(buf);
    let mut width = 0;
    let mut height = 0;
    let mut header = Vec::new();
    while let Some((field, value)) = r.next_field()? {
        match field {
            1 => width = as_u32(value)?,
            2 => height = as_u32(value)?,
            3 => header = as_bytes(value)?.to_vec(),
            _ => {}
        }
    }
    Ok(VideoResolutionInfo {
        width,
        height,
        header,
    })
}

pub fn parse_disconnect_reason(buf: &[u8]) -> Result<Option<String>> {
    let mut outer = Reader::new(buf);
    let mut payload = None;
    let mut ty = None;
    while let Some((field, value)) = outer.next_field()? {
        match field {
            1 => ty = Some(as_u32(value)?),
            10 => payload = Some(as_bytes(value)?.to_vec()),
            _ => {}
        }
    }
    if ty != Some(TYPE_DISCONNECT) {
        return Ok(None);
    }
    let Some(payload) = payload else {
        return Ok(Some(String::new()));
    };
    let mut r = Reader::new(&payload);
    while let Some((field, value)) = r.next_field()? {
        if field == 1 {
            return Ok(Some(as_string(value)?));
        }
    }
    Ok(Some(String::new()))
}

/// Rumble payloads are not protobuf; they have three direct bytes.
pub fn parse_rumble(buf: &[u8]) -> Option<(u8, u8, u8)> {
    (buf.len() >= 3).then(|| (buf[0], buf[1], buf[2]))
}

pub fn parse_trigger_effects(buf: &[u8]) -> Option<([u8; 10], [u8; 10], u8, u8)> {
    if buf.len() < 25 {
        return None;
    }
    let mut left = [0u8; 10];
    let mut right = [0u8; 10];
    left.copy_from_slice(&buf[5..15]);
    right.copy_from_slice(&buf[15..25]);
    Some((left, right, buf[1], buf[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idr_request_is_payload_type_25() {
        assert_eq!(encode_idr_request(), vec![0x08, 0x19]);
    }

    #[test]
    fn corrupt_frame_encodes_range() {
        assert_eq!(
            encode_corrupt_frame(1, 2),
            vec![0x08, 0x05, 0x32, 0x04, 0x08, 0x01, 0x10, 0x02]
        );
    }

    #[test]
    fn parses_connection_quality_rtt() {
        let mut inner = vec![0x29]; // field 5, wire type 1 (double)
        inner.extend_from_slice(&12.5f64.to_le_bytes());
        let mut msg = vec![0x08, 0x10, 0x8a, 0x01, inner.len() as u8];
        msg.extend_from_slice(&inner);

        let quality = parse_connection_quality(&msg).unwrap();
        assert_eq!(quality.rtt, Some(12.5));
        let mut unit = None;
        assert_eq!(quality.rtt_ms(&mut unit), Some(12.5));
    }

    #[test]
    fn connection_quality_unit_is_sticky() {
        let mut seconds = None;
        assert_eq!(
            ConnectionQuality { rtt: Some(0.0125) }.rtt_ms(&mut seconds),
            Some(12.5)
        );
        assert_eq!(
            ConnectionQuality { rtt: Some(1.5) }.rtt_ms(&mut seconds),
            Some(1500.0)
        );

        let mut milliseconds = None;
        assert_eq!(
            ConnectionQuality { rtt: Some(12.5) }.rtt_ms(&mut milliseconds),
            Some(12.5)
        );
        assert_eq!(
            ConnectionQuality { rtt: Some(1.5) }.rtt_ms(&mut milliseconds),
            Some(1.5)
        );

        assert_eq!(
            ConnectionQuality {
                rtt: Some(f64::NAN)
            }
            .rtt_ms(&mut milliseconds),
            None
        );
        assert_eq!(
            ConnectionQuality { rtt: Some(0.0) }.rtt_ms(&mut milliseconds),
            None
        );
    }
}
