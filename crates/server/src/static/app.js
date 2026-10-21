// SPDX-License-Identifier: AGPL-3.0-only

const B = {
  CROSS: 1 << 0,
  MOON: 1 << 1,
  BOX: 1 << 2,
  PYRAMID: 1 << 3,
  DPAD_LEFT: 1 << 4,
  DPAD_RIGHT: 1 << 5,
  DPAD_UP: 1 << 6,
  DPAD_DOWN: 1 << 7,
  L1: 1 << 8,
  R1: 1 << 9,
  L3: 1 << 10,
  R3: 1 << 11,
  OPTIONS: 1 << 12,
  SHARE: 1 << 13,
  TOUCHPAD: 1 << 14,
  PS: 1 << 15,
};

const ICE_RESTART_MAX = 3;
const $ = (id) => document.getElementById(id);
const video = $("video");
const videoWrap = $("video-wrap");
const overlay = $("video-overlay");
const overlayTitle = $("overlay-title");
const overlayText = $("overlay-text");
const hintOverlay = $("hint");
const statusPill = $("conn-status");
const statsEl = $("stats");

let consoleLatencyMs = null;
let packetLossPct = null;
let packetCount = { received: 0, lost: 0 };

function renderStats() {
  const parts = [];
  if (Number.isFinite(state.activeBitrate) && state.activeBitrate > 0) {
    parts.push(`${(state.activeBitrate / 1000).toFixed(state.activeBitrate % 1000 ? 1 : 0)} Mbps`);
  }
  if (Number.isFinite(consoleLatencyMs)) {
    const ms = consoleLatencyMs < 10
      ? consoleLatencyMs.toFixed(1)
      : Math.round(consoleLatencyMs);
    parts.push(`${ms} ms`);
  }
  if (Number.isFinite(packetLossPct) && packetLossPct >= 0.05) {
    parts.push(`${packetLossPct.toFixed(1)}% loss`);
  }
  parts.push(`${packetCount.received} pkts`, `${packetCount.lost} lost`);
  statsEl.textContent = parts.join(" · ");
}

function formatMbps(kbps) {
  const value = kbps / 1000;
  return `${value.toFixed(value % 1 ? 1 : 0)} Mbps`;
}

// Preserve settings saved under the previous product name on first load.
(function migrateLegacyStorage() {
  const legacyPrefix = "setu.";
  const prefix = "gamebridge.";
  try {
    const keys = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(legacyPrefix)) keys.push(key);
    }
    for (const key of keys) {
      const target = prefix + key.slice(legacyPrefix.length);
      if (localStorage.getItem(target) === null) {
        const value = localStorage.getItem(key);
        if (value !== null) localStorage.setItem(target, value);
      }
    }
  } catch {}
})();

const TOUCH_CONTROLS_KEY = "gamebridge.touchControls";
const MOTION_KEY = "gamebridge.motion";
const INSECURE_DISMISS_KEY = "gamebridge.insecureWarningDismissed";
const TOUCH_TAP_MIN_MS = 48;
const MOTION_STALE_MS = 500;
const MotionMath = window.GameBridgeMotion;

const TOUCH_BUTTON_BITS = {
  cross: B.CROSS,
  moon: B.MOON,
  box: B.BOX,
  pyramid: B.PYRAMID,
  l1: B.L1,
  r1: B.R1,
  options: B.OPTIONS,
  share: B.SHARE,
  ps: B.PS,
};
const TOUCH_DPAD_BITS = {
  up: B.DPAD_UP,
  down: B.DPAD_DOWN,
  left: B.DPAD_LEFT,
  right: B.DPAD_RIGHT,
};
const TOUCH_MENU_SOURCE = {
  pause: "pause-btn",
  mute: "mute-btn",
  fullscreen: "fullscreen-btn",
  screenshot: "screenshot-btn",
  resync: "resync-btn",
  settings: "settings-btn",
  rest: "rest-btn",
  stop: "stop-btn",
};

const VOLUME_KEY = "gamebridge.volume";
const MUTE_KEY = "gamebridge.muted";
const volumeSlider = $("volume-slider");
const muteBtn = $("mute-btn");
const audio = { volume: 1, muted: false, lastVolume: 1 };

function saveAudioState() {
  try {
    localStorage.setItem(VOLUME_KEY, String(audio.volume));
    localStorage.setItem(MUTE_KEY, audio.muted ? "1" : "0");
  } catch {}
}

function applyAudioState() {
  video.volume = audio.volume;
  video.muted = audio.muted || state.paused;
  volumeSlider.value = String(Math.round(audio.volume * 100));
  const silent = audio.muted || audio.volume === 0;
  muteBtn.classList.toggle("muted", silent);
  muteBtn.setAttribute("aria-pressed", String(audio.muted));
  const label = audio.muted ? "Unmute" : "Mute";
  muteBtn.setAttribute("aria-label", label);
  muteBtn.title = `${label} (M)`;
}

function setVolume(value) {
  audio.volume = Math.max(0, Math.min(1, value));
  if (audio.volume > 0) {
    audio.lastVolume = audio.volume;
    audio.muted = false;
  }
  applyAudioState();
}

function toggleMute() {
  audio.muted = !audio.muted;
  if (!audio.muted && audio.volume === 0) {
    audio.volume = audio.lastVolume || 0.5;
  }
  applyAudioState();
  saveAudioState();
}

const hostsEl = $("hosts");
const discoveredEl = $("discovered");
const pinModal = $("pin-modal");
const authModal = $("auth-modal");
const lockBtn = $("lock-btn");
const toastEl = $("toast");
const demoBtn = $("demo-btn");

const state = {
  hosts: [],
  sessionId: null,
  // Set by `session_started` on the event socket. A teardown event from an
  // older session must not clean up the session that replaced it.
  serverSessionId: null,
  // Synchronous guard: `sessionId` is only set after the start response, so
  // without this a double-click can fire two starts and the loser cleans up
  // the winner's player.
  starting: false,
  startEpoch: 0,
  pc: null,
  ws: null,
  wsRetry: null,
  wsRetryDelay: 500,
  stream: null,
  activeBitrate: null,
  inputTimer: null,
  lastInputJson: "",
  lastInputAt: 0,
  keyboard: new Set(),
  mouse: { dx: 0, dy: 0, left: false, right: false, locked: false },
  touches: new Map(),
  touchSeq: 1,
  padIndex: null,
  streaming: false,
  detachedActive: false,
  iceTimer: null,
  iceRestarting: false,
  iceAttempts: 0,
  iceGaveUp: false,
  iceStopTimer: null,
  hostId: null,
  paused: false,
  resumePending: null,
  resumeTarget: null,
  rumble: { left: 0, right: 0 },
  rumbleTimer: null,
  triggerEffectTimer: null,
  triggerEffect: { left: 0, right: 0 },
  touch: {
    enabled: false,
    pointers: new Map(),
    latches: new Map(),
    left: { x: 0, y: 0, active: false, pointerId: null, origin: null },
    right: { x: 0, y: 0, active: false, pointerId: null, origin: null },
    dpad: { up: false, down: false, left: false, right: false, pointerId: null, rect: null, origin: null },
    triggers: {
      l2: { active: false, pointerId: null, until: 0 },
      r2: { active: false, pointerId: null, until: 0 },
    },
  },
  motion: {
    enabled: true,
    permission: "unknown",
    sensorsAttached: false,
    lastEventAt: 0,
    rotationRate: { alpha: 0, beta: 0, gamma: 0 },
    acceleration: { x: 0, y: 9.80665, z: 0 },
    alpha: 0,
    beta: 0,
    gamma: 0,
    recenterQuat: [0, 0, 0, 1],
    hasOrientation: false,
  },
  ui: {
    touchCapable: false,
    portrait: false,
    insecure: false,
  },
};

(function initAudio() {
  try {
    const storedVolume = parseFloat(localStorage.getItem(VOLUME_KEY) ?? "");
    if (Number.isFinite(storedVolume)) {
      audio.volume = Math.max(0, Math.min(1, storedVolume));
    }
    audio.muted = localStorage.getItem(MUTE_KEY) === "1";
  } catch {}
  audio.lastVolume = audio.volume > 0 ? audio.volume : 1;
  applyAudioState();
  volumeSlider.addEventListener("input", () => {
    setVolume(parseInt(volumeSlider.value, 10) / 100);
  });
  volumeSlider.addEventListener("change", saveAudioState);
  muteBtn.addEventListener("click", toggleMute);
})();

// Console PINs are masked by default, with an explicit local reveal toggle.
function setupPinReveal(inputId, buttonId) {
  const input = $(inputId);
  const button = $(buttonId);
  if (!input || !button) return { reset() {} };
  const setVisible = (visible) => {
    input.type = visible ? "text" : "password";
    button.classList.toggle("revealed", visible);
    button.setAttribute("aria-pressed", String(visible));
    const label = visible ? "Hide PIN" : "Show PIN";
    button.setAttribute("aria-label", label);
    button.title = label;
  };
  button.addEventListener("click", () => setVisible(input.type === "password"));
  setVisible(false);
  return { reset: () => setVisible(false) };
}

const regPinReveal = setupPinReveal("reg-pin", "reg-pin-reveal");
const loginPinReveal = setupPinReveal("pin-input", "pin-reveal");

let detachedTimer = null;

/* ── Sharpening: FSR 1 EASU upscale + gamma-aware RCAS ──────────────────
 * EASU and RCAS are ported from AMD FidelityFX Super Resolution 1.0
 * (Copyright (c) 2021 Advanced Micro Devices, Inc.; MIT licence, see
 * THIRD_PARTY_NOTICES.md). EASU reconstructs the video at the display's
 * native pixel grid; RCAS then sharpens in linear light.
 */

const sharpCanvas = $("video-sharp");
const sharpnessInput = $("opt-sharpness");
const sharpnessValue = $("sharpness-value");
const brightnessInput = $("opt-brightness");
const brightnessValue = $("brightness-value");
const contrastInput = $("opt-contrast");
const contrastValue = $("contrast-value");
const saturationInput = $("opt-saturation");
const saturationValue = $("saturation-value");
const temporalInput = $("opt-temporal");
const temporalValue = $("temporal-value");
const pictureReset = $("picture-reset");

// The HTML carries the initial slider positions; keep one source of truth.
// The defaults are deliberately a mild "vivid" grade; zero every picture
// control to bypass the post-process.
// defaultValue avoids browser form restoration affecting the reset target.
const PICTURE_DEFAULTS = {
  sharpness: parseInt(sharpnessInput.defaultValue, 10),
  brightness: parseInt(brightnessInput.defaultValue, 10),
  contrast: parseInt(contrastInput.defaultValue, 10),
  saturation: parseInt(saturationInput.defaultValue, 10),
  temporal: parseInt(temporalInput.defaultValue, 10),
};

// Cap the post-process target. 4K is crisp on high-DPI displays without
// making integrated GPUs chew through an 8K EASU/RCAS pass at 60 fps.
const SHARP_MAX_PIXELS = 3840 * 2160;

const FULLSCREEN_VS = `#version 300 es
  void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
  }`;

const EASU_FS = `#version 300 es
  precision highp float;
  precision highp int;

  uniform sampler2D u_tex;
  uniform ivec2 u_in_size;
  uniform vec4 u_con0;

  out vec4 o_color;

  vec4 fetch(ivec2 p) {
    return texelFetch(u_tex, clamp(p, ivec2(0), u_in_size - 1), 0);
  }

  float luma2(vec4 c) {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
  }

  float sat1(float x) { return clamp(x, 0.0, 1.0); }
  float rcp_lo(float a) { return uintBitsToFloat(0x7ef07ebbu - floatBitsToUint(a)); }
  float rsq_lo(float a) { return uintBitsToFloat(0x5f347d74u - (floatBitsToUint(a) >> 1u)); }

  void easu_set(inout vec2 dir, inout float len, vec2 pp,
                bool biS, bool biT, bool biU, bool biV,
                float lA, float lB, float lC, float lD, float lE) {
    float w = 0.0;
    if (biS) w = (1.0 - pp.x) * (1.0 - pp.y);
    if (biT) w = pp.x * (1.0 - pp.y);
    if (biU) w = (1.0 - pp.x) * pp.y;
    if (biV) w = pp.x * pp.y;

    float dc = lD - lC;
    float cb = lC - lB;
    float lenX = max(abs(dc), abs(cb));
    lenX = rcp_lo(lenX);
    float dirX = lD - lB;
    dir.x += dirX * w;
    lenX = sat1(abs(dirX) * lenX);
    lenX *= lenX;
    len += lenX * w;

    float ec = lE - lC;
    float ca = lC - lA;
    float lenY = max(abs(ec), abs(ca));
    lenY = rcp_lo(lenY);
    float dirY = lE - lA;
    dir.y += dirY * w;
    lenY = sat1(abs(dirY) * lenY);
    lenY *= lenY;
    len += lenY * w;
  }

  void easu_tap(inout vec3 aC, inout float aW, vec2 off, vec2 dir, vec2 len,
                float lob, float clp, vec3 c) {
    vec2 v;
    v.x = off.x * dir.x + off.y * dir.y;
    v.y = off.x * (-dir.y) + off.y * dir.x;
    v *= len;
    float d2 = v.x * v.x + v.y * v.y;
    d2 = min(d2, clp);
    float wB = (2.0 / 5.0) * d2 - 1.0;
    float wA = lob * d2 - 1.0;
    wB *= wB;
    wA *= wA;
    wB = (25.0 / 16.0) * wB - (25.0 / 16.0 - 1.0);
    float w = wB * wA;
    aC += c * w;
    aW += w;
  }

  void main() {
    ivec2 ip = ivec2(gl_FragCoord.xy);

    vec2 pp = vec2(ip) * u_con0.xy + u_con0.zw;
    vec2 fp = floor(pp);
    pp -= fp;
    ivec2 fpi = ivec2(fp);

    // The 12-tap EASU kernel. These are the same texels AMD's gather4
    // path reads; WebGL2 has no textureGather, so fetch them directly.
    vec4 b = fetch(fpi + ivec2( 0, -1));
    vec4 c = fetch(fpi + ivec2( 1, -1));
    vec4 e = fetch(fpi + ivec2(-1,  0));
    vec4 f = fetch(fpi + ivec2( 0,  0));
    vec4 g = fetch(fpi + ivec2( 1,  0));
    vec4 h = fetch(fpi + ivec2( 2,  0));
    vec4 i = fetch(fpi + ivec2(-1,  1));
    vec4 j = fetch(fpi + ivec2( 0,  1));
    vec4 k = fetch(fpi + ivec2( 1,  1));
    vec4 l = fetch(fpi + ivec2( 2,  1));
    vec4 n = fetch(fpi + ivec2( 0,  2));
    vec4 o = fetch(fpi + ivec2( 1,  2));

    float bL = luma2(b), cL = luma2(c);
    float eL = luma2(e), fL = luma2(f), gL = luma2(g), hL = luma2(h);
    float iL = luma2(i), jL = luma2(j), kL = luma2(k), lL = luma2(l);
    float nL = luma2(n), oL = luma2(o);

    vec2 dir = vec2(0.0);
    float len = 0.0;
    easu_set(dir, len, pp, true,  false, false, false, bL, eL, fL, gL, jL);
    easu_set(dir, len, pp, false, true,  false, false, cL, fL, gL, hL, kL);
    easu_set(dir, len, pp, false, false, true,  false, fL, iL, jL, kL, nL);
    easu_set(dir, len, pp, false, false, false, true,  gL, jL, kL, lL, oL);

    vec2 dir2 = dir * dir;
    float dirR = dir2.x + dir2.y;
    bool zro = dirR < (1.0 / 32768.0);
    dirR = rsq_lo(dirR);
    dirR = zro ? 1.0 : dirR;
    dir.x = zro ? 1.0 : dir.x;
    dir *= dirR;
    len *= 0.5;
    len *= len;
    float stretch = (dir.x * dir.x + dir.y * dir.y) * rcp_lo(max(abs(dir.x), abs(dir.y)));
    vec2 len2 = vec2(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    float lob = 0.5 + ((0.25 - 0.04) - 0.5) * len;
    float clp = rcp_lo(lob);

    vec3 min4 = min(min(f.rgb, g.rgb), min(j.rgb, k.rgb));
    vec3 max4 = max(max(f.rgb, g.rgb), max(j.rgb, k.rgb));

    vec3 aC = vec3(0.0);
    float aW = 0.0;
    easu_tap(aC, aW, vec2( 0.0, -1.0) - pp, dir, len2, lob, clp, b.rgb);
    easu_tap(aC, aW, vec2( 1.0, -1.0) - pp, dir, len2, lob, clp, c.rgb);
    easu_tap(aC, aW, vec2(-1.0,  1.0) - pp, dir, len2, lob, clp, i.rgb);
    easu_tap(aC, aW, vec2( 0.0,  1.0) - pp, dir, len2, lob, clp, j.rgb);
    easu_tap(aC, aW, vec2( 0.0,  0.0) - pp, dir, len2, lob, clp, f.rgb);
    easu_tap(aC, aW, vec2(-1.0,  0.0) - pp, dir, len2, lob, clp, e.rgb);
    easu_tap(aC, aW, vec2( 1.0,  1.0) - pp, dir, len2, lob, clp, k.rgb);
    easu_tap(aC, aW, vec2( 2.0,  1.0) - pp, dir, len2, lob, clp, l.rgb);
    easu_tap(aC, aW, vec2( 2.0,  0.0) - pp, dir, len2, lob, clp, h.rgb);
    easu_tap(aC, aW, vec2( 1.0,  0.0) - pp, dir, len2, lob, clp, g.rgb);
    easu_tap(aC, aW, vec2( 1.0,  2.0) - pp, dir, len2, lob, clp, o.rgb);
    easu_tap(aC, aW, vec2( 0.0,  2.0) - pp, dir, len2, lob, clp, n.rgb);

    vec3 pix = min(max4, max(min4, aC * (1.0 / aW)));
    o_color = vec4(clamp(pix, 0.0, 1.0), 1.0);
  }`;

// Contrast-adaptive temporal accumulation. There are no engine motion
// vectors in a decoded stream, so history is clamped to the current 3x3
// neighbourhood and its weight falls off where the clamp had to move it.
// That keeps moving edges and scene cuts from smearing while settling
// compression shimmer and specular aliasing.
const TEMPORAL_FS = `#version 300 es
  precision highp float;
  precision highp int;

  uniform sampler2D u_current;
  uniform sampler2D u_history;
  uniform ivec2 u_size;
  uniform float u_weight;
  uniform int u_valid;

  out vec4 o_color;

  vec3 fetch(ivec2 p) {
    return texelFetch(u_current, clamp(p, ivec2(0), u_size - 1), 0).rgb;
  }

  void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    vec3 c = fetch(p);
    if (u_valid == 0 || u_weight <= 0.0) {
      o_color = vec4(c, 1.0);
      return;
    }
    vec3 lo = c;
    vec3 hi = c;
    for (int y = -1; y <= 1; y++) {
      for (int x = -1; x <= 1; x++) {
        vec3 s = fetch(p + ivec2(x, y));
        lo = min(lo, s);
        hi = max(hi, s);
      }
    }
    vec3 h = clamp(texelFetch(u_history, p, 0).rgb, lo, hi);
    float diff = max(max(abs(c.r - h.r), abs(c.g - h.g)), abs(c.b - h.b));
    float confidence = 1.0 - smoothstep(0.015, 0.14, diff);
    o_color = vec4(mix(c, h, clamp(u_weight * confidence, 0.0, 0.9)), 1.0);
  }`;

