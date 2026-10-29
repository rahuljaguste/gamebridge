// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const motion = require("../crates/server/src/static/motion-math.js");

function near(actual, expected, tolerance = 1e-9) {
  assert.ok(
    Math.abs(actual - expected) <= tolerance,
    `expected ${actual} to be within ${tolerance} of ${expected}`,
  );
}

function nearVector(actual, expected, tolerance = 1e-9) {
  assert.equal(actual.length, expected.length);
  for (let i = 0; i < actual.length; i += 1) near(actual[i], expected[i], tolerance);
}

test("identity orientation", () => {
  nearVector(motion.quatFromEulerZXY(0, 0, 0), [0, 0, 0, 1]);
});

test("single-axis orientations use Z-X-Y order", () => {
  const half = Math.SQRT1_2;
  nearVector(motion.quatFromEulerZXY(90, 0, 0), [0, 0, half, half], 1e-12);
  nearVector(motion.quatFromEulerZXY(0, 90, 0), [half, 0, 0, half], 1e-12);
  nearVector(motion.quatFromEulerZXY(0, 0, 90), [0, half, 0, half], 1e-12);
});

test("Euler composition is qz * qx * qy", () => {
  const combined = motion.quatFromEulerZXY(20, 35, 50);
  const expected = motion.quatMultiply(
    motion.quatFromEulerZXY(20, 0, 0),
    motion.quatMultiply(
      motion.quatFromEulerZXY(0, 35, 0),
      motion.quatFromEulerZXY(0, 0, 50),
    ),
  );
  nearVector(combined, expected, 1e-12);
});

test("arbitrary orientations stay normalized", () => {
  for (const angles of [[15, -30, 70], [-80, 20, 120], [170, 45, -160]]) {
    const q = motion.quatFromEulerZXY(...angles);
    near(Math.hypot(...q), 1, 1e-12);
  }
});

test("screen rotation convention is locked", () => {
  nearVector(motion.rotateVectorToScreen([1, 0, 0], 0), [1, 0, 0]);
  nearVector(motion.rotateVectorToScreen([1, 0, 0], 90), [0, -1, 0], 1e-12);
  nearVector(motion.rotateVectorToScreen([1, 0, 0], 180), [-1, 0, 0], 1e-12);
  nearVector(motion.rotateVectorToScreen([1, 0, 0], 270), [0, 1, 0], 1e-12);
  nearVector(
    motion.screenQuaternion(90),
    [0, 0, Math.sin(-Math.PI / 4), Math.cos(-Math.PI / 4)],
    1e-12,
  );
});

test("gyro converts degrees per second and clamps", () => {
  nearVector(
    motion.gyroDegPerSecToRadPerSec({ alpha: 0, beta: 180, gamma: 0 }, 0),
    [Math.PI, 0, 0],
    1e-12,
  );
  nearVector(
    motion.gyroDegPerSecToRadPerSec({ alpha: 90, beta: 0, gamma: 0 }, 0),
    [0, 0, Math.PI / 2],
    1e-12,
  );
  nearVector(
    motion.gyroDegPerSecToRadPerSec({ alpha: 0, beta: 1800, gamma: 0 }, 0),
    [30, 0, 0],
  );
});

test("acceleration converts to g and clamps", () => {
  nearVector(
    motion.accelMps2ToG({ x: 0, y: 9.80665, z: 0 }, 0),
    [0, 1, 0],
    1e-12,
  );
  nearVector(motion.accelMps2ToG([0, 9.80665, 0], 0), [0, 1, 0], 1e-12);
  nearVector(motion.accelMps2ToG({ x: 100, y: 0, z: 0 }, 0), [5, 0, 0]);
  nearVector(motion.accelMps2ToG({ x: 0, y: 0, z: -100 }, 0), [0, 0, -5]);
});

test("quaternion conjugate recenters to identity", () => {
  const q = motion.quatFromEulerZXY(90, 30, -20);
  nearVector(motion.quatMultiply(motion.quatConjugate(q), q), [0, 0, 0, 1], 1e-12);
});

test("yaw recenter preserves tilt", () => {
  const q = motion.quatFromEulerZXY(90, 30, -20);
  const corrected = motion.quatMultiply(
    motion.quatFromYaw(-motion.quatYaw(q)),
    q,
  );
  near(motion.quatYaw(corrected), 0, 1e-12);
  near(Math.hypot(...corrected), 1, 1e-12);
  assert.notDeepEqual(corrected, [0, 0, 0, 1]);
});

test("clamp rejects non-finite values", () => {
  assert.equal(motion.clamp(Number.NaN, -1, 1), 0);
  assert.equal(motion.clamp(Infinity, -1, 1), 0);
  assert.equal(motion.clamp(4, -1, 1), 1);
});
