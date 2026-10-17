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

pub const TOUCH_ID_MASK: u8 = 0x7f;
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
    pub touch_id_next: u8,
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
            touch_id_next: 0,
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

    pub fn start_touch(&mut self, x: u16, y: u16) -> i8 {
        for touch in &mut self.touches {
            if touch.id < 0 {
                touch.id = self.touch_id_next as i8;
                self.touch_id_next = (self.touch_id_next + 1) & TOUCH_ID_MASK;
                touch.x = x;
                touch.y = y;
                return touch.id;
            }
        }
        -1
    }

    pub fn stop_touch(&mut self, id: u8) {
        for touch in &mut self.touches {
            if touch.id as u8 == id {
                touch.id = -1;
                break;
            }
        }
    }

    pub fn set_touch_pos(&mut self, id: u8, x: u16, y: u16) {
        let id = (id & TOUCH_ID_MASK) as i8;
        for touch in &mut self.touches {
            if touch.id == id {
                touch.x = x;
                touch.y = y;
                break;
            }
        }
    }

    pub fn equals(&self, other: &Self) -> bool {
        let float_eq = |a: f32, b: f32| (a - b).abs() < 1e-7;
        self.buttons == other.buttons
            && self.l2_state == other.l2_state
            && self.r2_state == other.r2_state
            && self.left_x == other.left_x
            && self.left_y == other.left_y
            && self.right_x == other.right_x
            && self.right_y == other.right_y
            && self
                .touches
                .iter()
                .zip(&other.touches)
                .all(|(a, b)| a.id == b.id && (a.id < 0 || (a.x == b.x && a.y == b.y)))
            && float_eq(self.gyro_x, other.gyro_x)
            && float_eq(self.gyro_y, other.gyro_y)
            && float_eq(self.gyro_z, other.gyro_z)
            && float_eq(self.accel_x, other.accel_x)
            && float_eq(self.accel_y, other.accel_y)
            && float_eq(self.accel_z, other.accel_z)
            && float_eq(self.orient_x, other.orient_x)
            && float_eq(self.orient_y, other.orient_y)
            && float_eq(self.orient_z, other.orient_z)
            && float_eq(self.orient_w, other.orient_w)
    }

    /// Union used when merging e.g. keyboard and gamepad input.
    pub fn or(a: &Self, b: &Self) -> Self {
        let max_abs = |x: i16, y: i16| if x.abs() > y.abs() { x } else { y };
        let orf = |x: f32, idle: f32, y: f32| if x == idle { y } else { x };
        let mut out = Self::idle();
        out.buttons = a.buttons | b.buttons;
        out.l2_state = a.l2_state.max(b.l2_state);
        out.r2_state = a.r2_state.max(b.r2_state);
        out.left_x = max_abs(a.left_x, b.left_x);
        out.left_y = max_abs(a.left_y, b.left_y);
        out.right_x = max_abs(a.right_x, b.right_x);
        out.right_y = max_abs(a.right_y, b.right_y);
        out.accel_x = orf(a.accel_x, 0.0, b.accel_x);
        out.accel_y = orf(a.accel_y, 1.0, b.accel_y);
        out.accel_z = orf(a.accel_z, 0.0, b.accel_z);
        out.gyro_x = orf(a.gyro_x, 0.0, b.gyro_x);
        out.gyro_y = orf(a.gyro_y, 0.0, b.gyro_y);
        out.gyro_z = orf(a.gyro_z, 0.0, b.gyro_z);
        out.orient_x = orf(a.orient_x, 0.0, b.orient_x);
        out.orient_y = orf(a.orient_y, 0.0, b.orient_y);
        out.orient_z = orf(a.orient_z, 0.0, b.orient_z);
        out.orient_w = orf(a.orient_w, 1.0, b.orient_w);
        for i in 0..TOUCHES_MAX {
            out.touches[i] = if a.touches[i].id >= 0 {
                a.touches[i]
            } else {
                b.touches[i]
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_ids_cycle() {
        let mut s = ControllerState::idle();
        assert_eq!(s.start_touch(1, 2), 0);
        assert_eq!(s.start_touch(3, 4), 1);
        assert_eq!(s.start_touch(5, 6), -1);
        s.stop_touch(0);
        assert_eq!(s.start_touch(7, 8), 2);
    }

    #[test]
    fn equality_ignores_lifted_touch_position() {
        let mut a = ControllerState::idle();
        let mut b = ControllerState::idle();
        assert!(a.equals(&b));
        a.touches[0] = ControllerTouch { x: 9, y: 9, id: -1 };
        b.touches[0] = ControllerTouch { x: 0, y: 0, id: -1 };
        assert!(a.equals(&b));
    }
}