const RCAS_FS = `#version 300 es
  precision highp float;
  precision highp int;

  uniform sampler2D u_tex;
  uniform sampler2D u_lut;
  uniform ivec2 u_size;
  uniform float u_sharpness;
  uniform float u_exposure;
  uniform float u_contrast;
  uniform float u_saturation;

  out vec4 o_color;

  // This is the limit AMD uses for "unnatural results" in RCAS.
  const float FSR_RCAS_LIMIT = 0.1875;

  vec3 fetch(ivec2 p) {
    return texelFetch(u_tex, clamp(p, ivec2(0), u_size - 1), 0).rgb;
  }

  // Exact sRGB EOTF. The cubic approximation this replaced lost several
  // display codes in deep shadows across the encode/decode round trip.
  float srgb_to_linear(float x) {
    return x <= 0.04045 ? x / 12.92 : pow((x + 0.055) / 1.055, 2.4);
  }

  float linear_to_srgb(float x) {
    // The LUT has 256 texels; sample their centers, not the texture edges.
    float u = (clamp(x, 0.0, 1.0) * 255.0 + 0.5) / 256.0;
    return texture(u_lut, vec2(u, 0.5)).r;
  }

  float max3(float a, float b, float c) { return max(a, max(b, c)); }
  float min3(float a, float b, float c) { return min(a, min(b, c)); }

  void main() {
    ivec2 ip = ivec2(gl_FragCoord.xy);
    vec3 bg = fetch(ip + ivec2( 0, -1));
    vec3 dg = fetch(ip + ivec2(-1,  0));
    vec3 eg = fetch(ip);
    vec3 fg = fetch(ip + ivec2( 1,  0));
    vec3 hg = fetch(ip + ivec2( 0,  1));

    // Gamma-aware variant: decode the five taps, solve RCAS and clamp the
    // result in linear light, then re-encode. This avoids the highlight
    // overdrive a gamma-space unsharp mask produces.
    vec3 b = vec3(srgb_to_linear(bg.r), srgb_to_linear(bg.g), srgb_to_linear(bg.b));
    vec3 d = vec3(srgb_to_linear(dg.r), srgb_to_linear(dg.g), srgb_to_linear(dg.b));
    vec3 e = vec3(srgb_to_linear(eg.r), srgb_to_linear(eg.g), srgb_to_linear(eg.b));
    vec3 f = vec3(srgb_to_linear(fg.r), srgb_to_linear(fg.g), srgb_to_linear(fg.b));
    vec3 h = vec3(srgb_to_linear(hg.r), srgb_to_linear(hg.g), srgb_to_linear(hg.b));

    float mn4R = min(min3(b.r, d.r, f.r), h.r);
    float mn4G = min(min3(b.g, d.g, f.g), h.g);
    float mn4B = min(min3(b.b, d.b, f.b), h.b);
    float mx4R = max(max3(b.r, d.r, f.r), h.r);
    float mx4G = max(max3(b.g, d.g, f.g), h.g);
    float mx4B = max(max3(b.b, d.b, f.b), h.b);

    // RCAS limiters. Exact reciprocals, guarded where the original can hit
    // 0/0 on flat black or white ("no sharpening possible").
    float hitMinR = min(mn4R, e.r) * (4.0 * mx4R > 0.0 ? 1.0 / (4.0 * mx4R) : 0.0);
    float hitMinG = min(mn4G, e.g) * (4.0 * mx4G > 0.0 ? 1.0 / (4.0 * mx4G) : 0.0);
    float hitMinB = min(mn4B, e.b) * (4.0 * mx4B > 0.0 ? 1.0 / (4.0 * mx4B) : 0.0);
    float denR = 4.0 * mn4R - 4.0;
    float denG = 4.0 * mn4G - 4.0;
    float denB = 4.0 * mn4B - 4.0;
    float hitMaxR = denR < -1e-6 ? (1.0 - max(mx4R, e.r)) / denR : 0.0;
    float hitMaxG = denG < -1e-6 ? (1.0 - max(mx4G, e.g)) / denG : 0.0;
    float hitMaxB = denB < -1e-6 ? (1.0 - max(mx4B, e.b)) / denB : 0.0;

    float lobeR = max(-hitMinR, hitMaxR);
    float lobeG = max(-hitMinG, hitMaxG);
    float lobeB = max(-hitMinB, hitMaxB);
    float lobe = max(-FSR_RCAS_LIMIT, min(max3(lobeR, lobeG, lobeB), 0.0)) * u_sharpness;
    float rcpL = 1.0 / (4.0 * lobe + 1.0);
    vec3 pix = (lobe * (b + d + h + f) + e) * rcpL;

    // Colour grade in linear light: exposure, then contrast around middle
    // gray, then saturation around Rec.709 luma. Doing this before the sRGB
    // encode keeps the sliders perceptually well behaved.
    pix *= u_exposure;
    pix = (pix - 0.18) * u_contrast + 0.18;
    float luma = dot(pix, vec3(0.2126, 0.7152, 0.0722));
    pix = vec3(luma) + (pix - luma) * u_saturation;
    pix = max(pix, 0.0);

    o_color = vec4(linear_to_srgb(pix.r), linear_to_srgb(pix.g), linear_to_srgb(pix.b), 1.0);
  }`;

const sharp = {
  gl: null,
  easuProgram: null,
  temporalProgram: null,
  rcasProgram: null,
  inputTex: null,
  midTex: null,
  fbo: null,
  historyTex: [null, null],
  historyFbo: [null, null],
  historyIndex: 0,
  historyValid: false,
  lutTex: null,
  u: {
    easuTex: null, easuSize: null, con0: null,
    tmpCurrent: null, tmpHistory: null, tmpSize: null, tmpWeight: null, tmpValid: null,
    rcasTex: null, rcasLut: null, rcasSize: null, rcasSharp: null,
    rcasExposure: null, rcasContrast: null, rcasSaturation: null,
  },
  conCache: null,
  grade: { exposure: 1, contrast: 1, saturation: 1, temporal: 0 },
  inW: 0,
  inH: 0,
  outSize: null,
  texW: 0,
  texH: 0,
  maxTex: 0,
  active: false,
  shown: false,
  raf: 0,
  rvfc: 0,
  sizeDirty: true,
  dpr: 0,
  unavailable: false,
  factor: 0,
};

function sliderAmount(input, min, max) {
  const n = parseInt(input.value, 10);
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : 0;
}

function sharpnessAmount() {
  return sliderAmount(sharpnessInput, 0, 100);
}

function colorAmount(input) {
  return sliderAmount(input, -100, 100);
}

function temporalAmount() {
  return sliderAmount(temporalInput, 0, 100);
}

// The UI slider is inverted onto FSR's "stops" scale: 100 is FSR sharpness
// 0 (maximum) and lower values halve the effect every 50 slider points.
function sharpnessFactor() {
  const amount = sharpnessAmount();
  return amount > 0 ? Math.pow(2, -(100 - amount) / 50) : 0;
}

function colorIsNeutral() {
  return colorAmount(brightnessInput) === 0
    && colorAmount(contrastInput) === 0
    && colorAmount(saturationInput) === 0;
}

function postProcessActive() {
  return sharpnessAmount() > 0 || !colorIsNeutral() || temporalAmount() > 0;
}

// Brightness is exposure in stops (±1.5 EV); contrast pivots around linear
// middle gray; saturation is a straight 0..2× mix.
function updateGrade() {
  const exposure = Math.pow(2, colorAmount(brightnessInput) / 100 * 1.5);
  const contrast = Math.max(0.05, 1 + colorAmount(contrastInput) / 100);
  const saturation = Math.max(0, 1 + colorAmount(saturationInput) / 100);
  sharp.grade.exposure = exposure;
  sharp.grade.contrast = contrast;
  sharp.grade.saturation = saturation;
  const temporal = temporalAmount() / 100;
  if (temporal !== sharp.grade.temporal) sharp.historyValid = false;
  sharp.grade.temporal = temporal;
  brightnessValue.textContent = String(colorAmount(brightnessInput));
  contrastValue.textContent = String(colorAmount(contrastInput));
  saturationValue.textContent = String(colorAmount(saturationInput));
  temporalValue.textContent = String(temporalAmount());
}

function makeSharpProgram(gl, fragmentSource) {
  const compile = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      console.warn("sharpening shader:", gl.getShaderInfoLog(shader));
      gl.deleteShader(shader);
      return null;
    }
    return shader;
  };
  const vs = compile(gl.VERTEX_SHADER, FULLSCREEN_VS);
  const fs = compile(gl.FRAGMENT_SHADER, fragmentSource);
  if (!vs || !fs) {
    if (vs) gl.deleteShader(vs);
    if (fs) gl.deleteShader(fs);
    return null;
  }
  const program = gl.createProgram();
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.warn("sharpening program:", gl.getProgramInfoLog(program));
    gl.deleteProgram(program);
    return null;
  }
  return program;
}

function makeSrgbLut(gl) {
  const tex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, tex);
  const data = new Uint8Array(256 * 4);
  for (let i = 0; i < 256; i++) {
    const x = i / 255;
    const s = x <= 0.0031308 ? 12.92 * x : 1.055 * Math.pow(x, 1 / 2.4) - 0.055;
    const v = Math.round(Math.min(1, Math.max(0, s)) * 255);
    data[i * 4] = v;
    data[i * 4 + 1] = v;
    data[i * 4 + 2] = v;
    data[i * 4 + 3] = 255;
  }
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 256, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  return tex;
}

function initSharpGl() {
  if (sharp.gl || sharp.unavailable) return sharp.gl;
  const gl = sharpCanvas.getContext("webgl2", {
    alpha: false,
    antialias: false,
    depth: false,
    stencil: false,
    desynchronized: true,
    powerPreference: "high-performance",
  });
  if (!gl) {
    sharp.unavailable = true;
    return null;
  }
  if (gl.isContextLost()) return null;
  const easu = makeSharpProgram(gl, EASU_FS);
  const temporal = makeSharpProgram(gl, TEMPORAL_FS);
  const rcas = makeSharpProgram(gl, RCAS_FS);
  if (!easu || !temporal || !rcas) {
    if (easu) gl.deleteProgram(easu);
    if (temporal) gl.deleteProgram(temporal);
    if (rcas) gl.deleteProgram(rcas);
    sharp.unavailable = true;
    return null;
  }

  const inputTex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, inputTex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);

  sharp.gl = gl;
  sharp.easuProgram = easu;
  sharp.temporalProgram = temporal;
  sharp.rcasProgram = rcas;
  sharp.inputTex = inputTex;
  sharp.lutTex = makeSrgbLut(gl);
  sharp.maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE);
  sharp.u.easuTex = gl.getUniformLocation(easu, "u_tex");
  sharp.u.easuSize = gl.getUniformLocation(easu, "u_in_size");
  sharp.u.con0 = gl.getUniformLocation(easu, "u_con0");
  sharp.u.tmpCurrent = gl.getUniformLocation(temporal, "u_current");
  sharp.u.tmpHistory = gl.getUniformLocation(temporal, "u_history");
  sharp.u.tmpSize = gl.getUniformLocation(temporal, "u_size");
  sharp.u.tmpWeight = gl.getUniformLocation(temporal, "u_weight");
  sharp.u.tmpValid = gl.getUniformLocation(temporal, "u_valid");
  sharp.u.rcasTex = gl.getUniformLocation(rcas, "u_tex");
  sharp.u.rcasLut = gl.getUniformLocation(rcas, "u_lut");
  sharp.u.rcasSize = gl.getUniformLocation(rcas, "u_size");
  sharp.u.rcasSharp = gl.getUniformLocation(rcas, "u_sharpness");
  sharp.u.rcasExposure = gl.getUniformLocation(rcas, "u_exposure");
  sharp.u.rcasContrast = gl.getUniformLocation(rcas, "u_contrast");
  sharp.u.rcasSaturation = gl.getUniformLocation(rcas, "u_saturation");
  gl.disable(gl.BLEND);
  gl.disable(gl.DEPTH_TEST);
  gl.disable(gl.CULL_FACE);
  return gl;
}

function targetSize(vw, vh) {
  const dpr = window.devicePixelRatio || 1;
  if (!sharp.sizeDirty && sharp.dpr === dpr
      && sharp.inW === vw && sharp.inH === vh && sharp.outSize) {
    return sharp.outSize;
  }
  const cw = videoWrap.clientWidth;
  const ch = videoWrap.clientHeight;
  if (!cw || !ch) return null;

  sharp.sizeDirty = false;
  sharp.dpr = dpr;
  sharp.inW = vw;
  sharp.inH = vh;

  // Match the video's on-screen device-pixel size. This matters even when
  // the CSS size is smaller than the source: a 720p stream shown in a
  // 1200px-wide box at 2x DPR still has 2400 device pixels to fill.
  const scale = Math.min(cw / vw, ch / vh);
  let w = Math.max(vw, Math.round(vw * scale * dpr));
  let h = Math.max(vh, Math.round(w * vh / vw));

  const maxPixels = Math.min(SHARP_MAX_PIXELS, sharp.maxTex * sharp.maxTex);
  if (w * h > maxPixels) {
    const k = Math.sqrt(maxPixels / (w * h));
    w = Math.max(1, Math.floor(w * k));
    h = Math.max(1, Math.round(w * vh / vw));
  }
  if (w > sharp.maxTex) {
    w = sharp.maxTex;
    h = Math.max(1, Math.round(w * vh / vw));
  }
  if (h > sharp.maxTex) {
    h = sharp.maxTex;
    w = Math.max(1, Math.round(h * vw / vh));
  }
  sharp.outSize = { w, h };
  return sharp.outSize;
}

function easuConstants(vw, vh, w, h) {
  if (sharp.conCache && sharp.conCache.vw === vw && sharp.conCache.vh === vh
      && sharp.conCache.w === w && sharp.conCache.h === h) {
    return sharp.conCache;
  }
  const rx = vw / w;
  const ry = vh / h;
  const con = {
    vw, vh, w, h,
    con0: [rx, ry, 0.5 * rx - 0.5, 0.5 * ry - 0.5],
  };
  sharp.conCache = con;
  return con;
}

function makeColorTarget(gl, w, h) {
  const tex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, tex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  const fbo = gl.createFramebuffer();
  gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
  gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
  const status = gl.checkFramebufferStatus(gl.FRAMEBUFFER);
  gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  if (status !== gl.FRAMEBUFFER_COMPLETE) {
    console.warn("sharpening framebuffer incomplete:", status);
    gl.deleteFramebuffer(fbo);
    gl.deleteTexture(tex);
    return null;
  }
  return { tex, fbo };
}

function deleteColorTarget(gl, tex, fbo) {
  if (fbo) gl.deleteFramebuffer(fbo);
  if (tex) gl.deleteTexture(tex);
}

function ensureSharpTargets(w, h) {
  const gl = sharp.gl;
  if (sharp.midTex && sharp.historyTex[0] && sharp.historyTex[1]
      && sharp.texW === w && sharp.texH === h) {
    return true;
  }
  deleteColorTarget(gl, sharp.midTex, sharp.fbo);
  deleteColorTarget(gl, sharp.historyTex[0], sharp.historyFbo[0]);
  deleteColorTarget(gl, sharp.historyTex[1], sharp.historyFbo[1]);
  sharp.midTex = null;
  sharp.fbo = null;
  sharp.historyTex = [null, null];
  sharp.historyFbo = [null, null];

  const mid = makeColorTarget(gl, w, h);
  const historyA = makeColorTarget(gl, w, h);
  const historyB = makeColorTarget(gl, w, h);
  if (!mid || !historyA || !historyB) {
    if (mid) deleteColorTarget(gl, mid.tex, mid.fbo);
    if (historyA) deleteColorTarget(gl, historyA.tex, historyA.fbo);
    if (historyB) deleteColorTarget(gl, historyB.tex, historyB.fbo);
    sharp.unavailable = true;
    return false;
  }
  sharp.midTex = mid.tex;
  sharp.fbo = mid.fbo;
  sharp.historyTex = [historyA.tex, historyB.tex];
  sharp.historyFbo = [historyA.fbo, historyB.fbo];
  sharp.texW = w;
  sharp.texH = h;
  sharp.historyIndex = 0;
  sharp.historyValid = false;
  return true;
}

function drawSharp() {
  const gl = sharp.gl;
  if (!gl) return false;
  if (video.readyState < 2) return true;
  const vw = video.videoWidth;
  const vh = video.videoHeight;
  if (!vw || !vh) return true;

  const target = targetSize(vw, vh);
  if (!target) return true;
  const { w, h } = target;
  if (!ensureSharpTargets(w, h)) {
    stopSharpening();
    return false;
  }
  if (sharpCanvas.width !== w || sharpCanvas.height !== h) {
    sharpCanvas.width = w;
    sharpCanvas.height = h;
  }

  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, sharp.inputTex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, video);

  const con = easuConstants(vw, vh, w, h);
  gl.bindFramebuffer(gl.FRAMEBUFFER, sharp.fbo);
  gl.viewport(0, 0, w, h);
  gl.useProgram(sharp.easuProgram);
  gl.uniform1i(sharp.u.easuTex, 0);
  gl.uniform2i(sharp.u.easuSize, vw, vh);
  gl.uniform4fv(sharp.u.con0, con.con0);
  gl.drawArrays(gl.TRIANGLES, 0, 3);

  let postTex = sharp.midTex;
  if (sharp.grade.temporal > 0) {
    const write = sharp.historyIndex ^ 1;
    gl.bindFramebuffer(gl.FRAMEBUFFER, sharp.historyFbo[write]);
    gl.viewport(0, 0, w, h);
    gl.useProgram(sharp.temporalProgram);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, sharp.midTex);
    gl.uniform1i(sharp.u.tmpCurrent, 0);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, sharp.historyTex[sharp.historyIndex]);
    gl.uniform1i(sharp.u.tmpHistory, 1);
    gl.uniform2i(sharp.u.tmpSize, w, h);
    gl.uniform1f(sharp.u.tmpWeight, sharp.grade.temporal);
    gl.uniform1i(sharp.u.tmpValid, sharp.historyValid ? 1 : 0);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    sharp.historyIndex = write;
    sharp.historyValid = true;
    postTex = sharp.historyTex[write];
  } else {
    // Re-enabling temporal must not blend against frames from before it was
    // switched off.
    sharp.historyValid = false;
  }

  gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  gl.viewport(0, 0, w, h);
  gl.useProgram(sharp.rcasProgram);
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, postTex);
  gl.uniform1i(sharp.u.rcasTex, 0);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, sharp.lutTex);
  gl.uniform1i(sharp.u.rcasLut, 1);
  gl.uniform2i(sharp.u.rcasSize, w, h);
  gl.uniform1f(sharp.u.rcasSharp, sharp.factor);
  gl.uniform1f(sharp.u.rcasExposure, sharp.grade.exposure);
  gl.uniform1f(sharp.u.rcasContrast, sharp.grade.contrast);
  gl.uniform1f(sharp.u.rcasSaturation, sharp.grade.saturation);
  gl.drawArrays(gl.TRIANGLES, 0, 3);

  if (!sharp.shown) {
    sharp.shown = true;
    sharpCanvas.classList.remove("hidden");
    video.classList.add("sharpening");
  }
  return true;
}

function sharpFrame() {
  if (!sharp.active) return;
  if (drawSharp()) {
    if (video.requestVideoFrameCallback) {
      sharp.rvfc = video.requestVideoFrameCallback(sharpFrame);
    } else {
      sharp.raf = requestAnimationFrame(sharpFrame);
    }
  }
}

function startSharpening() {
  if (sharp.active || !postProcessActive()) return;
  if (!initSharpGl()) return;
  sharp.active = true;
  sharp.sizeDirty = true;
  sharpFrame();
}

function stopSharpening() {
  sharp.active = false;
  sharp.shown = false;
  if (sharp.rvfc && video.cancelVideoFrameCallback) video.cancelVideoFrameCallback(sharp.rvfc);
  if (sharp.raf) cancelAnimationFrame(sharp.raf);
  sharp.rvfc = 0;
  sharp.raf = 0;
  sharpCanvas.classList.add("hidden");
  video.classList.remove("sharpening");
}

function persistPicture() {
  try {
    localStorage.setItem("gamebridge.sharpness", String(sharpnessAmount()));
    localStorage.setItem("gamebridge.brightness", String(colorAmount(brightnessInput)));
    localStorage.setItem("gamebridge.contrast", String(colorAmount(contrastInput)));
    localStorage.setItem("gamebridge.saturation", String(colorAmount(saturationInput)));
    localStorage.setItem("gamebridge.temporal", String(temporalAmount()));
  } catch {}
}

function applyPicture() {
  sharpnessValue.textContent = String(sharpnessAmount());
  sharp.factor = sharpnessFactor();
  updateGrade();
  if (!postProcessActive()) stopSharpening();
  else if (state.stream) startSharpening();
}

function bindPictureSlider(input) {
  input.addEventListener("input", applyPicture);
  input.addEventListener("change", persistPicture);
}

[sharpnessInput, brightnessInput, contrastInput, saturationInput, temporalInput]
  .forEach(bindPictureSlider);

