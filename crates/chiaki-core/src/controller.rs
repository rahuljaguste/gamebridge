// SPDX-License-Identifier: AGPL-3.0-only
//! Controller state shared by the input path and the feedback sender.

pub const BUTTON_CROSS: u32 = 1 << 0;
pub const BUTTON_MOON: u32 = 1 << 1;
pub const BUTTON_BOX: u32 = 1 << 2;
pub const BUTTON_PYRAMID: u32 = 1 << 3;
pub const BUTTON_DPAD_LEFT: u32 = 1 << 4;
pub const BUTTON_DPAD_RIGHT: u32 = 1 << 5;
pub const BUTTON_DPAD_UP: u32 = 1 << 6;
pub const BUTTON_DPAD_DOWN: u32 = 1 << 7;
pub const BUTTON_L1: u32 = 1 << 8;
pub const BUTTON_R1: u32 = 1 << 9;
pub const BUTTON_L3: u32 = 1 << 10;
pub const BUTTON_R3: u32 = 1 << 11;
pub const BUTTON_OPTIONS: u32 = 1 << 12;
pub const BUTTON_SHARE: u32 = 1 << 13;
pub const BUTTON_TOUCHPAD: u32 = 1 << 14;
pub const BUTTON_PS: u32 = 1 << 15;
pub const BUTTONS_COUNT: u8 = 16;

/// Analog triggers are represented above the 16 digital buttons.
pub const ANALOG_BUTTON_L2: u32 = 1 << 16;
pub const ANALOG_BUTTON_R2: u32 = 1 << 17;

pub const TOUCHES_MAX: usize = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerTouch {
    pub x: u16,
    pub y: u16,
    /// -1 = lifted.
    pub id: i8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControllerState {
    pub buttons: u32,
    pub l2_state: u8,
    pub r2_state: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub touches: [ControllerTouch; TOUCHES_MAX],
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
}

impl Default for ControllerState {
    fn default() -> Self {
        Self::idle()
    }
}

impl ControllerState {
    pub fn idle() -> Self {
        Self {
            buttons: 0,
            l2_state: 0,
            r2_state: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
            touches: [
                ControllerTouch { x: 0, y: 0, id: -1 },
                ControllerTouch { x: 0, y: 0, id: -1 },
            ],
            gyro_x: 0.0,
            gyro_y: 0.0,
            gyro_z: 0.0,
            accel_x: 0.0,
            accel_y: 1.0,
            accel_z: 0.0,
            orient_x: 0.0,
            orient_y: 0.0,
            orient_z: 0.0,
            orient_w: 1.0,
        }
    }
}
