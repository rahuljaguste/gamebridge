// SPDX-License-Identifier: AGPL-3.0-only
//! Feedback state and history encoding (rumble, button edges, touch).

use std::collections::VecDeque;

use crate::controller::*;
use crate::error::{Error, Result};

const GYRO_MIN: f32 = -30.0;
const GYRO_MAX: f32 = 30.0;
const ACCEL_MIN: f32 = -5.0;
const ACCEL_MAX: f32 = 5.0;
const SQRT1_2: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[derive(Debug, Clone, Copy, Default)]
pub struct FeedbackState {
    pub gyro_x: f32,
    pub gyro_y: f32,
    pub gyro_z: f32,
    pub accel_x: f32,
    pub accel_y: f32,
    pub accel_z: f32,
    pub orient_x: f32,
    pub orient_y: f32,
    pub orient_z: f32,
    pub orient_w: f32,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

impl From<&ControllerState> for FeedbackState {
    fn from(s: &ControllerState) -> Self {
        Self {
            gyro_x: s.gyro_x,
            gyro_y: s.gyro_y,
            gyro_z: s.gyro_z,
            accel_x: s.accel_x,
            accel_y: s.accel_y,
            accel_z: s.accel_z,
            orient_x: s.orient_x,
            orient_y: s.orient_y,
            orient_z: s.orient_z,
            orient_w: s.orient_w,
            left_x: s.left_x,
            left_y: s.left_y,
            right_x: s.right_x,
            right_y: s.right_y,
        }
    }
}

fn compress_quat(q: [f32; 4]) -> u32 {
    let mut largest = 0usize;
    for (i, v) in q.iter().enumerate().skip(1) {
        if v.abs() > q[largest].abs() {
            largest = i;
        }
    }
    let mut r = (q[largest] < 0.0) as u32 | ((largest as u32) << 1);
    for i in 0..3 {
        let qi = if i < largest { i } else { i + 1 };
        let mut v = q[qi];
        v = v.clamp(-SQRT1_2, SQRT1_2);
        v += SQRT1_2;
        v *= 0x1ff as f32 / (2.0 * SQRT1_2);
        r |= (v as u32) << (3 + i * 9);
    }
    r
}

fn put_i16_be(buf: &mut [u8], off: usize, v: i16) {
    buf[off..off + 2].copy_from_slice(&v.to_be_bytes());
}

pub fn format_v9(state: &FeedbackState) -> [u8; 0x19] {
    let mut buf = [0u8; 0x19];
    buf[0] = 0xa0;
    let gyro =
        |v: f32| ((0xffff as f32 * (v - GYRO_MIN) / (GYRO_MAX - GYRO_MIN)) as u16).to_be_bytes();
    let accel =
        |v: f32| ((0xffff as f32 * (v - ACCEL_MIN) / (ACCEL_MAX - ACCEL_MIN)) as u16).to_be_bytes();
    buf[1..3].copy_from_slice(&gyro(state.gyro_x));
    buf[3..5].copy_from_slice(&gyro(state.gyro_y));
    buf[5..7].copy_from_slice(&gyro(state.gyro_z));
    buf[7..9].copy_from_slice(&accel(state.accel_x));
    buf[9..11].copy_from_slice(&accel(state.accel_y));
    buf[11..13].copy_from_slice(&accel(state.accel_z));
    let qc = compress_quat([
        state.orient_x,
        state.orient_y,
        state.orient_z,
        state.orient_w,
    ]);
    buf[13..17].copy_from_slice(&qc.to_be_bytes());
    put_i16_be(&mut buf, 0x11, state.left_x);
    put_i16_be(&mut buf, 0x13, state.left_y);
    put_i16_be(&mut buf, 0x15, state.right_x);
    put_i16_be(&mut buf, 0x17, state.right_y);
    buf
}

pub fn format_v12(state: &FeedbackState) -> [u8; 0x1c] {
    let mut buf = [0u8; 0x1c];
    let v9 = format_v9(state);
    buf[..0x19].copy_from_slice(&v9);
    buf[0x19] = 0;
    buf[0x1a] = 0;
    buf[0x1b] = 1;
    buf
}

#[derive(Debug, Clone, Copy)]
pub struct HistoryEvent {
    pub buf: [u8; 5],
    pub len: usize,
}

pub fn history_button(button: u64, state: u8) -> Result<HistoryEvent> {
    let mut event = HistoryEvent {
        buf: [0; 5],
        len: 2,
    };
    event.buf[0] = 0x80;
    let third = |event: &mut HistoryEvent, b: u8| {
        event.buf[1] = b;
        event.buf[2] = state;
        event.len = 3;
        Ok(*event)
    };
    match button {
        b if b == BUTTON_CROSS as u64 => third(&mut event, 0x88),
        b if b == BUTTON_MOON as u64 => third(&mut event, 0x89),
        b if b == BUTTON_BOX as u64 => third(&mut event, 0x8a),
        b if b == BUTTON_PYRAMID as u64 => third(&mut event, 0x8b),
        b if b == BUTTON_DPAD_LEFT as u64 => third(&mut event, 0x82),
        b if b == BUTTON_DPAD_RIGHT as u64 => third(&mut event, 0x83),
        b if b == BUTTON_DPAD_UP as u64 => third(&mut event, 0x80),
        b if b == BUTTON_DPAD_DOWN as u64 => third(&mut event, 0x81),
        b if b == BUTTON_L1 as u64 => third(&mut event, 0x84),
        b if b == BUTTON_R1 as u64 => third(&mut event, 0x85),
        b if b == ANALOG_BUTTON_L2 as u64 => third(&mut event, 0x86),
        b if b == ANALOG_BUTTON_R2 as u64 => third(&mut event, 0x87),
        b if b == BUTTON_L3 as u64 => {
            event.buf[1] = if state != 0 { 0xaf } else { 0x8f };
            Ok(event)
        }
        b if b == BUTTON_R3 as u64 => {
            event.buf[1] = if state != 0 { 0xb0 } else { 0x90 };
            Ok(event)
        }
        b if b == BUTTON_OPTIONS as u64 => {
            event.buf[1] = if state != 0 { 0xac } else { 0x8c };
            Ok(event)
        }
        b if b == BUTTON_SHARE as u64 => {
            event.buf[1] = if state != 0 { 0xad } else { 0x8d };
            Ok(event)
        }
        b if b == BUTTON_TOUCHPAD as u64 => {
            event.buf[1] = if state != 0 { 0xb1 } else { 0x91 };
            Ok(event)
        }
        b if b == BUTTON_PS as u64 => {
            event.buf[1] = if state != 0 { 0xae } else { 0x8e };
            Ok(event)
        }
        _ => Err(Error::InvalidData),
    }
}

pub fn history_touch(down: bool, pointer_id: u8, x: u16, y: u16) -> HistoryEvent {
    let mut event = HistoryEvent {
        buf: [0; 5],
        len: 5,
    };
    event.buf[0] = if down { 0xd0 } else { 0xc0 };
    event.buf[1] = pointer_id & 0x7f;
    event.buf[2] = (x >> 4) as u8;
    event.buf[3] = ((x & 0xf) << 4) as u8 | (y >> 8) as u8;
    event.buf[4] = y as u8;
    event
}

#[derive(Debug, Clone)]
pub struct HistoryBuffer {
    events: VecDeque<HistoryEvent>,
    capacity: usize,
}

impl HistoryBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, event: HistoryEvent) {
        if self.events.len() == self.capacity {
            self.events.pop_back();
        }
        self.events.push_front(event);
    }

    pub fn format(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.events.len() * 5);
        for event in &self.events {
            out.extend_from_slice(&event.buf[..event.len]);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_button_encodings() {
        assert_eq!(
            history_button(BUTTON_CROSS as u64, 0xff).unwrap().buf[..3],
            [0x80, 0x88, 0xff]
        );
        assert_eq!(
            history_button(BUTTON_L3 as u64, 0xff).unwrap().buf[..2],
            [0x80, 0xaf]
        );
        assert_eq!(
            history_button(BUTTON_L3 as u64, 0).unwrap().buf[..2],
            [0x80, 0x8f]
        );
        assert!(history_button(12345, 0).is_err());
    }

    #[test]
    fn touch_encoding() {
        let e = history_touch(true, 3, 0x123, 0x45);
        assert_eq!(e.buf, [0xd0, 3, 0x12, 0x30, 0x45]);
        let e = history_touch(false, 3, 0x123, 0x45);
        assert_eq!(e.buf, [0xc0, 3, 0x12, 0x30, 0x45]);
    }

    #[test]
    fn feedback_sizes() {
        let state = FeedbackState::default();
        assert_eq!(format_v9(&state).len(), 0x19);
        assert_eq!(format_v12(&state).len(), 0x1c);
    }
}