pictureReset.addEventListener("click", () => {
  sharpnessInput.value = String(PICTURE_DEFAULTS.sharpness);
  brightnessInput.value = String(PICTURE_DEFAULTS.brightness);
  contrastInput.value = String(PICTURE_DEFAULTS.contrast);
  saturationInput.value = String(PICTURE_DEFAULTS.saturation);
  temporalInput.value = String(PICTURE_DEFAULTS.temporal);
  persistPicture();
  applyPicture();
});

(function initPicture() {
  const load = (input, key, fallback) => {
    let value = fallback;
    try {
      const stored = localStorage.getItem(key);
      if (stored !== null) {
        const n = parseInt(stored, 10);
        if (Number.isFinite(n)) value = n;
      }
    } catch {}
    const min = parseInt(input.min, 10);
    const max = parseInt(input.max, 10);
    value = Math.min(max, Math.max(min, value));
    input.value = String(value);
  };
  load(sharpnessInput, "gamebridge.sharpness", PICTURE_DEFAULTS.sharpness);
  load(brightnessInput, "gamebridge.brightness", PICTURE_DEFAULTS.brightness);
  load(contrastInput, "gamebridge.contrast", PICTURE_DEFAULTS.contrast);
  load(saturationInput, "gamebridge.saturation", PICTURE_DEFAULTS.saturation);
  load(temporalInput, "gamebridge.temporal", PICTURE_DEFAULTS.temporal);
  applyPicture();
})();

let resizeTimer = 0;
function scheduleSizeDirty() {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => { sharp.sizeDirty = true; }, 150);
}

if (typeof ResizeObserver === "function") {
  new ResizeObserver(scheduleSizeDirty).observe(videoWrap);
} else {
  window.addEventListener("resize", scheduleSizeDirty);
}
function updateFullscreenButton() {
  const button = $("fullscreen-btn");
  if (!button) return;
  const active = Boolean(document.fullscreenElement);
  button.textContent = active ? "Exit fullscreen" : "Fullscreen";
  button.title = active ? "Exit fullscreen" : "Enter fullscreen";
  button.setAttribute("aria-label", active ? "Exit fullscreen" : "Enter fullscreen");
}

document.addEventListener("fullscreenchange", () => {
  sharp.sizeDirty = true;
  updateFullscreenButton();
  resetTouchControls();
  updateOrientationState();
  if (touchMenuIsOpen()) updateTouchMenuState();
});
updateFullscreenButton();

sharpCanvas.addEventListener("webglcontextlost", (event) => {
  event.preventDefault();
  sharp.gl = null;
  sharp.easuProgram = null;
  sharp.temporalProgram = null;
  sharp.rcasProgram = null;
  sharp.inputTex = null;
  sharp.midTex = null;
  sharp.fbo = null;
  sharp.historyTex = [null, null];
  sharp.historyFbo = [null, null];
  sharp.historyValid = false;
  sharp.lutTex = null;
  sharp.texW = 0;
  sharp.texH = 0;
  sharp.unavailable = false;
  stopSharpening();
});
sharpCanvas.addEventListener("webglcontextrestored", () => {
  sharp.unavailable = false;
  sharp.sizeDirty = true;
  if (postProcessActive() && state.stream) startSharpening();
});


function setStatus(text, kind = "") {
  statusPill.textContent = text;
  statusPill.className = "status-pill" + (kind ? " " + kind : "");
}

let toastTimer = null;
let toastKind = "";
function toast(message, bad = false, duration = 4500, kind = "") {
  // A new toast replaces the old one, so its dismissal timer must be reset;
  // otherwise the previous timer hides the new message almost immediately.
  if (toastTimer) clearTimeout(toastTimer);
  toastKind = kind;
  toastEl.textContent = message;
  toastEl.className = "toast" + (bad ? " bad" : "");
  toastEl.setAttribute("role", bad ? "alert" : "status");
  toastEl.setAttribute("aria-live", bad ? "assertive" : "polite");
  toastTimer = setTimeout(() => {
    toastEl.classList.add("hidden");
    toastTimer = null;
  }, bad ? Math.max(duration, 7000) : duration);
}

function dismissToast() {
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = null;
  toastKind = "";
  toastEl.classList.add("hidden");
}

async function api(path, options = {}) {
  const response = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  const text = await response.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = text; }
  if (!response.ok) {
    const error = new Error((data && data.error) || `HTTP ${response.status}`);
    error.status = response.status;
    if (response.status === 401 && path !== "/api/auth/login") {
      showAuthModal();
    }
    throw error;
  }
  return data;
}

function showAuthModal(message = "") {
  if (state.sessionId || state.streaming || state.ws || state.detachedActive || state.pc) {
    cleanupSession(true);
  }
  closeAddConsole({ restoreFocus: false, notify: false });
  closeSettings();
  hideContextToast();
  state.hosts = [];
  renderHosts();
  discoveredEl.innerHTML = "";
  lockBtn.classList.add("hidden");
  pinModal.classList.add("hidden");
  authModal.classList.remove("hidden");
  if (message) $("auth-error").textContent = message;
  $("auth-token").focus();
  updateContextMessage();
}

function hideAuthModal() {
  authModal.classList.add("hidden");
  $("auth-error").textContent = "";
  $("auth-token").value = "";
  updateContextMessage();
}

function showOverlay(title, text) {
  overlayTitle.textContent = title;
  overlayText.textContent = text;
  overlay.classList.remove("hidden");
}

function hideOverlay() {
  overlay.classList.add("hidden");
}

function showPlayer() {
  $("home-view").classList.add("hidden");
  $("player-view").classList.remove("hidden");
  document.body.classList.add("player-active");
  hideContextToast();
  updateContextMessage();
  syncTouchControlsUi();
  updateOrientationState();
}

function showHome() {
  $("home-view").classList.remove("hidden");
  $("player-view").classList.add("hidden");
  document.body.classList.remove("player-active");
  hidePlayerBar();
  closeTouchMenu({ restoreFocus: false });
  resetTouchControls();
  stopMotionSensors();
  rotateHintEl.classList.add("hidden");
  updateInsecureBanner();
  syncTouchControlsUi();
  updateMotionStatus();
  updateContextMessage();
}

function showHomeNotice(title, text) {
  toast(text ? `${title} — ${text}` : title, true, 6500);
}

function showResumePanel(pending) {
  state.resumeTarget = pending;
  const text = $("resume-text");
  text.textContent = pending.reason
    ? `The console reported: ${pending.reason}. It may be in Rest Mode.`
    : "The console disconnected. It may be in Rest Mode.";
  $("wake-resume-btn").disabled = !pending.hostId;
  $("resume-panel").classList.remove("hidden");
  setStatus("Disconnected", "bad");
}

function hideResumePanel() {
  state.resumeTarget = null;
  $("resume-panel").classList.add("hidden");
}

async function resumeFromDisconnect() {
  const pending = state.resumeTarget;
  if (!pending?.hostId) return;
  let host = state.hosts.find((item) => item.id === pending.hostId);
  if (!host) {
    await loadHosts();
    host = state.hosts.find((item) => item.id === pending.hostId);
  }
  if (!host) {
    toast("The console is no longer in the saved list", true);
    return;
  }
  hideResumePanel();
  await startSession(host);
}

$("wake-resume-btn").addEventListener("click", () => {
  resumeFromDisconnect().catch((error) => toast(error.message, true));
});
$("resume-back-btn").addEventListener("click", () => {
  hideResumePanel();
  showHome();
  setStatus("Idle", "");
});

let contextReady = false;
let lastShownContext = "";

function contextMessage() {
  if (!authModal.classList.contains("hidden")) return "Private access to this bridge";
  if (addIsOpen()) return "Pair a new console";
  if (settingsIsOpen()) return "Fine-tune your stream";
  if (discovering) return "Scanning your network…";
  if (state.streaming) return "Streaming live";
  if (state.hosts.length) return "Game Console Bridge";
  if (discoveredEl.querySelector(".nearby-tile")) return "Pair a console to get started";
  return "Your console, in your browser";
}

function canShowContextToast() {
  if (document.body.classList.contains("player-active")) return false;
  if (settingsIsOpen() || addIsOpen()) return false;
  if (!authModal.classList.contains("hidden")) return false;
  if (!toastEl.classList.contains("hidden") && toastKind !== "context") return false;
  return true;
}

function updateContextMessage() {
  const message = contextMessage();
  if (!contextReady || message === lastShownContext) return;
  if (!canShowContextToast()) return;
  lastShownContext = message;
  toast(message, false, 3200, "context");
}

function primeContextToast() {
  contextReady = true;
  const message = contextMessage();
  lastShownContext = message;
  if (canShowContextToast()) toast(message, false, 3200, "context");
}

function hideContextToast() {
  if (toastKind !== "context") return;
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = null;
  toastKind = "";
  toastEl.classList.add("hidden");
}

let playerBarTimer = null;

function showPlayerBar(autoHide = true) {
  // Touch devices use the dedicated Menu button and its modal sheet. Keeping
  // the desktop player bar off-screen means it can never overlap a stick,
  // including after a fullscreen resize.
  if ((state.ui.touchCapable || state.touch.enabled) && state.streaming) {
    hidePlayerBar();
    return;
  }
  if (document.body.classList.contains("player-active")) {
    $("player-bar").classList.remove("hidden");
  }
  clearTimeout(playerBarTimer);
  if (autoHide && state.streaming) {
    playerBarTimer = setTimeout(() => {
      if (settingsIsOpen()) return;
      if (state.touch.pointers.size > 0 || state.touch.latches.size > 0) {
        showPlayerBar(true);
        return;
      }
      $("player-bar").classList.add("hidden");
    }, 3000);
  }
}

function hidePlayerBar() {
  clearTimeout(playerBarTimer);
  $("player-bar").classList.add("hidden");
}

let settingsReturnFocus = null;

function settingsIsOpen() {
  return $("settings-overlay").classList.contains("open");
}

function openSettings(tab = "stream") {
  if (addIsOpen()) closeAddConsole({ restoreFocus: false, notify: false });
  if (document.fullscreenElement) {
    ignorePromiseRejection(document.exitFullscreen?.());
  }
  resetTouchControls();
  hideContextToast();
  const overlay = $("settings-overlay");
  if (!overlay.classList.contains("open")) {
    settingsReturnFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    overlay.removeAttribute("inert");
    overlay.setAttribute("aria-hidden", "false");
    overlay.classList.add("open");
  }
  setSettingsTab(tab);
  clearTimeout(playerBarTimer);
  syncTouchControlsUi();
  updateMotionStatus();
  $("settings-drawer").focus({ preventScroll: true });
  updateContextMessage();
}

function closeSettings() {
  const overlay = $("settings-overlay");
  if (!overlay.classList.contains("open")) return;
  cancelRebind(false);
  overlay.classList.remove("open");
  overlay.setAttribute("aria-hidden", "true");
  overlay.setAttribute("inert", "");
  if (settingsReturnFocus && document.contains(settingsReturnFocus)) {
    settingsReturnFocus.focus({ preventScroll: true });
  }
  settingsReturnFocus = null;
  syncTouchControlsUi();
  updateMotionStatus();
  if (document.body.classList.contains("player-active")) showPlayerBar();
  updateContextMessage();
}

function setSettingsTab(name) {
  for (const key of ["stream", "picture", "input"]) {
    $(`settings-${key}`).classList.toggle("hidden", key !== name);
    const tab = $(`settings-${key}-tab`);
    tab.classList.toggle("active", key === name);
    tab.setAttribute("aria-selected", String(key === name));
  }
}

let addReturnFocus = null;

function addIsOpen() {
  return $("add-overlay").classList.contains("open");
}

function setFieldHelp(name, open) {
  $(`${name}-help-btn`).setAttribute("aria-expanded", String(open));
  $(`${name}-help`).classList.toggle("hidden", !open);
}

function toggleFieldHelp(name) {
  const willOpen = $(`${name}-help`).classList.contains("hidden");
  for (const key of ["host", "pin"]) {
    setFieldHelp(key, key === name ? willOpen : false);
  }
}

function closeFieldHelps() {
  setFieldHelp("host", false);
  setFieldHelp("pin", false);
}

function openAddConsole({ auto = false } = {}) {
  closeSettings();
  hideContextToast();
  const overlay = $("add-overlay");
  if (!overlay.classList.contains("open")) {
    addReturnFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    overlay.removeAttribute("inert");
    overlay.setAttribute("aria-hidden", "false");
    overlay.classList.add("open");
  }
  closeFieldHelps();
  regPinReveal.reset();
  if (auto) $("add-drawer").focus({ preventScroll: true });
  else $("reg-host").focus();
  updateContextMessage();
}

function closeAddConsole({ restoreFocus = true, notify = true } = {}) {
  const overlay = $("add-overlay");
  if (!overlay.classList.contains("open")) return;
  overlay.classList.remove("open");
  overlay.setAttribute("aria-hidden", "true");
  overlay.setAttribute("inert", "");
  closeFieldHelps();
  regPinReveal.reset();
  if (restoreFocus && addReturnFocus && document.contains(addReturnFocus)) {
    addReturnFocus.focus({ preventScroll: true });
  }
  addReturnFocus = null;
  if (notify) updateContextMessage();
}

function setBusy(busy) {
  $("stop-btn").disabled = !busy;
}

/* ── Hosts ─────────────────────────────────────────────────────────────── */

async function loadHosts() {
  try {
    state.hosts = await api("/api/hosts");
    renderHosts();
    return true;
  } catch (e) {
    if (e.status !== 401) toast(`Could not load consoles: ${e.message}`, true);
    return false;
  }
}

function hostDisplayName(host) {
  return host.name || host.nickname || host.host;
}

function consoleAvatar(name) {
  return String(name || "").trim().charAt(0).toUpperCase() || "C";
}

// Deterministic blue/violet artwork tint per console, so each tile has its
// own identity without loading any external images.
function hueFor(name) {
  let hash = 0;
  for (const ch of String(name || "console")) {
    hash = (hash * 31 + ch.codePointAt(0)) >>> 0;
  }
  return 198 + (hash % 96);
}

function tileArt(name) {
  const art = document.createElement("span");
  art.className = "tile-art";
  art.setAttribute("aria-hidden", "true");
  const device = document.createElement("span");
  device.className = "tile-device";
  const monogram = document.createElement("span");
  monogram.className = "tile-monogram";
  monogram.textContent = consoleAvatar(name);
  art.append(device, monogram);
  return art;
}

function tileCaption(name, meta) {
  const caption = document.createElement("span");
  caption.className = "tile-caption";
  const title = document.createElement("span");
  title.className = "tile-name";
  title.textContent = name;
  const line = document.createElement("span");
  line.className = "tile-meta";
  line.textContent = meta;
  caption.append(title, line);
  return caption;
}

function tileBadge(text, nearby = false) {
  const badge = document.createElement("span");
  badge.className = "tile-badge" + (nearby ? " nearby" : "");
  badge.textContent = text;
  return badge;
}

function isPs4(target) {
  return String(target || "").startsWith("ps4");
}

function consoleMenu(host, displayName) {
  const menu = document.createElement("details");
  menu.className = "tile-menu row-menu";
  const summary = document.createElement("summary");
  summary.setAttribute("aria-label", "More options");
  summary.textContent = "⋯";
  const pop = document.createElement("div");
  pop.className = "menu-pop";

  const wake = document.createElement("button");
  wake.type = "button";
  wake.textContent = "Wake";
  wake.onclick = async () => {
    menu.open = false;
    wake.disabled = true;
    wake.textContent = "Waking…";
    try {
      await api(`/api/wakeup/${encodeURIComponent(host.id)}`, { method: "POST", body: "{}" });
      toast(`Wake packet sent to ${displayName}`);
    } catch (e) {
      toast(e.message, true);
    } finally {
      wake.disabled = false;
      wake.textContent = "Wake";
    }
  };

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "danger-text";
  remove.textContent = "Remove";
  remove.onclick = async () => {
    menu.open = false;
    if (!confirm(`Remove ${displayName}?`)) return;
    remove.disabled = true;
    try {
      await api(`/api/hosts/${encodeURIComponent(host.id)}`, { method: "DELETE" });
      state.hosts = state.hosts.filter((h) => h.id !== host.id);
      renderHosts();
      loadHosts();
      discover({ quiet: true });
    } catch (e) {
      toast(e.message, true);
      remove.disabled = false;
    }
  };

  pop.append(wake, remove);
  menu.append(summary, pop);
  return menu;
}

function renderHosts() {
  hostsEl.innerHTML = "";
  for (const host of state.hosts) {
    const displayName = hostDisplayName(host);
    const slot = document.createElement("article");
    slot.className = "console-slot";
    slot.style.setProperty("--hue", String(hueFor(displayName)));

    const tile = document.createElement("button");
    tile.type = "button";
    tile.className = "console-tile" + (isPs4(host.target) ? " ps4" : "");
    tile.setAttribute("aria-label", `Play on ${displayName}`);
    tile.onclick = () => startSession(host);
    tile.append(
      tileArt(displayName),
      tileBadge(targetLabel(host.target)),
      tileCaption(displayName, host.host),
    );

    const play = document.createElement("span");
    play.className = "tile-play";
    play.textContent = "▶";
    play.setAttribute("aria-hidden", "true");
    tile.appendChild(play);

    slot.append(tile, consoleMenu(host, displayName));
    hostsEl.appendChild(slot);
  }
  updateContextMessage();
}

function targetLabel(target) {
  return ({ ps5_1: "PS5", ps4_10: "PS4 8.0+", ps4_9: "PS4 7.0", ps4_8: "PS4" })[target] || target;
}

let detectTimer = null;
let detectSeq = 0;

function setDetectStatus(text, kind = "") {
  const status = $("reg-target-status");
  status.className = "detect-status" + (kind ? " " + kind : "");
  $("reg-target-text").textContent = text;
}

function targetRadios() {
  return document.querySelectorAll('input[name="console-type"]');
}

function clearTargetSelection() {
  $("reg-target").value = "";
  for (const radio of targetRadios()) radio.checked = false;
}

function applyDetectedTarget(target) {
  $("reg-target").value = target;
  for (const radio of targetRadios()) {
    radio.checked = radio.value === target;
  }
  setDetectStatus(`Detected ${targetLabel(target)}`, "good");
}

async function detectConsole() {
  const host = $("reg-host").value.trim();
  if (!host) {
    clearTargetSelection();
    setDetectStatus("Enter the console IP or scan for consoles.", "bad");
    return;
  }
  const seq = ++detectSeq;
  $("reg-detect-btn").disabled = true;
  setDetectStatus("Detecting console type…", "busy");
  try {
    const found = await api("/api/detect", {
      method: "POST",
      body: JSON.stringify({ host }),
    });
    if (seq !== detectSeq) return;
    applyDetectedTarget(found.target);
  } catch (e) {
    if (seq !== detectSeq) return;
    clearTargetSelection();
    const text = e.status === 404
      ? "No console responded. Choose the type manually."
      : `Could not detect: ${e.message}`;
    setDetectStatus(text, "bad");
  } finally {
    $("reg-detect-btn").disabled = false;
  }
}

function onHostInput() {
  clearTimeout(detectTimer);
  detectSeq += 1;
  clearTargetSelection();
  const host = $("reg-host").value.trim();
  if (!host) {
    setDetectStatus("Console type is detected automatically.", "");
    $("reg-detect-btn").disabled = false;
    return;
  }
  setDetectStatus("Waiting to detect console type…", "");
  detectTimer = setTimeout(detectConsole, 650);
}

$("reg-detect-btn").addEventListener("click", detectConsole);
$("reg-host").addEventListener("input", onHostInput);
for (const radio of targetRadios()) {
  radio.addEventListener("change", () => {
    if (!radio.checked) return;
    clearTimeout(detectTimer);
    detectSeq += 1;
    $("reg-target").value = radio.value;
    setDetectStatus(`Manual: ${targetLabel(radio.value)}`, "");
  });
}

const PSN_ACCOUNT_KEY = "gamebridge.psnAccountId";
const PSN_LEGACY_SIGNIN_KEY = "gamebridge.psnSignInId";

function readSaved(key) {
  try { return localStorage.getItem(key) || ""; } catch { return ""; }
}

function writeSaved(key, value) {
  try {
    const text = String(value || "").trim();
    if (text) localStorage.setItem(key, text);
    else localStorage.removeItem(key);
  } catch {}
}

