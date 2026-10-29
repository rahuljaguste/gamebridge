// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Pure phone-sensor math shared by the browser and Node tests.
(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.GameBridgeMotion = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const DEG = Math.PI / 180;
  const GRAVITY = 9.80665;
  // Device-to-screen rotation sign. Keep this in one place so a real-device
  // calibration can flip it without touching the call sites.
  const SCREEN_ROTATION_SIGN = -1;

  function clamp(value, min, max) {
    const number = Number(value);
    if (!Number.isFinite(number)) return 0;
    return Math.max(min, Math.min(max, number));
  }

  function quatMultiply(a, b) {
    const [ax, ay, az, aw] = a;
    const [bx, by, bz, bw] = b;
    return [
      aw * bx + ax * bw + ay * bz - az * by,
      aw * by - ax * bz + ay * bw + az * bx,
      aw * bz + ax * by - ay * bx + az * bw,
      aw * bw - ax * bx - ay * by - az * bz,
    ];
  }

  function quatNormalize(q) {
    if (!Array.isArray(q) || q.length !== 4) return [0, 0, 0, 1];
    const values = q.map(Number);
    if (values.some((value) => !Number.isFinite(value))) return [0, 0, 0, 1];
    const norm = Math.hypot(...values);
    if (!Number.isFinite(norm) || norm < 1e-8) return [0, 0, 0, 1];
    return values.map((value) => value / norm);
  }

  function quatConjugate(q) {
    const normalized = quatNormalize(q);
    return [-normalized[0], -normalized[1], -normalized[2], normalized[3]];
  }

  function quatYaw(q) {
    const [x, y, z, w] = quatNormalize(q);
    return Math.atan2(2 * (w * z + x * y), 1 - 2 * (y * y + z * z));
  }

  function quatFromYaw(yawRad) {
    return axisAngle([0, 0, 1], Number(yawRad) || 0);
  }

  function axisAngle(axis, angle) {
    const half = angle / 2;
    const sine = Math.sin(half);
    return [axis[0] * sine, axis[1] * sine, axis[2] * sine, Math.cos(half)];
  }

  function quatFromEulerZXY(alphaDeg, betaDeg, gammaDeg) {
    const alpha = Number(alphaDeg) || 0;
    const beta = Number(betaDeg) || 0;
    const gamma = Number(gammaDeg) || 0;
    const qz = axisAngle([0, 0, 1], alpha * DEG);
    const qx = axisAngle([1, 0, 0], beta * DEG);
    const qy = axisAngle([0, 1, 0], gamma * DEG);
    // DeviceOrientation is intrinsic Z-X'-Y''.
    return quatNormalize(quatMultiply(qz, quatMultiply(qx, qy)));
  }

  function screenAngleRad(angleDeg) {
    return (Number(angleDeg) || 0) * DEG * SCREEN_ROTATION_SIGN;
  }

  function screenQuaternion(angleDeg) {
    return axisAngle([0, 0, 1], screenAngleRad(angleDeg));
  }

  function rotateVectorToScreen(vector, angleDeg) {
    const x = Number(vector?.[0]) || 0;
    const y = Number(vector?.[1]) || 0;
    const z = Number(vector?.[2]) || 0;
    const angle = screenAngleRad(angleDeg);
    const cosine = Math.cos(angle);
    const sine = Math.sin(angle);
    return [x * cosine - y * sine, x * sine + y * cosine, z];
  }

  function gyroDegPerSecToRadPerSec(rotationRate, angleDeg) {
    // DeviceMotion rotationRate fields are degrees per second around z, x, y.
    const device = [
      Number(rotationRate?.beta) || 0,
      Number(rotationRate?.gamma) || 0,
      Number(rotationRate?.alpha) || 0,
    ];
    return rotateVectorToScreen(device, angleDeg).map((value) =>
      clamp(value * DEG, -30, 30),
    );
  }

  function accelMps2ToG(acceleration, angleDeg) {
    const device = Array.isArray(acceleration)
      ? [
          Number(acceleration[0]) || 0,
          Number(acceleration[1]) || 0,
          Number(acceleration[2]) || 0,
        ]
      : [
          Number(acceleration?.x) || 0,
          Number(acceleration?.y) || 0,
          Number(acceleration?.z) || 0,
        ];
    return rotateVectorToScreen(device, angleDeg).map((value) =>
      clamp(value / GRAVITY, -5, 5),
    );
  }

  return {
    clamp,
    quatMultiply,
    quatNormalize,
    quatConjugate,
    quatYaw,
    quatFromYaw,
    quatFromEulerZXY,
    screenQuaternion,
    rotateVectorToScreen,
    gyroDegPerSecToRadPerSec,
    accelMps2ToG,
  };
});