function updatePsnForgetVisibility() {
  $("reg-account-clear").classList.toggle("hidden", !readSaved(PSN_ACCOUNT_KEY));
}

function clearLegacyPsnSignIn() {
  try { localStorage.removeItem(PSN_LEGACY_SIGNIN_KEY); } catch {}
}

function loadSavedPsnAccount() {
  clearLegacyPsnSignIn();
  const account = readSaved(PSN_ACCOUNT_KEY);
  if (account) $("reg-account").value = account;
  updatePsnForgetVisibility();
}

function savePsnAccount(value) {
  writeSaved(PSN_ACCOUNT_KEY, value);
  updatePsnForgetVisibility();
}

function forgetSavedPsnAccount() {
  clearLegacyPsnSignIn();
  writeSaved(PSN_ACCOUNT_KEY, "");
  $("reg-account").value = "";
  updatePsnForgetVisibility();
  toast("Saved PSN Account ID removed from this device");
}

$("reg-account-clear").addEventListener("click", forgetSavedPsnAccount);

let psnCodeTimer = null;
let psnCodeSeq = 0;

function setPsnCodeStatus(text, kind = "") {
  $("psn-code-status").className = "detect-status" + (kind ? " " + kind : "");
  $("psn-code-text").textContent = text;
}

function isPsnRedirect(value) {
  return /^https?:\/\//i.test(value);
}

async function resolvePsnCode() {
  clearTimeout(psnCodeTimer);
  const input = $("reg-psn-code").value.trim();
  if (!input) {
    setPsnCodeStatus("Paste the redirect URL or code; the Account ID is filled automatically.");
    return;
  }
  if (isPsnRedirect(input) && !/[?&]code=/.test(input)) {
    setPsnCodeStatus("That URL has no ?code=. Paste the complete redirect URL.", "bad");
    return;
  }
  const seq = ++psnCodeSeq;
  const button = $("psn-code-fetch");
  button.disabled = true;
  setPsnCodeStatus("Fetching Account ID…", "busy");
  try {
    const result = await api("/api/psn/account-id", {
      method: "POST",
      body: JSON.stringify({ redirect_url: input }),
    });
    if (seq !== psnCodeSeq) return;
    $("reg-account").value = result.account_id;
    savePsnAccount(result.account_id);
    $("reg-psn-code").value = "";
    setPsnCodeStatus("Account ID filled in and saved.", "good");
    $("reg-account").focus();
  } catch (e) {
    if (seq !== psnCodeSeq) return;
    setPsnCodeStatus(e.message, "bad");
  } finally {
    button.disabled = false;
  }
}

function onPsnCodeInput() {
  psnCodeSeq += 1;
  clearTimeout(psnCodeTimer);
  const value = $("reg-psn-code").value.trim();
  if (!value) {
    setPsnCodeStatus("Paste the redirect URL or code; the Account ID is filled automatically.");
    return;
  }
  if (isPsnRedirect(value) && /[?&]code=/.test(value)) {
    setPsnCodeStatus("Waiting to fetch Account ID…", "busy");
    psnCodeTimer = setTimeout(resolvePsnCode, 350);
  }
}

$("reg-psn-code").addEventListener("input", onPsnCodeInput);
$("reg-psn-code").addEventListener("paste", (event) => {
  const text = event.clipboardData?.getData("text");
  if (!text) {
    clearTimeout(psnCodeTimer);
    setTimeout(resolvePsnCode, 0);
    return;
  }
  event.preventDefault();
  $("reg-psn-code").value = text.trim();
  onPsnCodeInput();
  resolvePsnCode();
});
$("reg-psn-code").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    resolvePsnCode();
  }
});
$("psn-code-fetch").addEventListener("click", resolvePsnCode);
$("psn-login-link").addEventListener("click", (event) => {
  const url = $("psn-login-link").href;
  if (!url || url.endsWith("#")) return;
  event.preventDefault();
  const popup = window.open(url, "gamebridge-psn-login", "popup=yes,width=520,height=760");
  if (!popup) {
    const fallback = window.open(url, "_blank");
    if (!fallback) {
      toast("Allow popups to open PSN sign-in", true);
      return;
    }
    setPsnCodeStatus("Sign in, then copy the full URL from the sign-in tab and paste it below.");
    $("reg-psn-code").focus({ preventScroll: true });
    return;
  }
  popup.focus();
  setPsnCodeStatus("Sign in, then copy the full URL from the popup's address bar and paste it below.");
  $("reg-psn-code").focus({ preventScroll: true });
  const timer = setInterval(() => {
    if (!popup.closed) return;
    clearInterval(timer);
    setPsnCodeStatus("Sign-in window closed. Paste the redirect URL or code below.");
    $("reg-psn-code").focus({ preventScroll: true });
  }, 1000);
});

function setPsnLookupStatus(text, kind = "") {
  $("psn-lookup-status").className = "detect-status" + (kind ? " " + kind : "");
  $("psn-lookup-text").textContent = text;
}

async function publicLookupPsn() {
  const onlineId = $("reg-psn-lookup").value.trim();
  if (!onlineId) {
    setPsnLookupStatus("Enter your PSN Online ID.", "bad");
    $("reg-psn-lookup").focus();
    return;
  }
  const button = $("psn-lookup-btn");
  button.disabled = true;
  setPsnLookupStatus("Looking up PSN account…", "busy");
  try {
    const result = await api("/api/psn/public-lookup", {
      method: "POST",
      body: JSON.stringify({ online_id: onlineId }),
    });
    $("reg-account").value = result.account_id;
    savePsnAccount(result.account_id);
    setPsnLookupStatus("Account ID filled in and saved.", "good");
    $("reg-account").focus();
  } catch (e) {
    setPsnLookupStatus(e.message, "bad");
  } finally {
    button.disabled = false;
  }
}

$("psn-lookup-btn").addEventListener("click", publicLookupPsn);
$("reg-psn-lookup").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    publicLookupPsn();
  }
});

async function loadPsnLogin() {
  const link = $("psn-login-link");
  try {
    const info = await api("/api/psn/login");
    link.href = info.url;
    link.classList.remove("hidden");
  } catch (e) {
    if (e.status === 401) return;
    link.classList.add("hidden");
  }
}

let discovering = false;

async function discover({ quiet = false } = {}) {
  if (discovering) return;
  discovering = true;
  const btn = $("discover-btn");
  btn.disabled = true;
  btn.classList.add("scanning");
  btn.setAttribute("aria-label", "Scanning for consoles");
  setDiscoveredNote("Looking for consoles on your network…");
  updateContextMessage();
  try {
    const found = await api("/api/discover", { method: "POST", body: "{}" });
    renderDiscovered(found);
  } catch (e) {
    if (e.status === 401) return;
    setDiscoveredNote("Could not scan the network. Try again with the scan button.");
    if (!quiet) toast(`Scan failed: ${e.message}`, true);
  } finally {
    discovering = false;
    btn.disabled = false;
    btn.classList.remove("scanning");
    btn.setAttribute("aria-label", "Scan for consoles");
    updateContextMessage();
  }
}

function setDiscoveredNote(text) {
  discoveredEl.innerHTML = "";
  if (!text) return;
  const note = document.createElement("p");
  note.className = "grid-note";
  note.textContent = text;
  discoveredEl.appendChild(note);
}

function renderDiscovered(found) {
  const registered = new Set(state.hosts.map((host) => String(host.host || "").trim()));
  const nearby = found.filter((host) => !registered.has(String(host.host_addr || "").trim()));
  discoveredEl.innerHTML = "";
  if (!nearby.length) {
    setDiscoveredNote("No other consoles found nearby. Make sure the console is awake and Remote Play is enabled.");
    return;
  }
  for (const host of nearby) {
    const displayName = host.host_name || host.host_addr;
    const tile = document.createElement("button");
    tile.type = "button";
    tile.className = "console-tile nearby-tile" + (isPs4(host.target) ? " ps4" : "");
    tile.style.setProperty("--hue", String(hueFor(displayName)));
    tile.setAttribute("aria-label", `Add ${displayName}`);
    tile.onclick = () => {
      $("reg-host").value = host.host_addr;
      applyDetectedTarget(host.target);
      openAddConsole();
      $("reg-pin").focus();
    };
    tile.append(
      tileArt(displayName),
      tileBadge("Nearby", true),
      tileCaption(displayName, `${host.host_addr} · ${host.state || "unknown"} · ${targetLabel(host.target)}`),
    );

    const add = document.createElement("span");
    add.className = "tile-play";
    add.textContent = "+";
    add.setAttribute("aria-hidden", "true");
    tile.appendChild(add);
    discoveredEl.appendChild(tile);
  }
}

/* ── Registration ──────────────────────────────────────────────────────── */

$("register-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const target = $("reg-target").value;
  if (!target) {
    toast("Choose the console type or wait for detection", true);
    const first = document.querySelector('input[name="console-type"]');
    if (first) first.focus();
    return;
  }
  const button = $("register-btn");
  button.disabled = true;
  button.textContent = "Pairing…";
  try {
    await api("/api/register", {
      method: "POST",
      body: JSON.stringify({
        host: $("reg-host").value.trim(),
        target,
        pin: $("reg-pin").value.trim(),
        psn_account_id: $("reg-account").value.trim() || null,
        psn_online_id: $("reg-online").value.trim() || null,
        name: $("reg-name").value.trim() || null,
        broadcast: $("reg-broadcast").checked,
      }),
    });
    savePsnAccount($("reg-account").value.trim());
    $("reg-pin").value = "";
    regPinReveal.reset();
    toast("Console added");
    closeAddConsole();
    await loadHosts();
    discover({ quiet: true });
  } catch (e) {
    toast(e.message, true);
  } finally {
    button.disabled = false;
    button.textContent = "Add console";
  }
});

/* ── Session ───────────────────────────────────────────────────────────── */

async function startSession(host) {
  if (state.starting || state.pc || state.sessionId || state.detachedActive) {
    toast("A session is already active", true);
    return;
  }
  primeMotionForStart();
  state.starting = true;
  state.hostId = host.id;
  state.resumePending = null;
  state.resumeTarget = null;
  state.paused = false;
  $("resume-panel").classList.add("hidden");
  $("pause-btn").textContent = "Pause";
  const epoch = ++state.startEpoch;
  closeAddConsole({ notify: false });
  showPlayer();
  showOverlay("Connecting…", "Requesting a session. If the console is asleep, GameBridge wakes it first.");
  showPlayerBar(false);
  setStatus("Starting", "");
  try {
    const bitrate = parseInt($("opt-bitrate").value, 10);
    const response = await api("/api/session/start", {
      method: "POST",
      body: JSON.stringify({
        host_id: host.id,
        resolution: $("opt-resolution").value,
        fps: parseInt($("opt-fps").value, 10),
        bitrate_kbps: Number.isFinite(bitrate) ? bitrate : null,
        codec: $("opt-codec").value,
        adaptive_bitrate: $("opt-adaptive").checked,
        enable_dualsense: $("opt-dualsense").checked,
      }),
    });
    if (epoch !== state.startEpoch) {
      // The user stopped or a newer start superseded this response. Release
      // the console session the server just created.
      api("/api/session/stop", { method: "POST", body: "{}" }).catch(() => {});
      return;
    }
    const sessionId = response.session_id;
    state.sessionId = sessionId;
    state.activeBitrate = Number.isFinite(response.bitrate_kbps) && response.bitrate_kbps > 0
      ? response.bitrate_kbps
      : null;
    renderStats();
    if (response.adaptive_stepped_down) {
      toast(`Adaptive bitrate: starting at ${formatMbps(response.bitrate_kbps)} after recent packet loss`);
    }
    // Subscribe before ICE gathering so PIN/phase events are not missed.
    connectSocket();
    setBusy(true);
    await setupPeer(response.offer, sessionId);
    if (state.sessionId !== sessionId || epoch !== state.startEpoch) return;
    startInputLoop();
  } catch (e) {
    if (epoch !== state.startEpoch) return;
    // A WebRTC setup failure after the start response still owns a console
    // session. Release it now instead of leaving the manager to time out.
    if (state.sessionId) {
      try {
        await api("/api/session/stop", { method: "POST", body: "{}" });
      } catch {}
    }
    await cleanupSession(false);
    showHomeNotice("Could not start", e.message);
  } finally {
    if (epoch === state.startEpoch) state.starting = false;
  }
}

async function startDemo() {
  if (state.starting || state.pc || state.sessionId || state.detachedActive) {
    toast("A session is already active", true);
    return;
  }
  primeMotionForStart();
  state.starting = true;
  state.hostId = null;
  state.resumePending = null;
  state.resumeTarget = null;
  state.paused = false;
  $("resume-panel").classList.add("hidden");
  $("pause-btn").textContent = "Pause";
  const epoch = ++state.startEpoch;
  closeAddConsole({ notify: false });
  showPlayer();
  showOverlay("Connecting…", "Opening a local H.264 test stream.");
  showPlayerBar(false);
  setStatus("Starting", "");
  try {
    const response = await api("/api/demo/start", { method: "POST" });
    if (epoch !== state.startEpoch) {
      api("/api/session/stop", { method: "POST", body: "{}" }).catch(() => {});
      return;
    }
    const sessionId = response.session_id;
    state.sessionId = sessionId;
    state.activeBitrate = null;
    renderStats();
    connectSocket();
    setBusy(true);
    await setupPeer(response.offer, sessionId);
    if (state.sessionId !== sessionId || epoch !== state.startEpoch) return;
    startInputLoop();
  } catch (e) {
    if (epoch !== state.startEpoch) return;
    if (state.sessionId) {
      try {
        await api("/api/session/stop", { method: "POST", body: "{}" });
      } catch {}
    }
    await cleanupSession(false);
    showHomeNotice("Could not start demo", e.message);
  } finally {
    if (epoch === state.startEpoch) state.starting = false;
  }
}

function stopDetachedPoll() {
  if (detachedTimer) {
    clearInterval(detachedTimer);
    detachedTimer = null;
  }
}

function startDetachedPoll() {
  if (detachedTimer) return;
  detachedTimer = setInterval(async () => {
    try {
      const status = await api("/api/status");
      if (!status.active) {
        stopDetachedPoll();
        state.detachedActive = false;
        setBusy(false);
        setStatus("Idle", "");
        hideOverlay();
        showHome();
      }
    } catch (e) {
      if (e.status === 401) stopDetachedPoll();
    }
  }, 2500);
}

async function loadStatus() {
  try {
    const status = await api("/api/status");
    demoBtn.classList.toggle("hidden", !status.demo);
    if (status.active && !state.pc && !state.sessionId && !state.detachedActive) {
      state.detachedActive = true;
      setBusy(true);
      setStatus("Session active", "live");
      showPlayer();
      showOverlay(
        "Session already active",
        "A stream started in another tab is still running. Press Stop to release the console.",
      );
      showPlayerBar(false);
      startDetachedPoll();
    }
  } catch {}
}

async function setupPeer(offer, sessionId) {
  const pc = new RTCPeerConnection({ iceServers: [] });
  state.pc = pc;
  state.stream = new MediaStream();
  // The session can end (or be replaced) while any of these awaits are in
  // flight; never let stale callbacks touch the new session's state.
  const alive = () => state.pc === pc && state.sessionId === sessionId;
  const abandon = () => {
    try { pc.close(); } catch {}
  };
  pc.ontrack = (event) => {
    if (!alive() || !state.stream) return;
    state.stream.addTrack(event.track);
    video.srcObject = state.stream;
    applyAudioState();
    applyPicture();
    video.play().catch(() => {
      toast("Click the video to enable audio");
    });
  };
  pc.onconnectionstatechange = () => {
    if (state.pc !== pc || state.sessionId !== sessionId) return;
    const s = pc.connectionState;
    if (s === "connected") {
      clearIceRestart();
      state.iceAttempts = 0;
      state.iceGaveUp = false;
      hideOverlay();
      setStatus("Live", "live");
      state.streaming = true;
      hintOverlay.classList.add("hidden");
      syncTouchControlsUi();
      startMotionIfEnabled();
      if (state.touch.enabled && state.ui.touchCapable) {
        hidePlayerBar();
      } else {
        showPlayerBar();
      }
    } else if (s === "failed") {
      setStatus(s, "bad");
      showPlayerBar();
      scheduleIceRestart(pc, sessionId, 0);
    } else if (s === "disconnected") {
      setStatus(s, "bad");
      showPlayerBar();
      // A transient Wi-Fi blip often recovers by itself; give it a moment
      // before renegotiating fresh ICE credentials.
      scheduleIceRestart(pc, sessionId, 3000);
    } else if (s === "closed") {
      setStatus(s, "bad");
      showPlayerBar(false);
    } else {
      setStatus(s, "");
    }
  };
  await pc.setRemoteDescription({ type: "offer", sdp: offer.sdp });
  if (!alive()) { abandon(); return; }
  const answer = await pc.createAnswer();
  if (!alive()) { abandon(); return; }
  await pc.setLocalDescription(answer);
  if (!alive()) { abandon(); return; }
  await waitForIceGathering(pc);
  if (!alive()) { abandon(); return; }
  await api("/api/session/answer", {
    method: "POST",
    body: JSON.stringify({ session_id: sessionId, sdp: pc.localDescription.sdp }),
  });
  video.focus();
}

function waitForIceGathering(pc) {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const timeout = setTimeout(done, 3500);
    function done() {
      clearTimeout(timeout);
      pc.removeEventListener("icegatheringstatechange", check);
      resolve();
    }
    function check() {
      if (pc.iceGatheringState === "complete") done();
    }
    pc.addEventListener("icegatheringstatechange", check);
  });
}

function clearIceRestart() {
  if (state.iceTimer) {
    clearTimeout(state.iceTimer);
    state.iceTimer = null;
  }
  if (state.iceStopTimer) {
    clearTimeout(state.iceStopTimer);
    state.iceStopTimer = null;
  }
}

function scheduleIceAutoStop(sessionId) {
  if (state.iceStopTimer) return;
  state.iceStopTimer = setTimeout(() => {
    state.iceStopTimer = null;
    if (state.sessionId !== sessionId) return;
    toast("Video connection could not be recovered; ending the session.", true);
    stopSession();
  }, 1500);
}

function scheduleIceRestart(pc, sessionId, delay) {
  if (state.pc !== pc || state.sessionId !== sessionId || state.iceGaveUp) return;
  if (state.iceAttempts >= ICE_RESTART_MAX) {
    state.iceGaveUp = true;
    setStatus("Connection failed", "bad");
    toast("Could not recover the video connection; ending the session.", true);
    scheduleIceAutoStop(sessionId);
    return;
  }
  if (state.iceTimer || state.iceRestarting) return;
  state.iceTimer = setTimeout(() => {
    state.iceTimer = null;
    restartIce(pc, sessionId);
  }, delay);
}

async function restartIce(pc, sessionId) {
  if (state.pc !== pc || state.sessionId !== sessionId || state.iceRestarting) return;
  const alive = () => state.pc === pc && state.sessionId === sessionId;
  state.iceRestarting = true;
  // Count every renegotiation attempt, not only HTTP failures. A restart can
  // complete successfully while ICE still never connects; without this the
  // loop would retry until the server's 30 s orphan watchdog.
  state.iceAttempts += 1;
  let retry = false;
  setStatus("Reconnecting", "bad");
  try {
    const result = await api("/api/session/restart", {
      method: "POST",
      body: JSON.stringify({ session_id: sessionId }),
    });
    if (!alive()) return;
    await pc.setRemoteDescription({ type: "offer", sdp: result.offer.sdp });
    if (!alive()) return;
    const answer = await pc.createAnswer();
    if (!alive()) return;
    await pc.setLocalDescription(answer);
    if (!alive()) return;
    await waitForIceGathering(pc);
    if (!alive()) return;
    await api("/api/session/answer", {
      method: "POST",
      body: JSON.stringify({ session_id: sessionId, sdp: pc.localDescription.sdp }),
    });
  } catch (e) {
    if (!alive()) return;
    if (state.iceAttempts < ICE_RESTART_MAX) {
      retry = true;
    } else {
      state.iceGaveUp = true;
      toast(`Reconnect failed: ${e.message}; ending the session.`, true);
      setStatus("Connection failed", "bad");
      scheduleIceAutoStop(sessionId);
    }
  } finally {
    state.iceRestarting = false;
    if (retry && alive()) scheduleIceRestart(pc, sessionId, 5000);
  }
}

function connectSocket() {
  if (state.ws && (state.ws.readyState === WebSocket.OPEN || state.ws.readyState === WebSocket.CONNECTING)) {
    return;
  }
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${scheme}://${location.host}/api/session/ws`);
  state.ws = ws;
  ws.onopen = () => {
    state.wsRetryDelay = 500;
  };
  ws.onmessage = (event) => {
    let msg;
    try { msg = JSON.parse(event.data); } catch { return; }
    handleServerEvent(msg);
  };
  ws.onclose = () => {
    if (state.ws !== ws) return;
    state.ws = null;
    if (!state.sessionId) return;
    if (state.streaming) setStatus("Disconnected", "bad");
    scheduleSocketReconnect();
  };
}

function scheduleSocketReconnect() {
  if (state.wsRetry || !state.sessionId) return;
  const delay = state.wsRetryDelay;
  state.wsRetryDelay = Math.min(5000, delay * 2);
  state.wsRetry = setTimeout(() => {
    state.wsRetry = null;
    if (state.sessionId) connectSocket();
  }, delay);
}

function handleServerEvent(event) {
  switch (event.type) {
    case "session_started":
      state.serverSessionId = event.session_id;
      // Reconnecting the event socket does not change the WebRTC transport;
      // restore the live status instead of leaving "Disconnected" up.
      if (state.streaming && state.pc?.connectionState === "connected") {
        setStatus("Live", "live");
      }
      break;
    case "phase":
      setStatus(phaseLabel(event.phase), "");
      break;
    case "connected":
      setStatus("Connected", "live");
      break;
    case "pin_request":
      showPinModal(event.incorrect);
      break;
    case "stream_info": {
      const p = event.profiles && event.profiles[0];
      if (p) {
        overlayText.textContent = `${p.width} × ${p.height}`;
      }
      break;
    }
    case "rumble":
      vibrate(event.left, event.right);
      break;
    case "trigger_effects":
      applyTriggerEffects(event.left, event.right);
      break;
    case "stats":
      packetCount = { received: event.received, lost: event.lost };
      packetLossPct = Number.isFinite(event.loss_pct) ? event.loss_pct : null;
      consoleLatencyMs = Number.isFinite(event.latency_ms) ? event.latency_ms : null;
      renderStats();
      break;
    case "notice":
      toast(event.message, false, 7000);
      break;
    case "remote_disconnect":
      state.resumePending = { hostId: state.hostId, reason: event.reason };
      toast(`Console disconnected: ${event.reason}`, true);
      setStatus("Remote disconnected", "bad");
      break;
    case "error":
      if (event.message === "no active session" && state.sessionId) {
        cleanupSession(true);
        showHomeNotice("Session ended", "The console session is no longer active.");
      } else {
        toast(event.message, true);
      }
      break;
    case "quit": {
      // A teardown from an earlier session can arrive after a new one has
      // connected on the same global event channel. Ignore it.
      if (event.session_id) {
        if (state.serverSessionId && event.session_id !== state.serverSessionId) break;
        if (state.sessionId && event.session_id !== state.sessionId) break;
      }
      {
        const remote = String(event.reason || "").includes("Remote has disconnected");
        const pending = state.resumePending
          || (remote && state.hostId
            ? { hostId: state.hostId, reason: event.reason_string || event.reason }
            : null);
        state.resumePending = null;
        if (pending) {
          cleanupSession(false).then(() => {
            showPlayer();
            showResumePanel(pending);
          });
          break;
        }
      }
      cleanupSession(true);
      if (event.reason === "Stopped") {
        toast("Session stopped");
      } else {
        const detail = event.reason_string || event.reason || "Session ended";
        showHomeNotice("Session ended", detail);
      }
      break;
    }
    case "peer_state":
      if (["failed", "disconnected", "closed"].includes(event.state)) {
        setStatus(event.state, "bad");
      }
      break;
  }
}

function phaseLabel(phase) {
  return ({
    requesting_session: "Requesting session",
    starting_ctrl: "Connecting control",
    waiting_pin: "Waiting for PIN",
    starting_stream: "Starting stream",
    streaming: "Streaming",
    stopped: "Stopped",
  })[phase] || phase;
}

function showPinModal(incorrect) {
  pinModal.classList.remove("hidden");
  $("pin-text").textContent = incorrect
    ? "That PIN was incorrect. Enter the current PIN shown on your console."
    : "The console is asking for its login PIN. Enter the PIN shown on your console.";
  $("pin-input").value = "";
  loginPinReveal.reset();
  $("pin-input").focus();
}

$("pin-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const pin = $("pin-input").value.trim();
  if (!pin) return;
  sendWs({ type: "pin", pin });
  pinModal.classList.add("hidden");
  loginPinReveal.reset();
});

function cancelPin() {
  pinModal.classList.add("hidden");
  loginPinReveal.reset();
  stopSession();
}

$("pin-cancel").addEventListener("click", cancelPin);
$("pin-input").addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    cancelPin();
  }
});

async function stopSession() {
  state.resumePending = null;
  hideResumePanel();
  try {
    await api("/api/session/stop", { method: "POST", body: "{}" });
  } catch (e) {
    toast(e.message, true);
  }
  await cleanupSession(true);
}

async function cleanupSession(resetUi) {
  // Supersede any start that is still waiting for its HTTP response.
  state.startEpoch += 1;
  state.starting = false;
  state.streaming = false;
  state.detachedActive = false;
  state.serverSessionId = null;
  state.paused = false;
  state.hostId = null;
  state.resumePending = null;
  state.resumeTarget = null;
  closeTouchMenu({ restoreFocus: false });
  $("pause-btn").textContent = "Pause";
  $("resume-panel").classList.add("hidden");
  clearRumble();
  if (state.iceStopTimer) {
    clearTimeout(state.iceStopTimer);
    state.iceStopTimer = null;
  }
  stopDetachedPoll();
  state.keyboard.clear();
  state.mouse = { dx: 0, dy: 0, left: false, right: false, locked: false };
  state.touches.clear();
  resetTouchControls();
  stopMotionSensors();
  state.motion.recenterQuat = [0, 0, 0, 1];
  state.lastInputJson = "";
  state.lastInputAt = 0;
  pinModal.classList.add("hidden");
  loginPinReveal.reset();
  if (document.pointerLockElement) {
    ignorePromiseRejection(document.exitPointerLock());
  }
  // Leaving the stream should restore the page instead of leaving the home
  // screen stuck in fullscreen.
  if (document.fullscreenElement) {
    ignorePromiseRejection(document.exitFullscreen?.());
  }
  if (state.inputTimer) {
    clearInterval(state.inputTimer);
    state.inputTimer = null;
  }
  if (state.wsRetry) {
    clearTimeout(state.wsRetry);
    state.wsRetry = null;
  }
  clearIceRestart();
  state.iceRestarting = false;
  state.iceAttempts = 0;
  state.iceGaveUp = false;
  state.wsRetryDelay = 500;
  clearTriggerEffects();
  const ws = state.ws;
  state.ws = null;
  if (ws) {
    try { ws.close(); } catch {}
  }
  state.sessionId = null;
  const pc = state.pc;
  state.pc = null;
  if (pc) {
    try { pc.close(); } catch {}
  }
  state.stream = null;
  consoleLatencyMs = null;
  packetLossPct = null;
  packetCount = { received: 0, lost: 0 };
  state.activeBitrate = null;
  stopSharpening();
  video.srcObject = null;
  setBusy(false);
  hintOverlay.classList.add("hidden");
  hideOverlay();
  showHome();
  if (resetUi) {
    setStatus("Idle", "");
    statsEl.textContent = "";
  }
}

/* ── Input ─────────────────────────────────────────────────────────────── */

const KEYBOARD_PROFILES_KEY = "gamebridge.keyboardProfiles";

// Every remappable action. `bit` actions are buttons, `trigger` actions feed
// an analog trigger, and `axis` actions drive the left stick.
const KEYBOARD_ACTIONS = [
  { id: "cross", label: "Cross", bit: B.CROSS, defaultCode: "Space" },
  { id: "moon", label: "Moon", bit: B.MOON, defaultCode: "KeyJ" },
  { id: "box", label: "Box", bit: B.BOX, defaultCode: "KeyK" },
  { id: "pyramid", label: "Pyramid", bit: B.PYRAMID, defaultCode: "KeyL" },
  { id: "dpad_left", label: "D-pad left", bit: B.DPAD_LEFT, defaultCode: "ArrowLeft" },
  { id: "dpad_right", label: "D-pad right", bit: B.DPAD_RIGHT, defaultCode: "ArrowRight" },
  { id: "dpad_up", label: "D-pad up", bit: B.DPAD_UP, defaultCode: "ArrowUp" },
  { id: "dpad_down", label: "D-pad down", bit: B.DPAD_DOWN, defaultCode: "ArrowDown" },
  { id: "l1", label: "L1", bit: B.L1, defaultCode: "KeyQ" },
  { id: "r1", label: "R1", bit: B.R1, defaultCode: "KeyE" },
  { id: "l3", label: "L3", bit: B.L3, defaultCode: "KeyF" },
  { id: "r3", label: "R3", bit: B.R3, defaultCode: "KeyG" },
  { id: "options", label: "Options", bit: B.OPTIONS, defaultCode: "Enter" },
  { id: "share", label: "Share", bit: B.SHARE, defaultCode: "Backspace" },
  { id: "touchpad", label: "Touchpad", bit: B.TOUCHPAD, defaultCode: "Tab" },
  { id: "l2", label: "L2 trigger", trigger: "l2", defaultCode: "KeyZ" },
  { id: "r2", label: "R2 trigger", trigger: "r2", defaultCode: "KeyC" },
  { id: "stick_left", label: "Left stick left", axis: "lx", value: -32767, defaultCode: "KeyA" },
  { id: "stick_right", label: "Left stick right", axis: "lx", value: 32767, defaultCode: "KeyD" },
  { id: "stick_up", label: "Left stick up", axis: "ly", value: -32767, defaultCode: "KeyW" },
  { id: "stick_down", label: "Left stick down", axis: "ly", value: 32767, defaultCode: "KeyS" },
];

const KEY_LABELS = {
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "Page Up",
  PageDown: "Page Down",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  ShiftLeft: "Left Shift",
  ShiftRight: "Right Shift",
  ControlLeft: "Left Ctrl",
  ControlRight: "Right Ctrl",
  AltLeft: "Left Alt",
  AltRight: "Right Alt",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

function defaultKeyboardBindings() {
  return Object.fromEntries(KEYBOARD_ACTIONS.map((action) => [action.id, action.defaultCode]));
}

function sanitizeKeyboardBindings(raw) {
  const bindings = {};
  const used = new Set();
  for (const action of KEYBOARD_ACTIONS) {
    let code = raw && typeof raw[action.id] === "string" ? raw[action.id].trim() : action.defaultCode;
    if (!code || used.has(code)) code = "";
    if (code) used.add(code);
    bindings[action.id] = code;
  }
  return bindings;
}

function loadKeyboardProfiles() {
  const fallback = {
    current: "Default",
    profiles: { Default: defaultKeyboardBindings() },
  };
  let stored;
  try {
    stored = JSON.parse(localStorage.getItem(KEYBOARD_PROFILES_KEY) || "null");
  } catch {
    return fallback;
  }
  if (!stored || typeof stored !== "object" || !stored.profiles || typeof stored.profiles !== "object") {
    return fallback;
  }
  const profiles = {};
  for (const [name, raw] of Object.entries(stored.profiles)) {
    if (!name || ["__proto__", "prototype", "constructor"].includes(name)) continue;
    if (!raw || typeof raw !== "object") continue;
    profiles[name] = sanitizeKeyboardBindings(raw);
  }
  const names = Object.keys(profiles);
  if (!names.length) return fallback;
  const current = typeof stored.current === "string" && profiles[stored.current]
    ? stored.current
    : names[0];
  return { current, profiles };
}

const loadedKeyboardProfiles = loadKeyboardProfiles();
let keyboardProfiles = loadedKeyboardProfiles.profiles;
let keyboardProfile = loadedKeyboardProfiles.current;
let pendingRebind = null;
let rebindCleanup = null;

function currentKeyboardBindings() {
  if (!keyboardProfiles[keyboardProfile]) {
    keyboardProfile = Object.keys(keyboardProfiles)[0] || "Default";
    keyboardProfiles[keyboardProfile] ||= defaultKeyboardBindings();
  }
  return keyboardProfiles[keyboardProfile];
}

function saveKeyboardProfiles() {
  try {
    localStorage.setItem(KEYBOARD_PROFILES_KEY, JSON.stringify({
      version: 1,
      current: keyboardProfile,
      profiles: keyboardProfiles,
    }));
  } catch {}
}

function keyLabel(code) {
  if (!code) return "Unbound";
  if (KEY_LABELS[code]) return KEY_LABELS[code];
  let match = /^Key([A-Z])$/.exec(code);
  if (match) return match[1];
  match = /^Digit([0-9])$/.exec(code);
  if (match) return match[1];
  match = /^Numpad([0-9])$/.exec(code);
  if (match) return `Num ${match[1]}`;
  match = /^F([0-9]{1,2})$/.exec(code);
  if (match) return `F${match[1]}`;
  return code;
}

function bindingFor(actionId) {
  const bindings = currentKeyboardBindings();
  return typeof bindings[actionId] === "string" ? bindings[actionId] : "";
}

function isKeyboardBound(code) {
  const bindings = currentKeyboardBindings();
  return KEYBOARD_ACTIONS.some((action) => bindings[action.id] === code);
}

function setKeyboardBinding(actionId, code) {
  const bindings = currentKeyboardBindings();
  if (!(actionId in bindings)) return;
  if (code) {
    // A key can drive only one action. Moving it frees the previous owner.
    for (const action of KEYBOARD_ACTIONS) {
      if (action.id !== actionId && bindings[action.id] === code) {
        bindings[action.id] = "";
      }
    }
  }
  bindings[actionId] = code;
  state.keyboard.clear();
  saveKeyboardProfiles();
  renderKeyboardBindings();
}

function cancelRebind(render = true) {
  pendingRebind = null;
  if (rebindCleanup) {
    rebindCleanup();
    rebindCleanup = null;
  }
  if (render) renderKeyboardBindings();
}

function beginRebind(actionId) {
  cancelRebind(false);
  pendingRebind = actionId;
  renderKeyboardBindings();
  const handler = (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      cancelRebind();
      return;
    }
    if (event.metaKey || event.ctrlKey || event.altKey) {
      toast("Use a key without Ctrl, Alt, or Cmd", true);
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    setKeyboardBinding(actionId, event.code);
    cancelRebind();
  };
  window.addEventListener("keydown", handler, true);
  rebindCleanup = () => window.removeEventListener("keydown", handler, true);
}

function renderKeyboardBindings() {
  const select = $("kb-profile");
  if (!select) return;
  const bindings = currentKeyboardBindings();
  select.replaceChildren();
  for (const name of Object.keys(keyboardProfiles)) {
    const option = document.createElement("option");
    option.value = name;
    option.textContent = name;
    select.append(option);
  }
  select.value = keyboardProfile;
  $("kb-profile-delete").disabled = keyboardProfile === "Default";

  const list = $("keyboard-bindings");
  list.replaceChildren();
  for (const action of KEYBOARD_ACTIONS) {
    const row = document.createElement("div");
    row.className = "binding-row";

    const label = document.createElement("span");
    label.textContent = action.label;

    const key = document.createElement("button");
    key.type = "button";
    key.className = "binding-key";
    const code = bindings[action.id] || "";
    const listening = pendingRebind === action.id;
    key.classList.toggle("listening", listening);
    key.classList.toggle("unbound", !code);
    key.textContent = listening ? "Press a key…" : keyLabel(code);
    key.title = listening ? "Press a key, or Escape to cancel" : "Click to rebind";
    key.addEventListener("click", () => beginRebind(action.id));

    const clear = document.createElement("button");
    clear.type = "button";
    clear.className = "binding-clear";
    clear.textContent = "×";
    clear.title = "Clear binding";
    clear.disabled = !code;
    clear.setAttribute("aria-label", `Clear ${action.label} binding`);
    clear.addEventListener("click", () => setKeyboardBinding(action.id, ""));

    row.append(label, key, clear);
    list.append(row);
  }
}

function isEditableTarget(target) {
  return target instanceof HTMLElement
    && (target.isContentEditable
      || target.tagName === "INPUT"
      || target.tagName === "TEXTAREA"
      || target.tagName === "SELECT");
}

window.addEventListener("keydown", (event) => {
  // Controller keys are only meaningful while a stream is live, and must
  // never steal keys from the PIN modal or any text field (Backspace,
  // Space, Tab, Enter and the mapped letters must keep working).
  if (!state.streaming) return;
  if (settingsIsOpen()) return;
  if (!pinModal.classList.contains("hidden")) return;
  if (isEditableTarget(event.target)) return;
  if (
    event.code === "KeyM"
    && !isKeyboardBound("KeyM")
    && !event.ctrlKey
    && !event.metaKey
    && !event.altKey
  ) {
    event.preventDefault();
    toggleMute();
    return;
  }
  if (isKeyboardBound(event.code) || event.code === "Escape") {
    if (event.code !== "Escape") event.preventDefault();
  }
  state.keyboard.add(event.code);
});
window.addEventListener("keyup", (event) => {
  state.keyboard.delete(event.code);
});
window.addEventListener("blur", () => {
  state.keyboard.clear();
  resetTouchControls();
  if (state.streaming) sendInput(true);
});

document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    resetTouchControls();
    state.touches.clear();
    stopMotionSensors();
  } else if (state.streaming) {
    startMotionIfEnabled();
    sendInput(true);
  }
});

window.addEventListener("pagehide", () => {
  resetTouchControls();
  state.touches.clear();
  stopMotionSensors();
});

// Form controls keep all native editing keys (Backspace, Delete, arrows,
// Tab, Space, Enter). Stop these events before any gamepad shortcut sees
// them, independent of the streaming state. Escape still bubbles so it can
// dismiss the open drawer.
document.querySelectorAll("input, textarea, select").forEach((element) => {
  element.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") event.stopPropagation();
  });
});

function startInputLoop() {
  if (state.inputTimer) clearInterval(state.inputTimer);
  state.inputTimer = setInterval(sendInput, 16);
  startMotionIfEnabled();
}

function deadzone(value) {
  return Math.abs(value) < 0.08 ? 0 : value;
}

function finiteVector(values, length) {
  if (!values || values.length < length) return null;
  const out = [];
  for (let i = 0; i < length; i += 1) {
    const value = Number(values[i]);
    if (!Number.isFinite(value)) return null;
    out.push(value);
  }
  return out;
}

function gamepadMotion(pad) {
  const pose = pad?.pose;
  if (!pose) return null;
  const angular = finiteVector(pose.angularVelocity, 3);
  const accel = finiteVector(pose.linearAcceleration, 3);
  if (!angular || !accel) return null;
  let orientation = finiteVector(pose.orientation, 4) || [0, 0, 0, 1];
  const norm = Math.hypot(...orientation);
  if (!Number.isFinite(norm) || norm < 0.1) orientation = [0, 0, 0, 1];
  return {
    angular_velocity: angular,
    // FeedbackState expects acceleration in g; GamepadPose reports m/s².
    linear_acceleration: accel.map((value) => value / 9.80665),
    orientation,
  };
}

const touchControlsEl = $("touch-controls");
const leftStickEl = $("tc-stick-left");
const rightStickEl = $("tc-stick-right");
const dpadEl = $("tc-dpad");
const rotateHintEl = $("rotate-hint");
const touchMenuBtn = $("touch-menu-btn");
const touchMenuEl = $("touch-menu");
const touchMenuVolume = $("touch-menu-volume");
const portraitMedia = matchMedia("(orientation: portrait)");

function isTouchCapable() {
  return (navigator.maxTouchPoints || 0) > 0
    || (typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches);
}

function loadTouchControlsEnabled() {
  try {
    const stored = localStorage.getItem(TOUCH_CONTROLS_KEY);
    if (stored === "1" || stored === "0") return stored === "1";
  } catch {}
  return isTouchCapable();
}

function saveTouchControlsEnabled(enabled) {
  try {
    localStorage.setItem(TOUCH_CONTROLS_KEY, enabled ? "1" : "0");
  } catch {}
}

function touchMenuIsOpen() {
  return !touchMenuEl.classList.contains("hidden");
}

function updateTouchMenuState() {
  const status = $("conn-status");
  const menuState = $("touch-menu-state");
  menuState.textContent = status.textContent;
  menuState.className = "status-pill"
    + (status.classList.contains("live") ? " live" : "")
    + (status.classList.contains("bad") ? " bad" : "");
  $("touch-menu-stats").textContent = $("stats").textContent;
  touchMenuVolume.value = String(Math.round(audio.volume * 100));

  const labels = {
    pause: $("pause-btn").textContent,
    mute: audio.muted ? "Unmute" : "Mute",
    fullscreen: $("fullscreen-btn").textContent,
    controls: state.touch.enabled ? "Hide controls" : "Show controls",
  };
  for (const button of touchMenuEl.querySelectorAll("[data-menu-action]")) {
    const action = button.dataset.menuAction;
    if (labels[action]) button.textContent = labels[action];
    if (action === "controls") {
      button.disabled = false;
      continue;
    }
    const sourceId = TOUCH_MENU_SOURCE[action];
    button.disabled = Boolean(sourceId && $(sourceId)?.disabled);
  }
}

function openTouchMenu() {
  if (!state.streaming) return;
  resetTouchControls();
  sendInput(true);
  hidePlayerBar();
  updateTouchMenuState();
  touchMenuEl.classList.remove("hidden");
  touchMenuEl.setAttribute("aria-hidden", "false");
  touchMenuBtn.setAttribute("aria-expanded", "true");
  document.body.classList.add("touch-menu-open");
  $("touch-menu-close").focus({ preventScroll: true });
}

function closeTouchMenu({ restoreFocus = true } = {}) {
  if (!touchMenuIsOpen()) return;
  touchMenuEl.classList.add("hidden");
  touchMenuEl.setAttribute("aria-hidden", "true");
  touchMenuBtn.setAttribute("aria-expanded", "false");
  document.body.classList.remove("touch-menu-open");
  if (restoreFocus && !touchMenuBtn.classList.contains("hidden")) {
    touchMenuBtn.focus({ preventScroll: true });
  }
  if (state.streaming) sendInput(true);
}

function syncTouchMenuButton() {
  const visible = (state.ui.touchCapable || state.touch.enabled) && state.streaming;
  touchMenuBtn.classList.toggle("hidden", !visible);
  touchMenuBtn.setAttribute("aria-expanded", String(touchMenuIsOpen()));
  if (!visible && touchMenuIsOpen()) closeTouchMenu({ restoreFocus: false });
}

function syncTouchControlsUi() {
  const playerActive = document.body.classList.contains("player-active");
  const visible = state.touch.enabled && state.streaming && !state.ui.portrait;
  touchControlsEl.classList.toggle("hidden", !visible);
  touchControlsEl.classList.toggle("inactive", !state.streaming || state.paused);
  touchControlsEl.setAttribute("aria-hidden", String(!visible));
  // The player bar and toasts reposition themselves while the virtual
  // controller owns the bottom corners, so they never sit over a stick or
  // button and swallow its touch.
  document.body.classList.toggle("touch-controls-active", visible);

  const portraitHint = state.touch.enabled && playerActive && state.ui.portrait;
  document.body.classList.toggle("touch-portrait", portraitHint);
  rotateHintEl.classList.toggle("hidden", !portraitHint);

  const checkbox = $("opt-touch-controls");
  if (checkbox) checkbox.checked = state.touch.enabled;

  const toggle = $("touch-toggle-btn");
  if (toggle) {
    toggle.classList.toggle("hidden", !state.ui.touchCapable && !state.touch.enabled);
    toggle.setAttribute("aria-pressed", String(state.touch.enabled));
    toggle.textContent = state.touch.enabled ? "Hide controls" : "Show controls";
    toggle.title = state.touch.enabled ? "Hide on-screen touch controls" : "Show on-screen touch controls";
  }
  syncTouchMenuButton();
}

function setTouchControlsEnabled(enabled, persist = true) {
  state.touch.enabled = Boolean(enabled);
  if (persist) saveTouchControlsEnabled(state.touch.enabled);
  if (!state.touch.enabled) resetTouchControls();
  syncTouchControlsUi();
  updateOrientationState();
}

function virtualInputAllowed() {
  return state.touch.enabled && state.streaming && !state.paused
    && !settingsIsOpen() && !touchMenuIsOpen();
}

function touchButtons() {
  let mask = 0;
  for (const record of state.touch.pointers.values()) {
    if (record.kind === "button") mask |= TOUCH_BUTTON_BITS[record.action] || 0;
  }
  for (const key of state.touch.latches.keys()) {
    if (key.startsWith("button:")) mask |= TOUCH_BUTTON_BITS[key.slice(7)] || 0;
  }
  for (const direction of Object.keys(TOUCH_DPAD_BITS)) {
    if (state.touch.dpad[direction]) mask |= TOUCH_DPAD_BITS[direction];
  }
  return mask;
}

function touchTrigger(which) {
  for (const record of state.touch.pointers.values()) {
    if (record.kind === "trigger" && record.trigger === which) return 255;
  }
  return state.touch.latches.has(`trigger:${which}`) ? 255 : 0;
}

function vpadStick(slot) {
  const stick = state.touch[slot];
  return stick.active ? { x: stick.x, y: stick.y } : null;
}

function cancelTouchLatch(key) {
  const timer = state.touch.latches.get(key);
  if (timer) clearTimeout(timer);
  state.touch.latches.delete(key);
}

function scheduleTouchLatch(key, delay) {
  cancelTouchLatch(key);
  const timer = setTimeout(() => {
    state.touch.latches.delete(key);
    sendInput(true);
  }, Math.max(0, delay));
  state.touch.latches.set(key, timer);
}

function resetTouchControls() {
  for (const timer of state.touch.latches.values()) clearTimeout(timer);
  state.touch.latches.clear();
  for (const [pointerId, record] of state.touch.pointers) {
    record.element?.classList.remove("pressed");
    try { record.element?.releasePointerCapture(pointerId); } catch {}
  }
  state.touch.pointers.clear();

  for (const slot of ["left", "right"]) {
    const stick = state.touch[slot];
    const element = slot === "left" ? leftStickEl : rightStickEl;
    if (stick.pointerId !== null) {
      try { element?.releasePointerCapture(stick.pointerId); } catch {}
    }
    stick.active = false;
    stick.pointerId = null;
    stick.origin = null;
    stick.x = 0;
    stick.y = 0;
    const knob = (slot === "left" ? leftStickEl : rightStickEl)?.querySelector(".tc-stick-knob");
    if (knob) knob.style.transform = "translate(-50%, -50%)";
  }

  if (state.touch.dpad.pointerId !== null) {
    try { dpadEl.releasePointerCapture(state.touch.dpad.pointerId); } catch {}
  }
  state.touch.dpad.pointerId = null;
  state.touch.dpad.rect = null;
  state.touch.dpad.origin = null;
  state.touch.dpad.up = false;
  state.touch.dpad.down = false;
  state.touch.dpad.left = false;
  state.touch.dpad.right = false;
  if (dpadEl) {
    for (const button of dpadEl.querySelectorAll("[data-dpad]")) button.classList.remove("pressed");
  }

  for (const which of ["l2", "r2"]) {
    state.touch.triggers[which].active = false;
    state.touch.triggers[which].pointerId = null;
    state.touch.triggers[which].until = 0;
  }
  touchControlsEl?.querySelectorAll(".pressed").forEach((element) => element.classList.remove("pressed"));
}

function isLoopbackHost(hostname) {
  return hostname === "localhost"
    || hostname.endsWith(".localhost")
    || hostname === "127.0.0.1"
    || hostname === "[::1]"
    || hostname === "::1";
}

function isInsecureRemoteOrigin() {
  return location.protocol === "http:"
    && !isLoopbackHost(location.hostname)
    && window.isSecureContext === false;
}

function updateInsecureBanner() {
  const dismissed = (() => {
    try { return sessionStorage.getItem(INSECURE_DISMISS_KEY) === "1"; } catch { return false; }
  })();
  state.ui.insecure = isInsecureRemoteOrigin();
  $("insecure-banner").classList.toggle("hidden", !state.ui.insecure || dismissed);
}

function primeInsecureWarning() {
  updateInsecureBanner();
  if (!state.ui.insecure) return;
  let dismissed = false;
  try { dismissed = sessionStorage.getItem(INSECURE_DISMISS_KEY) === "1"; } catch {}
  if (!dismissed) {
    toast("Insecure HTTP connection: phones and tablets may block sensors, gamepads, or WebRTC. You can continue; use HTTPS or localhost for full support.", false, 8000);
  }
}

function updateOrientationState() {
  const portrait = portraitMedia.matches;
  const changed = state.ui.portrait !== portrait;
  state.ui.portrait = portrait;
  if (changed) resetTouchControls();
  syncTouchControlsUi();
  if (changed && state.streaming) sendInput(true);
}

function motionSupported() {
  return Boolean(MotionMath)
    && typeof DeviceMotionEvent !== "undefined"
    && typeof DeviceOrientationEvent !== "undefined";
}

function phoneSensorsAvailable() {
  return isTouchCapable() && motionSupported();
}

function motionNeedsPermission() {
  return (typeof DeviceMotionEvent !== "undefined"
      && typeof DeviceMotionEvent.requestPermission === "function")
    || (typeof DeviceOrientationEvent !== "undefined"
      && typeof DeviceOrientationEvent.requestPermission === "function");
}

function loadMotionEnabled() {
  try {
    const stored = localStorage.getItem(MOTION_KEY);
    if (stored === "1" || stored === "0") return stored === "1";
  } catch {}
  return true;
}

function startMotionIfEnabled() {
  if (!state.motion.enabled || !phoneSensorsAvailable() || !state.streaming) return;
  if (motionNeedsPermission() && state.motion.permission !== "granted") {
    if (state.motion.permission !== "denied") state.motion.permission = "prompt";
    updateMotionStatus();
    return;
  }
  if (state.motion.sensorsAttached) return;
  window.addEventListener("devicemotion", onDeviceMotion, { passive: true });
  window.addEventListener("deviceorientation", onDeviceOrientation, { passive: true });
  state.motion.sensorsAttached = true;
  state.motion.permission = "granted";
  updateMotionStatus();
}

function stopMotionSensors() {
  if (!state.motion.sensorsAttached) return;
  window.removeEventListener("devicemotion", onDeviceMotion);
  window.removeEventListener("deviceorientation", onDeviceOrientation);
  state.motion.sensorsAttached = false;
  state.motion.lastEventAt = 0;
  updateMotionStatus();
}

function requestMotionPermissionFromGesture() {
  if (!phoneSensorsAvailable()) {
    updateMotionStatus();
    return;
  }
  if (!motionSupported()) {
    state.motion.permission = "unsupported";
    toast("Motion sensors are not available in this browser", true);
    updateMotionStatus();
    return;
  }
  if (!motionNeedsPermission()) {
    state.motion.permission = "granted";
    startMotionIfEnabled();
    updateMotionStatus();
    return;
  }

  const requests = [];
  try {
    if (typeof DeviceMotionEvent.requestPermission === "function") {
      requests.push(DeviceMotionEvent.requestPermission());
    }
    if (typeof DeviceOrientationEvent.requestPermission === "function") {
      requests.push(DeviceOrientationEvent.requestPermission());
    }
  } catch {
    state.motion.permission = "denied";
    toast("Motion access was denied", true);
    updateMotionStatus();
    return;
  }

  Promise.all(requests).then((results) => {
    if (results.every((result) => result === "granted")) {
      state.motion.permission = "granted";
      startMotionIfEnabled();
      updateMotionStatus();
      toast("Motion sensors enabled");
    } else {
      state.motion.permission = "denied";
      updateMotionStatus();
      toast("Motion access was denied. Allow Motion & Orientation Access in browser settings, then try again.", true);
    }
  }).catch(() => {
    state.motion.permission = "denied";
    updateMotionStatus();
    toast("Motion access was denied or is unavailable on this page", true);
  });
}

function primeMotionForStart() {
  if (!state.motion.enabled || !phoneSensorsAvailable()) return;
  if (state.motion.permission === "granted"
      || state.motion.permission === "denied"
      || state.motion.permission === "unsupported") return;
  requestMotionPermissionFromGesture();
}

function currentScreenAngle() {
  const angle = screen.orientation?.angle;
  if (Number.isFinite(angle)) return angle;
  if (Number.isFinite(window.orientation)) {
    return (360 - window.orientation) % 360;
  }
  return 0;
}

function onDeviceMotion(event) {
  const now = performance.now();
  const wasStale = !state.motion.lastEventAt || now - state.motion.lastEventAt > MOTION_STALE_MS;
  state.motion.lastEventAt = now;
  const rate = event.rotationRate;
  state.motion.rotationRate = {
    alpha: Number.isFinite(rate?.alpha) ? rate.alpha : 0,
    beta: Number.isFinite(rate?.beta) ? rate.beta : 0,
    gamma: Number.isFinite(rate?.gamma) ? rate.gamma : 0,
  };
  const accel = event.accelerationIncludingGravity || event.acceleration;
  state.motion.acceleration = accel
    ? {
        x: Number.isFinite(accel.x) ? accel.x : 0,
        y: Number.isFinite(accel.y) ? accel.y : 9.80665,
        z: Number.isFinite(accel.z) ? accel.z : 0,
      }
    : { x: 0, y: 9.80665, z: 0 };
  if (wasStale) updateMotionStatus();
}

function onDeviceOrientation(event) {
  const now = performance.now();
  const wasStale = !state.motion.lastEventAt || now - state.motion.lastEventAt > MOTION_STALE_MS;
  state.motion.lastEventAt = now;
  state.motion.alpha = Number.isFinite(event.alpha) ? event.alpha : 0;
  state.motion.beta = Number.isFinite(event.beta) ? event.beta : 0;
  state.motion.gamma = Number.isFinite(event.gamma) ? event.gamma : 0;
  state.motion.hasOrientation = true;
  if (wasStale) updateMotionStatus();
}

function phoneMotion() {
  if (!phoneSensorsAvailable() || !state.motion.sensorsAttached || !MotionMath) return null;
  if (!state.motion.lastEventAt
      || performance.now() - state.motion.lastEventAt > MOTION_STALE_MS) return null;
  const angle = currentScreenAngle();
  const gyro = MotionMath.gyroDegPerSecToRadPerSec(state.motion.rotationRate, angle);
  const accel = MotionMath.accelMps2ToG(state.motion.acceleration, angle);
  let orientation = [0, 0, 0, 1];
  if (state.motion.hasOrientation) {
    const device = MotionMath.quatFromEulerZXY(
      state.motion.alpha,
      state.motion.beta,
      state.motion.gamma,
    );
    const current = MotionMath.quatMultiply(MotionMath.screenQuaternion(angle), device);
    orientation = MotionMath.quatNormalize(
      MotionMath.quatMultiply(state.motion.recenterQuat, current),
    );
  }
  if (gyro.some((value) => !Number.isFinite(value))
      || accel.some((value) => !Number.isFinite(value))
      || orientation.some((value) => !Number.isFinite(value))) {
    return null;
  }
  return {
    angular_velocity: gyro,
    linear_acceleration: accel,
    orientation,
  };
}

function sampleMotion(pad) {
  return phoneMotion() || gamepadMotion(pad);
}

function recenterMotion() {
  if (!state.motion.sensorsAttached || !state.motion.lastEventAt || !state.motion.hasOrientation) {
    toast("No motion orientation data yet", true);
    return;
  }
  const angle = currentScreenAngle();
  const device = MotionMath.quatFromEulerZXY(
    state.motion.alpha,
    state.motion.beta,
    state.motion.gamma,
  );
  const current = MotionMath.quatMultiply(MotionMath.screenQuaternion(angle), device);
  state.motion.recenterQuat = MotionMath.quatFromYaw(-MotionMath.quatYaw(current));
  toast("Motion recentered");
  updateMotionStatus();
}

function motionFresh() {
  return state.motion.sensorsAttached
    && state.motion.lastEventAt > 0
    && performance.now() - state.motion.lastEventAt <= MOTION_STALE_MS;
}

function updateMotionStatus() {
  const status = $("motion-status");
  if (!status) return;
  const enable = $("motion-enable-btn");
  const recenter = $("motion-recenter-btn");
  const secureHint = $("motion-secure-hint");
  const insecure = isInsecureRemoteOrigin();
  secureHint.classList.toggle("hidden", !insecure || !isTouchCapable());

  let text;
  if (!isTouchCapable()) {
    text = "Phone motion is available on touch devices; gamepad pose is used when available.";
  } else if (!motionSupported()) {
    text = "Motion sensors are not available in this browser.";
  } else if (!state.motion.enabled) {
    text = "Motion sensors are off.";
  } else if (state.motion.permission === "denied") {
    text = "Motion permission was denied. Allow Motion & Orientation Access in browser settings, then try again.";
  } else if (motionNeedsPermission() && state.motion.permission !== "granted") {
    text = "Sensor permission is required. Tap Enable sensors.";
  } else if (!state.motion.sensorsAttached) {
    text = "Ready when a stream starts.";
  } else if (!motionFresh()) {
    text = "Waiting for sensor data…";
  } else {
    text = "Sensors active.";
  }
  status.textContent = text;
  enable.classList.toggle(
    "hidden",
    !isTouchCapable() || !state.motion.enabled
      || !motionNeedsPermission() || state.motion.permission === "granted",
  );
  const canRecenter = isTouchCapable()
    && state.motion.enabled && motionFresh() && state.motion.hasOrientation;
  recenter.disabled = !canRecenter;
  const overlayRecenter = $("tc-recenter");
  overlayRecenter.disabled = !canRecenter;
  overlayRecenter.classList.toggle("hidden", !isTouchCapable() || !state.motion.enabled);
}

function sendInput(force = false) {
  if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return;
  const pad = activeGamepad();
  const buttons = keyboardButtons() | gamepadButtons() | touchButtons();
  const l2 = Math.max(
    touchTrigger("l2"),
    keyboardTrigger("l2"),
    mouseTrigger("right"),
    pad ? Math.round((pad.buttons[6]?.value || 0) * 255) : 0,
  );
  const r2 = Math.max(
    touchTrigger("r2"),
    keyboardTrigger("r2"),
    mouseTrigger("left"),
    pad ? Math.round((pad.buttons[7]?.value || 0) * 255) : 0,
  );
  let lx = 0, ly = 0, rx = 0, ry = 0;
  if (pad) {
    lx = axisToI16(deadzone(pad.axes[0] || 0));
    ly = axisToI16(deadzone(pad.axes[1] || 0));
    rx = axisToI16(deadzone(pad.axes[2] || 0));
    ry = axisToI16(deadzone(pad.axes[3] || 0));
  }
  const keyboardLx = keyboardAxis("lx");
  const keyboardLy = keyboardAxis("ly");
  if (keyboardLx !== null) lx = keyboardLx;
  if (keyboardLy !== null) ly = keyboardLy;

  if (state.mouse.locked) {
    rx = clampI16(rx + state.mouse.dx * 700);
    ry = clampI16(ry + state.mouse.dy * 700);
    state.mouse.dx *= 0.55;
    state.mouse.dy *= 0.55;
    if (Math.abs(state.mouse.dx) < 0.01) state.mouse.dx = 0;
    if (Math.abs(state.mouse.dy) < 0.01) state.mouse.dy = 0;
  }

  // Touch sticks are authoritative while their pointer is active:
  // left touch > keyboard > gamepad; right touch > mouse-look > gamepad.
  const touchLeft = vpadStick("left");
  if (touchLeft) { lx = touchLeft.x; ly = touchLeft.y; }
  const touchRight = vpadStick("right");
  if (touchRight) { rx = touchRight.x; ry = touchRight.y; }

  const touches = [...state.touches.values()].map((t) => ({
    id: t.id, x: t.x, y: t.y,
  }));
  while (touches.length < 2) touches.push({ id: -1, x: 0, y: 0 });

  const payload = {
    type: "input",
    buttons,
    l2, r2,
    lx, ly, rx, ry,
    touches,
  };
  if (state.motion.enabled) {
    const motion = sampleMotion(pad);
    if (motion) payload.motion = motion;
  }
  const json = JSON.stringify(payload);
  const now = performance.now();
  if (!force && json === state.lastInputJson && now - state.lastInputAt < 100) return;
  state.lastInputJson = json;
  state.lastInputAt = now;
  sendWs(payload);
}

function sendWs(payload) {
  if (state.ws && state.ws.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify(payload));
    return true;
  }
  return false;
}

function activeGamepad() {
  const pads = navigator.getGamepads ? navigator.getGamepads() : [];
  for (const pad of pads) {
    if (pad && pad.connected) return pad;
  }
  return null;
}

function gamepadButtons() {
  const pad = activeGamepad();
  if (!pad) return 0;
  let mask = 0;
  const map = [
    [0, B.CROSS], [1, B.MOON], [2, B.BOX], [3, B.PYRAMID],
    [4, B.L1], [5, B.R1], [8, B.SHARE], [9, B.OPTIONS],
    [10, B.L3], [11, B.R3], [12, B.DPAD_UP], [13, B.DPAD_DOWN],
    [14, B.DPAD_LEFT], [15, B.DPAD_RIGHT],
  ];
  for (const [index, bit] of map) {
    if (pad.buttons[index] && pad.buttons[index].pressed) mask |= bit;
  }
  if (pad.buttons[16] && pad.buttons[16].pressed) mask |= B.PS;
  return mask;
}

function keyboardButtons() {
  const bindings = currentKeyboardBindings();
  let mask = 0;
  for (const action of KEYBOARD_ACTIONS) {
    if (!action.bit) continue;
    const code = bindings[action.id];
    if (code && state.keyboard.has(code)) mask |= action.bit;
  }
  return mask;
}

function keyboardTrigger(actionId) {
  const code = bindingFor(actionId);
  return code && state.keyboard.has(code) ? 255 : 0;
}

function keyboardAxis(axis) {
  const bindings = currentKeyboardBindings();
  for (const action of KEYBOARD_ACTIONS) {
    if (action.axis !== axis) continue;
    const code = bindings[action.id];
    if (code && state.keyboard.has(code)) return action.value;
  }
  return null;
}

function mouseTrigger(button) {
  if (state.mouse.locked && state.mouse[button]) return 255;
  return 0;
}

function axisToI16(value) {
  return Math.round(Math.max(-1, Math.min(1, value)) * 32767);
}

function clampI16(value) {
  return Math.max(-32768, Math.min(32767, Math.round(value)));
}

/* Mouse capture and touchpad */

// Pointer lock and fullscreen return promises in current browsers and reject
// when the gesture is not trusted or the document is not focused. Those are
// expected; never leave an unhandled rejection behind.
function ignorePromiseRejection(result) {
  if (result && typeof result.catch === "function") result.catch(() => {});
}

function captureScreenshot() {
  if (!video.srcObject || !video.videoWidth || !video.videoHeight) {
    toast("No video frame to capture", true);
    return;
  }
  const canvas = document.createElement("canvas");
  canvas.width = video.videoWidth;
  canvas.height = video.videoHeight;
  const context = canvas.getContext("2d");
  if (!context) {
    toast("Could not capture the screenshot", true);
    return;
  }
  try {
    context.drawImage(video, 0, 0, canvas.width, canvas.height);
  } catch {
    toast("Could not capture the screenshot", true);
    return;
  }
  canvas.toBlob((blob) => {
    if (!blob) {
      toast("Could not encode the screenshot", true);
      return;
    }
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = `gamebridge-${new Date().toISOString().replace(/[:.]/g, "-")}.png`;
    document.body.append(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    toast("Screenshot saved");
  }, "image/png");
}

function setPaused(paused) {
  if (paused) {
    state.paused = true;
    resetTouchControls();
    if (state.streaming) sendInput(true);
    syncTouchControlsUi();
    // Keep the media element playing behind the overlay. Pausing it can stop
    // the browser from draining WebRTC, which backs up the server's video
    // queue and produces IDR churn. Mute and cover the view instead.
    applyAudioState();
    $("pause-btn").textContent = "Resume";
    setStatus("Paused", "");
    showOverlay("Paused", "The console keeps running. Press Resume to return to live video.");
    return;
  }
  state.paused = false;
  applyAudioState();
  $("pause-btn").textContent = "Pause";
  hideOverlay();
  syncTouchControlsUi();
  setStatus(state.streaming ? "Live" : "Connected", state.streaming ? "live" : "");
  video.play().catch(() => {
    toast("Click the video to resume playback", true);
  });
}

function togglePause() {
  if (!video.srcObject || !state.sessionId) return;
  setPaused(!state.paused);
}

$("screenshot-btn").addEventListener("click", captureScreenshot);
$("pause-btn").addEventListener("click", togglePause);

video.addEventListener("click", () => {
  if (!video.srcObject) return;
  if (state.paused) {
    setPaused(false);
    return;
  }
  if (document.pointerLockElement !== video) {
    ignorePromiseRejection(video.requestPointerLock?.());
  }
  video.play().catch(() => {});
});

document.addEventListener("pointerlockchange", () => {
  state.mouse.locked = document.pointerLockElement === video;
  hintOverlay.classList.toggle("hidden", !state.mouse.locked);
});

document.addEventListener("mousemove", (event) => {
  if (!state.mouse.locked) return;
  state.mouse.dx += event.movementX;
  state.mouse.dy += event.movementY;
});

document.addEventListener("mousedown", (event) => {
  if (!state.mouse.locked) return;
  if (event.button === 0) state.mouse.left = true;
  if (event.button === 2) state.mouse.right = true;
});
document.addEventListener("mouseup", (event) => {
  if (event.button === 0) state.mouse.left = false;
  if (event.button === 2) state.mouse.right = false;
});
video.addEventListener("contextmenu", (event) => event.preventDefault());

function videoContentRect() {
  const rect = video.getBoundingClientRect();
  const vw = video.videoWidth;
  const vh = video.videoHeight;
  if (!vw || !vh) return rect;
  // `object-fit: contain` may letterbox the frame inside the element.
  const scale = Math.min(rect.width / vw, rect.height / vh);
  const width = vw * scale;
  const height = vh * scale;
  return {
    left: rect.left + (rect.width - width) / 2,
    top: rect.top + (rect.height - height) / 2,
    width,
    height,
  };
}

function touchpadPoint(event) {
  const rect = videoContentRect();
  const x = Math.round(((event.clientX - rect.left) / rect.width) * 1920);
  const y = Math.round(((event.clientY - rect.top) / rect.height) * 942);
  return {
    x: Math.max(0, Math.min(1920, x)),
    y: Math.max(0, Math.min(942, y)),
  };
}

video.addEventListener("pointerdown", (event) => {
  if (event.pointerType !== "touch") return;
  event.preventDefault();
  if (state.touches.size >= 2) return;
  const id = state.touchSeq++ & 0x7f;
  const point = touchpadPoint(event);
  state.touches.set(event.pointerId, { id, ...point });
  try { video.setPointerCapture?.(event.pointerId); } catch {}
});
video.addEventListener("pointermove", (event) => {
  if (event.pointerType !== "touch") return;
  const touch = state.touches.get(event.pointerId);
  if (!touch) return;
  event.preventDefault();
  Object.assign(touch, touchpadPoint(event));
});
function endTouch(event) {
  if (event.pointerType !== "touch") return;
  state.touches.delete(event.pointerId);
}
video.addEventListener("pointerup", endTouch);
video.addEventListener("pointercancel", endTouch);
video.addEventListener("lostpointercapture", endTouch);

/* On-screen controller */

const dpadButtonEls = {
  up: dpadEl.querySelector('[data-dpad="up"]'),
  down: dpadEl.querySelector('[data-dpad="down"]'),
  left: dpadEl.querySelector('[data-dpad="left"]'),
  right: dpadEl.querySelector('[data-dpad="right"]'),
};

function releaseTouchRecord(pointerId) {
  const record = state.touch.pointers.get(pointerId);
  if (!record || (record.kind !== "button" && record.kind !== "trigger")) return;
  state.touch.pointers.delete(pointerId);
  record.element?.classList.remove("pressed");

  const key = record.kind === "button"
    ? `button:${record.action}`
    : `trigger:${record.trigger}`;
  const stillHeld = [...state.touch.pointers.values()].some((other) =>
    record.kind === "button"
      ? other.kind === "button" && other.action === record.action
      : other.kind === "trigger" && other.trigger === record.trigger
  );
  if (!stillHeld) {
    const held = performance.now() - record.pressedAt;
    if (held < TOUCH_TAP_MIN_MS) scheduleTouchLatch(key, TOUCH_TAP_MIN_MS - held);
  }
  sendInput(true);
}

function onTouchButtonDown(event) {
  if (!virtualInputAllowed()) return;
  const action = event.currentTarget.dataset.button;
  const trigger = event.currentTarget.dataset.trigger;
  if (!action && !trigger) return;
  if (action && !TOUCH_BUTTON_BITS[action]) return;
  if (state.touch.pointers.has(event.pointerId)) return;

  event.preventDefault();
  event.stopPropagation();
  cancelTouchLatch(action ? `button:${action}` : `trigger:${trigger}`);
  state.touch.pointers.set(event.pointerId, {
    kind: action ? "button" : "trigger",
    action,
    trigger,
    element: event.currentTarget,
    pressedAt: performance.now(),
  });
  event.currentTarget.classList.add("pressed");
  try { event.currentTarget.setPointerCapture(event.pointerId); } catch {}
  sendInput(true);
}

function onTouchButtonEnd(event) {
  const record = state.touch.pointers.get(event.pointerId);
  if (!record || (record.kind !== "button" && record.kind !== "trigger")) return;
  event.preventDefault();
  event.stopPropagation();
  releaseTouchRecord(event.pointerId);
}

function stickSlotFor(element) {
  return element === leftStickEl ? state.touch.left : state.touch.right;
}

// A touch origin keeps the control-local position captured at pointerdown.
// `offsetX/offsetY` is relative to the stick itself and remains usable when
// Android Chrome's fullscreen visual viewport makes clientX/clientY report a
// different origin. If a browser leaves offset coordinates frozen, drag
// deltas from the original client position take over instead.
function touchOffsetPoint(element, event) {
  if (event.target !== element) return null;
  const x = Number(event.offsetX);
  const y = Number(event.offsetY);
  if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
  // offsetX/Y are relative to the padding edge; the stick rect includes its
  // border. Align both coordinate spaces before comparing them.
  return { x: x + element.clientLeft, y: y + element.clientTop };
}

function pointInsideRect(point, rect) {
  return point.x >= -0.5 && point.y >= -0.5
    && point.x <= rect.width + 0.5 && point.y <= rect.height + 0.5;
}

function beginTouchOrigin(element, event) {
  const rect = element.getBoundingClientRect();
  const offset = touchOffsetPoint(element, event);
  const clientLocalX = event.clientX - rect.left;
  const clientLocalY = event.clientY - rect.top;
  const clientLocal = { x: clientLocalX, y: clientLocalY };
  const offsetUsable = offset && pointInsideRect(offset, rect);
  const clientUsable = pointInsideRect(clientLocal, rect);
  // Normal case: the client and target-local spaces agree, so use the exact
  // client position. If the client position falls outside a control we are
  // clearly touching, the browser has reported it in another viewport space
  // (Android fullscreen); use the target-local position instead.
  const local = (!clientUsable && offsetUsable) ? offset : clientLocal;
  return {
    rect,
    clientX: event.clientX,
    clientY: event.clientY,
    clientLocalX,
    clientLocalY,
    offsetX: offset?.x ?? 0,
    offsetY: offset?.y ?? 0,
    x: local.x,
    y: local.y,
  };
}

function localPointForEvent(element, origin, event) {
  const rect = origin.rect;
  const offset = touchOffsetPoint(element, event);
  const clientLocal = {
    x: event.clientX - rect.left,
    y: event.clientY - rect.top,
  };
  if (offset) {
    const clientMoved = Math.hypot(
      event.clientX - origin.clientX,
      event.clientY - origin.clientY,
    );
    const offsetMoved = Math.hypot(
      offset.x - origin.offsetX,
      offset.y - origin.offsetY,
    );
    // Prefer target-local coordinates when client coordinates are frozen.
    // When both move and both are inside the control, the client position is
    // exact; otherwise target-local coordinates avoid the viewport offset.
    if (offsetMoved >= 0.5 || clientMoved < 0.5) {
      const offsetUsable = pointInsideRect(offset, rect);
      const clientUsable = pointInsideRect(clientLocal, rect);
      if (offsetUsable && !clientUsable) return offset;
      if (!offsetUsable && clientUsable) return clientLocal;
      if (offsetUsable && clientUsable) {
        return clientMoved >= 0.5 ? clientLocal : offset;
      }
    }
  }
  return {
    x: origin.clientLocalX + (event.clientX - origin.clientX),
    y: origin.clientLocalY + (event.clientY - origin.clientY),
  };
}

function updateStickFromLocal(element, slot, localX, localY, rect) {
  const radius = rect.width * 0.32;
  if (!rect.width || !rect.height) return;
  let dx = (localX - rect.width / 2) / radius;
  let dy = (localY - rect.height / 2) / radius;
  const length = Math.hypot(dx, dy);
  if (length > 1) { dx /= length; dy /= length; }
  if (Math.hypot(dx, dy) < 0.12) { dx = 0; dy = 0; }
  slot.x = Math.round(dx * 32767);
  slot.y = Math.round(dy * 32767);
  const knob = element.querySelector(".tc-stick-knob");
  if (knob) {
    const travel = rect.width * 0.30;
    knob.style.transform = `translate(calc(-50% + ${dx * travel}px), calc(-50% + ${dy * travel}px))`;
  }
}

function onStickDown(event) {
  if (!virtualInputAllowed()) return;
  const slot = stickSlotFor(event.currentTarget);
  if (slot.pointerId !== null) return;
  event.preventDefault();
  event.stopPropagation();
  slot.pointerId = event.pointerId;
  slot.active = true;
  slot.origin = beginTouchOrigin(event.currentTarget, event);
  try { event.currentTarget.setPointerCapture(event.pointerId); } catch {}
  updateStickFromLocal(
    event.currentTarget, slot, slot.origin.x, slot.origin.y, slot.origin.rect,
  );
  sendInput(true);
}

const touchStickElements = [["left", leftStickEl], ["right", rightStickEl]];

function resetStick(slot, element) {
  slot.pointerId = null;
  slot.active = false;
  slot.origin = null;
  slot.x = 0;
  slot.y = 0;
  const knob = element?.querySelector(".tc-stick-knob");
  if (knob) knob.style.transform = "translate(-50%, -50%)";
}

function endStickForPointer(pointerId) {
  let changed = false;
  for (const [name, element] of touchStickElements) {
    const slot = state.touch[name];
    if (slot.pointerId !== pointerId) continue;
    try { element.releasePointerCapture(pointerId); } catch {}
    resetStick(slot, element);
    changed = true;
  }
  return changed;
}

function onStickMove(event) {
  let moved = false;
  for (const [name, element] of touchStickElements) {
    const slot = state.touch[name];
    if (slot.pointerId !== event.pointerId || !slot.origin) continue;
    const point = localPointForEvent(element, slot.origin, event);
    updateStickFromLocal(element, slot, point.x, point.y, slot.origin.rect);
    moved = true;
  }
  if (moved) event.preventDefault();
}

function onStickEnd(event) {
  if (!endStickForPointer(event.pointerId)) return;
  event.preventDefault();
  sendInput(true);
}

function setDpadBits(up, down, left, right, notify = true) {
  const dpad = state.touch.dpad;
  const changed = dpad.up !== up || dpad.down !== down || dpad.left !== left || dpad.right !== right;
  dpad.up = up;
  dpad.down = down;
  dpad.left = left;
  dpad.right = right;
  dpadButtonEls.up?.classList.toggle("pressed", up);
  dpadButtonEls.down?.classList.toggle("pressed", down);
  dpadButtonEls.left?.classList.toggle("pressed", left);
  dpadButtonEls.right?.classList.toggle("pressed", right);
  if (notify && changed && state.streaming) sendInput(true);
}

function updateDpadFromLocal(localX, localY, rect) {
  if (!rect) return;
  const dx = localX - rect.width / 2;
  const dy = localY - rect.height / 2;
  if (Math.hypot(dx, dy) < rect.width * 0.18) {
    setDpadBits(false, false, false, false);
    return;
  }
  const angle = (Math.atan2(dy, dx) * 180 / Math.PI + 360) % 360;
  const sector = Math.round(angle / 45) % 8;
  setDpadBits(
    sector === 5 || sector === 6 || sector === 7,
    sector === 1 || sector === 2 || sector === 3,
    sector === 3 || sector === 4 || sector === 5,
    sector === 7 || sector === 0 || sector === 1,
  );
}

function onDpadDown(event) {
  if (!virtualInputAllowed()) return;
  if (state.touch.dpad.pointerId !== null) return;
  event.preventDefault();
  event.stopPropagation();
  const origin = beginTouchOrigin(dpadEl, event);
  state.touch.dpad.pointerId = event.pointerId;
  state.touch.dpad.rect = origin.rect;
  state.touch.dpad.origin = origin;
  try { dpadEl.setPointerCapture(event.pointerId); } catch {}
  updateDpadFromLocal(origin.x, origin.y, origin.rect);
  sendInput(true);
}

function onDpadMove(event) {
  if (state.touch.dpad.pointerId !== event.pointerId) return;
  const origin = state.touch.dpad.origin;
  if (!origin) return;
  event.preventDefault();
  const point = localPointForEvent(dpadEl, origin, event);
  updateDpadFromLocal(point.x, point.y, origin.rect);
}

function endDpadForPointer(pointerId) {
  if (state.touch.dpad.pointerId !== pointerId) return false;
  try { dpadEl.releasePointerCapture(pointerId); } catch {}
  state.touch.dpad.pointerId = null;
  state.touch.dpad.rect = null;
  state.touch.dpad.origin = null;
  setDpadBits(false, false, false, false);
  return true;
}

function onDpadEnd(event) {
  if (!endDpadForPointer(event.pointerId)) return;
  event.preventDefault();
  sendInput(true);
}

// Pointer capture is the primary tracking mechanism, but some mobile
// browsers drop capture (or never grant it) when a finger leaves the control.
// Document-level fallbacks keep the stick following the finger and, more
// importantly, guarantee that a release cannot leave a button or axis stuck.
function onDocumentPointerMove(event) {
  onStickMove(event);
  onDpadMove(event);
}

function onDocumentPointerEnd(event) {
  const stickChanged = endStickForPointer(event.pointerId);
  const dpadChanged = endDpadForPointer(event.pointerId);
  if (state.touch.pointers.has(event.pointerId)) releaseTouchRecord(event.pointerId);
  // The video's own listeners cannot see a release once pointer capture has
  // been lost. Clear touchpad contacts here so a dropped capture cannot leave
  // a phantom finger held down for the rest of the session.
  state.touches.delete(event.pointerId);
  if (stickChanged || dpadChanged) sendInput(true);
}

function initTouchInput() {
  state.ui.touchCapable = isTouchCapable();
  state.touch.enabled = loadTouchControlsEnabled();

  for (const element of touchControlsEl.querySelectorAll("[data-button], [data-trigger]")) {
    element.addEventListener("pointerdown", onTouchButtonDown);
    element.addEventListener("pointerup", onTouchButtonEnd);
    element.addEventListener("pointercancel", onTouchButtonEnd);
    element.addEventListener("lostpointercapture", onTouchButtonEnd);
  }
  for (const element of [leftStickEl, rightStickEl]) {
    element.addEventListener("pointerdown", onStickDown);
    element.addEventListener("lostpointercapture", onStickEnd);
  }
  dpadEl.addEventListener("pointerdown", onDpadDown);
  dpadEl.addEventListener("lostpointercapture", onDpadEnd);
  document.addEventListener("pointermove", onDocumentPointerMove, { passive: false });
  document.addEventListener("pointerup", onDocumentPointerEnd);
  document.addEventListener("pointercancel", onDocumentPointerEnd);
  touchControlsEl.addEventListener("contextmenu", (event) => event.preventDefault());
  $("tc-recenter").addEventListener("click", recenterMotion);
  syncTouchControlsUi();
}

/* Rumble */

function fireRumble() {
  const actuator = activeGamepad()?.vibrationActuator;
  if (!actuator) return;
  try {
    ignorePromiseRejection(actuator.playEffect("dual-rumble", {
      // The console sends motor states, not durations. Hold the state with
      // short overlapping effects until a zero update or cleanup stops it.
      duration: 160,
      strongMagnitude: Math.min(1, state.rumble.left / 255),
      weakMagnitude: Math.min(1, state.rumble.right / 255),
    }));
  } catch {}
}

function vibrate(left, right) {
  state.rumble = { left: left || 0, right: right || 0 };
  if (!state.rumble.left && !state.rumble.right) {
    clearRumble();
    return;
  }
  fireRumble();
  if (!state.rumbleTimer) {
    state.rumbleTimer = setInterval(fireRumble, 100);
  }
}

function clearRumble() {
  if (state.rumbleTimer) {
    clearInterval(state.rumbleTimer);
    state.rumbleTimer = null;
  }
  state.rumble = { left: 0, right: 0 };
  const actuator = activeGamepad()?.vibrationActuator;
  if (!actuator) return;
  try {
    // Do not call reset(): it would also cancel adaptive-trigger effects.
    ignorePromiseRejection(actuator.playEffect("dual-rumble", {
      duration: 1,
      strongMagnitude: 0,
      weakMagnitude: 0,
    }));
  } catch {}
}

// Chrome exposes adaptive-trigger packets to the console, but the Gamepad API
// only gives us a plain trigger-rumble intensity. Map the raw effect strength
// to that approximation; full DualSense effect modes would need WebHID.
function triggerIntensity(data) {
  if (!Array.isArray(data)) return 0;
  let max = 0;
  for (const value of data) {
    if (Number.isFinite(value)) max = Math.max(max, value);
  }
  return Math.min(1, max / 255);
}

function applyTriggerEffects(leftData, rightData) {
  const left = triggerIntensity(leftData);
  const right = triggerIntensity(rightData);
  state.triggerEffect = { left, right };
  if (state.triggerEffectTimer) {
    clearInterval(state.triggerEffectTimer);
    state.triggerEffectTimer = null;
  }
  const pad = activeGamepad();
  const actuator = pad?.vibrationActuator;
  if (!actuator?.effects?.includes("trigger-rumble")) return;
  if (!left && !right) {
    clearTriggerEffects();
    return;
  }
  const fire = () => {
    const current = activeGamepad()?.vibrationActuator;
    if (!current?.effects?.includes("trigger-rumble")) return;
    try {
      ignorePromiseRejection(current.playEffect("trigger-rumble", {
        duration: 700,
        leftTrigger: state.triggerEffect.left,
        rightTrigger: state.triggerEffect.right,
      }));
    } catch {}
  };
  fire();
  // The effect is stateful on the console side. Re-issue it while active so a
  // static trigger resistance does not expire between console updates.
  state.triggerEffectTimer = setInterval(fire, 500);
}

function clearTriggerEffects() {
  if (state.triggerEffectTimer) {
    clearInterval(state.triggerEffectTimer);
    state.triggerEffectTimer = null;
  }
  state.triggerEffect = { left: 0, right: 0 };
  const actuator = activeGamepad()?.vibrationActuator;
  if (!actuator) return;
  try {
    if (typeof actuator.reset === "function") {
      ignorePromiseRejection(actuator.reset());
    } else {
      ignorePromiseRejection(actuator.playEffect("trigger-rumble", {
        duration: 100,
        leftTrigger: 0,
        rightTrigger: 0,
      }));
    }
  } catch {}
}

/* Buttons */

$("discover-btn").addEventListener("click", () => discover());
toastEl.addEventListener("click", dismissToast);
$("add-console-btn").addEventListener("click", openAddConsole);
$("add-close").addEventListener("click", () => closeAddConsole());
$("add-cancel").addEventListener("click", () => closeAddConsole());
$("host-help-btn").addEventListener("click", () => toggleFieldHelp("host"));
$("pin-help-btn").addEventListener("click", () => toggleFieldHelp("pin"));
$("settings-home-btn").addEventListener("click", () => openSettings("stream"));
$("settings-btn").addEventListener("click", () => openSettings("stream"));
$("settings-close").addEventListener("click", closeSettings);
$("settings-done").addEventListener("click", closeSettings);
$("settings-scrim").addEventListener("click", closeSettings);
$("settings-stream-tab").addEventListener("click", () => setSettingsTab("stream"));
$("settings-picture-tab").addEventListener("click", () => setSettingsTab("picture"));
$("settings-input-tab").addEventListener("click", () => {
  setSettingsTab("input");
  loadMacPsSetting();
  syncTouchControlsUi();
  updateMotionStatus();
});
$("opt-touch-controls").addEventListener("change", (event) => {
  setTouchControlsEnabled(event.target.checked);
});
$("touch-toggle-btn").addEventListener("click", () => {
  setTouchControlsEnabled(!state.touch.enabled);
  toast(state.touch.enabled ? "On-screen controls shown" : "On-screen controls hidden");
  showPlayerBar();
});
$("opt-motion").addEventListener("change", (event) => {
  state.motion.enabled = event.target.checked;
  try { localStorage.setItem(MOTION_KEY, state.motion.enabled ? "1" : "0"); } catch {}
  if (!state.motion.enabled) {
    stopMotionSensors();
    updateMotionStatus();
    return;
  }
  requestMotionPermissionFromGesture();
});
$("motion-enable-btn").addEventListener("click", () => {
  if (!state.motion.enabled) {
    state.motion.enabled = true;
    $("opt-motion").checked = true;
    try { localStorage.setItem(MOTION_KEY, "1"); } catch {}
  }
  requestMotionPermissionFromGesture();
});
$("motion-recenter-btn").addEventListener("click", recenterMotion);
$("insecure-dismiss").addEventListener("click", () => {
  try { sessionStorage.setItem(INSECURE_DISMISS_KEY, "1"); } catch {}
  updateInsecureBanner();
});
$("opt-mac-ps").addEventListener("change", async (event) => {
  const wanted = event.target.checked;
  event.target.disabled = true;
  try {
    const status = await api("/api/system/game-controller", {
      method: "POST",
      body: JSON.stringify({ use_ps_button: wanted }),
    });
    event.target.checked = Boolean(status.use_ps_button);
    toast(status.use_ps_button
      ? "PS button reserved for GameBridge. Log out and back in to apply."
      : "macOS Games app shortcut restored. Log out and back in to apply.");
  } catch (error) {
    event.target.checked = !wanted;
    toast(error.message, true);
  } finally {
    event.target.disabled = false;
  }
});

$("kb-profile").addEventListener("change", (event) => {
  keyboardProfile = event.target.value;
  state.keyboard.clear();
  cancelRebind(false);
  saveKeyboardProfiles();
  renderKeyboardBindings();
});
$("kb-profile-new").addEventListener("click", () => {
  const name = (prompt("Name for the new keyboard profile:", `Profile ${Object.keys(keyboardProfiles).length + 1}`) || "").trim();
  if (!name) return;
  if (["__proto__", "prototype", "constructor"].includes(name) || !/^[\w .-]{1,32}$/.test(name)) {
    toast("Use 1–32 letters, numbers, spaces, dots, dashes, or underscores", true);
    return;
  }
  if (keyboardProfiles[name]) {
    toast("A profile with that name already exists", true);
    return;
  }
  keyboardProfiles[name] = { ...currentKeyboardBindings() };
  keyboardProfile = name;
  state.keyboard.clear();
  saveKeyboardProfiles();
  renderKeyboardBindings();
});
$("kb-profile-delete").addEventListener("click", () => {
  if (keyboardProfile === "Default") return;
  if (!confirm(`Delete the keyboard profile "${keyboardProfile}"?`)) return;
  delete keyboardProfiles[keyboardProfile];
  keyboardProfile = "Default";
  state.keyboard.clear();
  saveKeyboardProfiles();
  renderKeyboardBindings();
});
$("kb-profile-reset").addEventListener("click", () => {
  if (!confirm(`Reset every binding in "${keyboardProfile}" to the defaults?`)) return;
  keyboardProfiles[keyboardProfile] = defaultKeyboardBindings();
  state.keyboard.clear();
  saveKeyboardProfiles();
  renderKeyboardBindings();
});
renderKeyboardBindings();

async function loadMacPsSetting() {
  try {
    const status = await api("/api/system/game-controller");
    $("mac-ps-setting").classList.toggle("hidden", !status.supported);
    $("opt-mac-ps").checked = Boolean(status.use_ps_button);
  } catch {}
}

const STREAM_OPTION_STORAGE = {
  resolution: "gamebridge.streamResolution",
  fps: "gamebridge.streamFps",
  bitrate: "gamebridge.streamBitrate",
  codec: "gamebridge.streamCodec",
  adaptive: "gamebridge.streamAdaptive",
};

function hevcCapabilities() {
  const result = { hevc: false, hdr: false };
  try {
    const caps = RTCRtpReceiver.getCapabilities?.("video");
    for (const codec of caps?.codecs || []) {
      if (String(codec.mimeType || "").toLowerCase() !== "video/h265") continue;
      result.hevc = true;
      const fmtp = String(codec.sdpFmtpLine || "").replace(/\s+/g, "");
      if (/(?:^|;)profile-id=2(?:;|$)/.test(fmtp)) result.hdr = true;
    }
  } catch {}
  return result;
}

function browserReportsHdrDisplay() {
  try {
    return window.matchMedia?.("(dynamic-range: high)")?.matches ?? false;
  } catch {
    return false;
  }
}

function loadStreamOptions() {
  const read = (key) => {
    try { return localStorage.getItem(key); } catch { return null; }
  };
  const resolution = read(STREAM_OPTION_STORAGE.resolution);
  if (["1080p", "720p", "540p", "360p"].includes(resolution)) {
    $("opt-resolution").value = resolution;
  }
  const fps = read(STREAM_OPTION_STORAGE.fps);
  if (fps === "30" || fps === "60") $("opt-fps").value = fps;
  const bitrate = parseInt(read(STREAM_OPTION_STORAGE.bitrate) || "", 10);
  if (Number.isFinite(bitrate) && bitrate >= 100 && bitrate <= 100000) {
    $("opt-bitrate").value = String(bitrate);
  }
  const codec = read(STREAM_OPTION_STORAGE.codec);
  if (["h264", "h265", "h265-hdr"].includes(codec)) $("opt-codec").value = codec;
  if (read(STREAM_OPTION_STORAGE.adaptive) === "0") $("opt-adaptive").checked = false;
}

function saveStreamOptions() {
  try {
    localStorage.setItem(STREAM_OPTION_STORAGE.resolution, $("opt-resolution").value);
    localStorage.setItem(STREAM_OPTION_STORAGE.fps, $("opt-fps").value);
    localStorage.setItem(STREAM_OPTION_STORAGE.bitrate, $("opt-bitrate").value);
    localStorage.setItem(STREAM_OPTION_STORAGE.codec, $("opt-codec").value);
    localStorage.setItem(STREAM_OPTION_STORAGE.adaptive, $("opt-adaptive").checked ? "1" : "0");
  } catch {}
}

const AUDIO_OUTPUT_KEY = "gamebridge.audioOutput";

function audioOutputSupported() {
  return typeof video.setSinkId === "function";
}

async function applyAudioOutput(deviceId) {
  if (!audioOutputSupported()) return false;
  try {
    await video.setSinkId(deviceId || "");
    return true;
  } catch {
    return false;
  }
}

async function initAudioOutputs() {
  const select = $("opt-audio-output");
  const hint = $("audio-output-hint");
  if (!select || !hint) return;
  if (!audioOutputSupported() || !navigator.mediaDevices?.enumerateDevices) {
    select.disabled = true;
    hint.textContent = "This browser does not support audio output selection; stream audio uses the system output.";
    return;
  }

  let outputs = [];
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    outputs = devices.filter((device) => device.kind === "audiooutput");
  } catch {}
  let saved = "";
  try { saved = localStorage.getItem(AUDIO_OUTPUT_KEY) || ""; } catch {}

  select.disabled = false;
  select.replaceChildren(new Option("System default", ""));
  outputs.forEach((device, index) => {
    select.add(new Option(device.label || `Audio output ${index + 1}`, device.deviceId));
  });
  const savedExists = saved && outputs.some((device) => device.deviceId === saved);
  select.value = savedExists && (await applyAudioOutput(saved)) ? saved : "";
  if (!select.value) await applyAudioOutput("");

  select.onchange = async () => {
    const deviceId = select.value;
    if (await applyAudioOutput(deviceId)) {
      try { localStorage.setItem(AUDIO_OUTPUT_KEY, deviceId); } catch {}
      toast(deviceId ? "Audio output changed" : "Using the system audio output");
    } else {
      toast("Could not switch audio output", true);
      select.value = "";
      await applyAudioOutput("");
    }
  };

  hint.textContent = outputs.some((device) => !device.label)
    ? "Output names can be hidden until the browser grants audio-device access; selection still works by device."
    : "Choose where stream audio plays.";
}

(function initStreamOptions() {
  loadStreamOptions();
  const { hevc, hdr } = hevcCapabilities();
  const hdrDisplay = browserReportsHdrDisplay();
  const select = $("opt-codec");
  for (const option of select.options) {
    if (option.value === "h264") continue;
    const unavailable = !hevc || (option.value === "h265-hdr" && !hdr);
    if (unavailable) {
      option.disabled = true;
      option.textContent = `${option.textContent} — unavailable`;
    }
  }
  if (!hevc) {
    select.value = "h264";
  } else if (!hdr && select.value === "h265-hdr") {
    select.value = "h265";
  }

  let support;
  if (!hevc) {
    support = "This browser does not expose hardware HEVC over WebRTC, so H.265 is disabled. Chrome 136+ and Safari 18+ can expose it on supported hardware.";
  } else if (!hdr) {
    support = "H.265 uses hardware HEVC decode in this browser. It did not advertise Main10 HDR support, so HDR is disabled.";
  } else if (!hdrDisplay) {
    support = "H.265 and Main10 HDR are available in this browser. This display does not report HDR, so HDR streams will be tone-mapped.";
  } else {
    support = "H.265 and Main10 HDR are available in this browser.";
  }
  $("codec-support").textContent = support;

  for (const id of ["opt-resolution", "opt-fps", "opt-bitrate", "opt-codec", "opt-adaptive"]) {
    $(id).addEventListener("change", saveStreamOptions);
  }
  saveStreamOptions();
})();

initAudioOutputs();
navigator.mediaDevices?.addEventListener?.("devicechange", initAudioOutputs);

document.addEventListener("click", (event) => {
  for (const menu of document.querySelectorAll(".row-menu[open]")) {
    if (!menu.contains(event.target)) menu.open = false;
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (touchMenuIsOpen()) {
    event.preventDefault();
    event.stopPropagation();
    closeTouchMenu();
    return;
  }
  if (settingsIsOpen()) {
    event.preventDefault();
    event.stopPropagation();
    closeSettings();
    return;
  }
  if (addIsOpen()) {
    event.preventDefault();
    event.stopPropagation();
    closeAddConsole();
    return;
  }
  for (const menu of document.querySelectorAll(".row-menu[open]")) menu.open = false;
});

videoWrap.addEventListener("mousemove", () => showPlayerBar());
videoWrap.addEventListener("touchstart", (event) => {
  if (event.target.closest?.("#touch-controls")) return;
  showPlayerBar();
}, { passive: true });

$("reg-pin-clear").addEventListener("click", () => {
  $("reg-pin").value = "";
  $("reg-pin").focus();
});
$("pin-clear").addEventListener("click", () => {
  $("pin-input").value = "";
  $("pin-input").focus();
});
demoBtn.addEventListener("click", startDemo);
$("stop-btn").addEventListener("click", stopSession);
$("resync-btn").addEventListener("click", () => {
  if (!sendWs({ type: "keyframe" })) {
    toast("Session is not connected", true);
    return;
  }
  toast("Requested a fresh keyframe");
});
$("rest-btn").addEventListener("click", () => {
  if (!confirm("Put the console into Rest Mode? The stream will end.")) return;
  if (!sendWs({ type: "goto_bed" })) {
    toast("Session is not connected", true);
    return;
  }
  setStatus("Rest mode", "");
  toast("Putting the console into Rest Mode…");
});
$("fullscreen-btn").addEventListener("click", () => {
  if (document.fullscreenElement) {
    ignorePromiseRejection(document.exitFullscreen());
  } else {
    ignorePromiseRejection(videoWrap.requestFullscreen?.());
  }
});

async function enterApp() {
  contextReady = false;
  lastShownContext = "";
  const loaded = await loadHosts();
  if (!loaded || !authModal.classList.contains("hidden")) return;
  await loadStatus();
  loadPsnLogin();
  loadMacPsSetting();
  await discover({ quiet: true });
  if (!authModal.classList.contains("hidden")) return;
  const noConsoles = !state.hosts.length && !discoveredEl.querySelector(".nearby-tile");
  const canAutoOpen = noConsoles
    && !settingsIsOpen()
    && !addIsOpen()
    && !document.body.classList.contains("player-active");
  if (canAutoOpen) openAddConsole({ auto: true });
  primeContextToast();
}

$("auth-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const token = $("auth-token").value;
  if (!token) return;
  const button = $("auth-form").querySelector("button[type=submit]");
  button.disabled = true;
  $("auth-error").textContent = "";
  try {
    await api("/api/auth/login", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    hideAuthModal();
    lockBtn.classList.remove("hidden");
    enterApp();
  } catch (e) {
    $("auth-error").textContent = e.status === 401 ? "Invalid token." : e.message;
    $("auth-token").select();
  } finally {
    button.disabled = false;
  }
});

lockBtn.addEventListener("click", async () => {
  if (state.sessionId || state.pc || state.detachedActive) {
    try { await stopSession(); } catch {}
  }
  try {
    await api("/api/auth/logout", { method: "POST", body: "{}" });
  } catch {}
  showAuthModal();
});

async function startApp() {
  try {
    const response = await fetch("/api/auth/status", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const status = await response.json();
    const required = !!status.required;
    lockBtn.classList.toggle("hidden", !required);
    if (required && !status.authenticated) {
      showAuthModal();
      return;
    }
    hideAuthModal();
    enterApp();
  } catch (e) {
    toast(`Could not reach the server: ${e.message}`, true);
  }
}

window.addEventListener("beforeunload", () => {
  if (state.sessionId) {
    navigator.sendBeacon?.("/api/session/stop", new Blob(["{}"], { type: "application/json" }));
  }
});

$("player-bar").addEventListener("pointerdown", () => clearTimeout(playerBarTimer));
$("player-bar").addEventListener("pointerup", () => showPlayerBar());
$("player-bar").addEventListener("pointercancel", () => showPlayerBar());
touchMenuBtn.addEventListener("click", () => {
  if (touchMenuIsOpen()) closeTouchMenu();
  else openTouchMenu();
});
$("touch-menu-close").addEventListener("click", () => closeTouchMenu());
$("touch-menu-backdrop").addEventListener("click", () => closeTouchMenu());
touchMenuVolume.addEventListener("input", () => {
  setVolume(parseInt(touchMenuVolume.value, 10) / 100);
});
touchMenuVolume.addEventListener("change", saveAudioState);
touchMenuEl.addEventListener("click", (event) => {
  const button = event.target.closest("[data-menu-action]");
  if (!button || button.disabled) return;
  const action = button.dataset.menuAction;
  if (action === "controls") {
    const enabled = !state.touch.enabled;
    closeTouchMenu({ restoreFocus: false });
    setTouchControlsEnabled(enabled);
    toast(enabled ? "On-screen controls shown" : "On-screen controls hidden");
    return;
  }
  const sourceId = TOUCH_MENU_SOURCE[action];
  if (!sourceId) return;
  const source = $(sourceId);
  closeTouchMenu({ restoreFocus: false });
  if (!source.disabled) source.click();
});
portraitMedia.addEventListener?.("change", updateOrientationState);
screen.orientation?.addEventListener?.("change", updateOrientationState);
window.addEventListener("orientationchange", updateOrientationState);

initTouchInput();
state.motion.enabled = loadMotionEnabled();
$("opt-motion").checked = state.motion.enabled;
showHome();
updateOrientationState();
updateMotionStatus();
primeInsecureWarning();
setBusy(false);
setBusy(false);
loadSavedPsnAccount();
startApp();
