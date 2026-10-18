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
let packetCount = { received: 0, lost: 0 };

function renderStats() {
  const parts = [];
  if (Number.isFinite(consoleLatencyMs)) {
    const ms = consoleLatencyMs < 10
      ? consoleLatencyMs.toFixed(1)
      : Math.round(consoleLatencyMs);
    parts.push(`${ms} ms`);
  }
  parts.push(`${packetCount.received} pkts`, `${packetCount.lost} lost`);
  statsEl.textContent = parts.join(" · ");
}
const hostsEl = $("hosts");
const discoveredEl = $("discovered");
const discoveredPanel = $("discovered-panel");
const pinModal = $("pin-modal");
const toastEl = $("toast");
const demoBtn = $("demo-btn");

const state = {
  hosts: [],
  sessionId: null,
  pc: null,
  ws: null,
  stream: null,
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
};
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
  rcasProgram: null,
  inputTex: null,
  midTex: null,
  fbo: null,
  lutTex: null,
  u: {
    easuTex: null, easuSize: null, con0: null,
    rcasTex: null, rcasLut: null, rcasSize: null, rcasSharp: null,
    rcasExposure: null, rcasContrast: null, rcasSaturation: null,
  },
  conCache: null,
  grade: { exposure: 1, contrast: 1, saturation: 1 },
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
  return sharpnessAmount() > 0 || !colorIsNeutral();
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
  brightnessValue.textContent = String(colorAmount(brightnessInput));
  contrastValue.textContent = String(colorAmount(contrastInput));
  saturationValue.textContent = String(colorAmount(saturationInput));
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
  const rcas = makeSharpProgram(gl, RCAS_FS);
  if (!easu || !rcas) {
    if (easu) gl.deleteProgram(easu);
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
  sharp.rcasProgram = rcas;
  sharp.inputTex = inputTex;
  sharp.lutTex = makeSrgbLut(gl);
  sharp.maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE);
  sharp.u.easuTex = gl.getUniformLocation(easu, "u_tex");
  sharp.u.easuSize = gl.getUniformLocation(easu, "u_in_size");
  sharp.u.con0 = gl.getUniformLocation(easu, "u_con0");
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

function ensureSharpTargets(w, h) {
  const gl = sharp.gl;
  if (sharp.midTex && sharp.texW === w && sharp.texH === h) return true;
  if (sharp.fbo) {
    gl.deleteFramebuffer(sharp.fbo);
    sharp.fbo = null;
  }
  if (sharp.midTex) {
    gl.deleteTexture(sharp.midTex);
    sharp.midTex = null;
  }
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
    sharp.unavailable = true;
    return false;
  }
  sharp.midTex = tex;
  sharp.fbo = fbo;
  sharp.texW = w;
  sharp.texH = h;
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

  gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  gl.viewport(0, 0, w, h);
  gl.useProgram(sharp.rcasProgram);
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, sharp.midTex);
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
    localStorage.setItem("psrp.sharpness", String(sharpnessAmount()));
    localStorage.setItem("psrp.brightness", String(colorAmount(brightnessInput)));
    localStorage.setItem("psrp.contrast", String(colorAmount(contrastInput)));
    localStorage.setItem("psrp.saturation", String(colorAmount(saturationInput)));
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

[sharpnessInput, brightnessInput, contrastInput, saturationInput].forEach(bindPictureSlider);

pictureReset.addEventListener("click", () => {
  sharpnessInput.value = String(PICTURE_DEFAULTS.sharpness);
  brightnessInput.value = String(PICTURE_DEFAULTS.brightness);
  contrastInput.value = String(PICTURE_DEFAULTS.contrast);
  saturationInput.value = String(PICTURE_DEFAULTS.saturation);
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
  load(sharpnessInput, "psrp.sharpness", PICTURE_DEFAULTS.sharpness);
  load(brightnessInput, "psrp.brightness", PICTURE_DEFAULTS.brightness);
  load(contrastInput, "psrp.contrast", PICTURE_DEFAULTS.contrast);
  load(saturationInput, "psrp.saturation", PICTURE_DEFAULTS.saturation);
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
document.addEventListener("fullscreenchange", () => { sharp.sizeDirty = true; });

sharpCanvas.addEventListener("webglcontextlost", (event) => {
  event.preventDefault();
  sharp.gl = null;
  sharp.easuProgram = null;
  sharp.rcasProgram = null;
  sharp.inputTex = null;
  sharp.midTex = null;
  sharp.fbo = null;
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
function toast(message, bad = false) {
  // A new toast replaces the old one, so its dismissal timer must be reset;
  // otherwise the previous timer hides the new message almost immediately.
  if (toastTimer) clearTimeout(toastTimer);
  toastEl.textContent = message;
  toastEl.className = "toast" + (bad ? " bad" : "");
  toastTimer = setTimeout(() => {
    toastEl.classList.add("hidden");
    toastTimer = null;
  }, 4500);
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
    throw new Error((data && data.error) || `HTTP ${response.status}`);
  }
  return data;
}

function showOverlay(title, text) {
  overlayTitle.textContent = title;
  overlayText.textContent = text;
  overlay.classList.remove("hidden");
}

function hideOverlay() {
  overlay.classList.add("hidden");
}

function setBusy(busy) {
  $("stop-btn").disabled = !busy;
}

/* ── Hosts ─────────────────────────────────────────────────────────────── */

async function loadHosts() {
  try {
    state.hosts = await api("/api/hosts");
    renderHosts();
  } catch (e) {
    toast(`Could not load consoles: ${e.message}`, true);
  }
}

function renderHosts() {
  hostsEl.innerHTML = "";
  if (!state.hosts.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = "No consoles registered yet.";
    hostsEl.appendChild(p);
    return;
  }
  for (const host of state.hosts) {
    const card = document.createElement("div");
    card.className = "host-card";
    const info = document.createElement("div");
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = host.name || host.nickname || host.host;
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = `${host.host} · ${targetLabel(host.target)}`;
    info.append(name, meta);
    const actions = document.createElement("div");
    actions.className = "actions";
    const play = document.createElement("button");
    play.className = "primary small";
    play.textContent = "Play";
    play.onclick = () => startSession(host);
    const wake = document.createElement("button");
    wake.className = "ghost small";
    wake.textContent = "Wake";
    wake.title = "Wake the console from rest mode";
    wake.onclick = async () => {
      wake.disabled = true;
      wake.textContent = "Waking…";
      try {
        await api(`/api/wakeup/${encodeURIComponent(host.id)}`, { method: "POST", body: "{}" });
        toast(`Wake packet sent to ${host.name || host.host}`);
      } catch (e) {
        toast(e.message, true);
      } finally {
        wake.disabled = false;
        wake.textContent = "Wake";
      }
    };
    const del = document.createElement("button");
    del.className = "danger small";
    del.textContent = "×";
    del.title = "Remove";
    del.onclick = async () => {
      if (!confirm(`Remove ${host.name}?`)) return;
      del.disabled = true;
      try {
        await api(`/api/hosts/${encodeURIComponent(host.id)}`, { method: "DELETE" });
        state.hosts = state.hosts.filter((h) => h.id !== host.id);
        renderHosts();
        loadHosts(); // best-effort refresh; it reports its own failures
      } catch (e) {
        toast(e.message, true);
        del.disabled = false;
      }
    };
    actions.append(play, wake, del);
    card.append(info, actions);
    hostsEl.appendChild(card);
  }
}

function targetLabel(target) {
  return ({ ps5_1: "PS5", ps4_10: "PS4 8.0+", ps4_9: "PS4 7.0", ps4_8: "PS4" })[target] || target;
}

async function discover() {
  const btn = $("discover-btn");
  btn.disabled = true;
  btn.textContent = "Scanning…";
  try {
    const found = await api("/api/discover", { method: "POST", body: "{}" });
    renderDiscovered(found);
  } catch (e) {
    toast(`Scan failed: ${e.message}`, true);
  } finally {
    btn.disabled = false;
    btn.textContent = "Scan";
  }
}

function renderDiscovered(found) {
  discoveredEl.innerHTML = "";
  if (!found.length) {
    discoveredEl.innerHTML = '<p class="empty">No consoles responded. Check that Remote Play is enabled and the console is awake.</p>';
    discoveredPanel.classList.remove("hidden");
    return;
  }
  for (const host of found) {
    const card = document.createElement("div");
    card.className = "host-card";
    const info = document.createElement("div");
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = host.host_name || host.host_addr;
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = `${host.host_addr} · ${host.state} · ${targetLabel(host.target)}`;
    info.append(name, meta);
    const actions = document.createElement("div");
    actions.className = "actions";
    const use = document.createElement("button");
    use.className = "small";
    use.textContent = "Use";
    use.onclick = () => {
      $("reg-host").value = host.host_addr;
      $("reg-target").value = host.target;
      $("register-panel").open = true;
      $("reg-host").focus();
    };
    actions.append(use);
    card.append(info, actions);
    discoveredEl.appendChild(card);
  }
  discoveredPanel.classList.remove("hidden");
}

/* ── Registration ──────────────────────────────────────────────────────── */

$("register-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const button = $("register-btn");
  button.disabled = true;
  button.textContent = "Pairing…";
  try {
    await api("/api/register", {
      method: "POST",
      body: JSON.stringify({
        host: $("reg-host").value.trim(),
        target: $("reg-target").value,
        pin: $("reg-pin").value.trim(),
        psn_account_id: $("reg-account").value.trim() || null,
        psn_online_id: $("reg-online").value.trim() || null,
        name: $("reg-name").value.trim() || null,
        broadcast: $("reg-broadcast").checked,
      }),
    });
    $("reg-pin").value = "";
    toast("Console registered");
    await loadHosts();
  } catch (e) {
    toast(e.message, true);
  } finally {
    button.disabled = false;
    button.textContent = "Register";
  }
});

/* ── Session ───────────────────────────────────────────────────────────── */

async function startSession(host) {
  if (state.pc || state.sessionId || state.detachedActive) {
    toast("A session is already active", true);
    return;
  }
  showOverlay("Starting…", "Requesting a session from the console.");
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
        enable_dualsense: $("opt-dualsense").checked,
      }),
    });
    const sessionId = response.session_id;
    state.sessionId = sessionId;
    // Subscribe before ICE gathering so PIN/phase events are not missed.
    connectSocket();
    setBusy(true);
    await setupPeer(response.offer, sessionId);
    if (state.sessionId !== sessionId) return;
    startInputLoop();
  } catch (e) {
    if (state.sessionId === null) return;
    showOverlay("Could not start", e.message);
    setStatus("Error", "bad");
    toast(e.message, true);
    await cleanupSession(false);
  }
}

async function startDemo() {
  if (state.pc || state.sessionId || state.detachedActive) {
    toast("A session is already active", true);
    return;
  }
  showOverlay("Starting demo…", "Opening a local H.264 test stream.");
  setStatus("Starting", "");
  try {
    const response = await api("/api/demo/start", { method: "POST" });
    const sessionId = response.session_id;
    state.sessionId = sessionId;
    connectSocket();
    setBusy(true);
    await setupPeer(response.offer, sessionId);
    if (state.sessionId !== sessionId) return;
    startInputLoop();
  } catch (e) {
    if (state.sessionId === null) return;
    showOverlay("Could not start demo", e.message);
    setStatus("Error", "bad");
    toast(e.message, true);
    await cleanupSession(false);
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
        showOverlay(
          "Select a console to begin",
          "Registration pairs this browser bridge with your PS4 or PS5.",
        );
      }
    } catch {}
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
      showOverlay(
        "Session already active",
        "A stream started in another tab is still running. Press Stop to release the console.",
      );
      startDetachedPoll();
    }
  } catch {}
}

async function resolvePsnAccountId() {
  const input = $("psn-redirect").value.trim();
  if (!input) {
    toast("Paste the redirect URL from the PSN sign-in page", true);
    return;
  }
  const button = $("psn-resolve-btn");
  button.disabled = true;
  button.textContent = "Looking up…";
  try {
    const result = await api("/api/psn/account-id", {
      method: "POST",
      body: JSON.stringify({ redirect_url: input }),
    });
    $("reg-account").value = result.account_id;
    $("psn-redirect").value = "";
    toast("PSN Account ID filled in");
  } catch (e) {
    toast(e.message, true);
  } finally {
    button.disabled = false;
    button.textContent = "Get ID";
  }
}

async function loadPsnLogin() {
  try {
    const info = await api("/api/psn/login");
    $("psn-login-link").href = info.url;
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
    video.muted = false;
    applyPicture();
    video.play().catch(() => {
      toast("Click the video to enable audio");
    });
  };
  pc.onconnectionstatechange = () => {
    if (state.pc !== pc) return;
    const s = pc.connectionState;
    if (s === "connected") {
      hideOverlay();
      setStatus("Live", "live");
      state.streaming = true;
      hintOverlay.classList.add("hidden");
    } else if (s === "failed" || s === "disconnected" || s === "closed") {
      setStatus(s, "bad");
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

function connectSocket() {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${scheme}://${location.host}/api/session/ws`);
  state.ws = ws;
  ws.onmessage = (event) => {
    let msg;
    try { msg = JSON.parse(event.data); } catch { return; }
    handleServerEvent(msg);
  };
  ws.onclose = () => {
    if (state.streaming) {
      setStatus("Disconnected", "bad");
    }
  };
}

function handleServerEvent(event) {
  switch (event.type) {
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
      break;
    case "stats":
      packetCount = { received: event.received, lost: event.lost };
      consoleLatencyMs = Number.isFinite(event.latency_ms) ? event.latency_ms : null;
      renderStats();
      break;
    case "remote_disconnect":
      toast(`Console disconnected: ${event.reason}`, true);
      setStatus("Remote disconnected", "bad");
      break;
    case "error":
      toast(event.message, true);
      break;
    case "quit":
      cleanupSession(true);
      setStatus(event.reason || "Ended", event.reason === "Stopped" ? "" : "bad");
      toast(event.reason_string || event.reason || "Session ended", event.reason !== "Stopped");
      showOverlay("Session ended", event.reason_string || event.reason || "");
      break;
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
  $("pin-input").focus();
}

$("pin-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const pin = $("pin-input").value.trim();
  if (!pin) return;
  sendWs({ type: "pin", pin });
  pinModal.classList.add("hidden");
});

function cancelPin() {
  pinModal.classList.add("hidden");
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
  try {
    await api("/api/session/stop", { method: "POST", body: "{}" });
  } catch (e) {
    toast(e.message, true);
  }
  await cleanupSession(true);
}

async function cleanupSession(resetUi) {
  state.streaming = false;
  state.detachedActive = false;
  stopDetachedPoll();
  state.keyboard.clear();
  state.mouse = { dx: 0, dy: 0, left: false, right: false, locked: false };
  state.touches.clear();
  state.lastInputJson = "";
  state.lastInputAt = 0;
  pinModal.classList.add("hidden");
  if (document.pointerLockElement) document.exitPointerLock();
  if (state.inputTimer) {
    clearInterval(state.inputTimer);
    state.inputTimer = null;
  }
  if (state.ws) {
    try { state.ws.close(); } catch {}
    state.ws = null;
  }
  if (state.pc) {
    try { state.pc.close(); } catch {}
    state.pc = null;
  }
  state.stream = null;
  state.sessionId = null;
  consoleLatencyMs = null;
  packetCount = { received: 0, lost: 0 };
  stopSharpening();
  video.srcObject = null;
  setBusy(false);
  hintOverlay.classList.add("hidden");
  if (resetUi) {
    setStatus("Idle", "");
    statsEl.textContent = "";
    showOverlay("Select a console to begin", "Registration pairs this browser bridge with your PS4 or PS5.");
  }
}

/* ── Input ─────────────────────────────────────────────────────────────── */

const KEY_BUTTONS = {
  Space: B.CROSS,
  KeyJ: B.MOON,
  KeyK: B.BOX,
  KeyL: B.PYRAMID,
  ArrowLeft: B.DPAD_LEFT,
  ArrowRight: B.DPAD_RIGHT,
  ArrowUp: B.DPAD_UP,
  ArrowDown: B.DPAD_DOWN,
  KeyQ: B.L1,
  KeyE: B.R1,
  KeyZ: 0,
  KeyC: 0,
  Tab: B.TOUCHPAD,
  Enter: B.OPTIONS,
  Backspace: B.SHARE,
  KeyF: B.L3,
  KeyG: B.R3,
};

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
  if (!pinModal.classList.contains("hidden")) return;
  if (isEditableTarget(event.target)) return;
  if (event.code in KEY_BUTTONS || ["KeyW", "KeyA", "KeyS", "KeyD", "Escape"].includes(event.code)) {
    if (event.code !== "Escape") event.preventDefault();
  }
  state.keyboard.add(event.code);
});
window.addEventListener("keyup", (event) => {
  state.keyboard.delete(event.code);
});
window.addEventListener("blur", () => state.keyboard.clear());

// Form controls keep all native editing keys (Backspace, Delete, arrows,
// Tab, Space, Enter). Stop these events before any gamepad shortcut sees
// them, independent of the streaming state.
document.querySelectorAll("input, textarea, select").forEach((element) => {
  element.addEventListener("keydown", (event) => event.stopPropagation());
});

function startInputLoop() {
  if (state.inputTimer) clearInterval(state.inputTimer);
  state.inputTimer = setInterval(sendInput, 16);
}

function deadzone(value) {
  return Math.abs(value) < 0.08 ? 0 : value;
}

function sendInput() {
  if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return;
  const buttons = keyboardButtons() | gamepadButtons();
  const pad = activeGamepad();
  const l2 = Math.max(keyboardTrigger("KeyZ"), mouseTrigger("right"), pad ? Math.round((pad.buttons[6]?.value || 0) * 255) : 0);
  const r2 = Math.max(keyboardTrigger("KeyC"), mouseTrigger("left"), pad ? Math.round((pad.buttons[7]?.value || 0) * 255) : 0);
  let lx = 0, ly = 0, rx = 0, ry = 0;
  if (pad) {
    lx = axisToI16(deadzone(pad.axes[0] || 0));
    ly = axisToI16(deadzone(pad.axes[1] || 0));
    rx = axisToI16(deadzone(pad.axes[2] || 0));
    ry = axisToI16(deadzone(pad.axes[3] || 0));
  }
  if (state.keyboard.has("KeyA")) lx = -32767;
  if (state.keyboard.has("KeyD")) lx = 32767;
  if (state.keyboard.has("KeyW")) ly = -32767;
  if (state.keyboard.has("KeyS")) ly = 32767;

  if (state.mouse.locked) {
    rx = clampI16(rx + state.mouse.dx * 700);
    ry = clampI16(ry + state.mouse.dy * 700);
    state.mouse.dx *= 0.55;
    state.mouse.dy *= 0.55;
    if (Math.abs(state.mouse.dx) < 0.01) state.mouse.dx = 0;
    if (Math.abs(state.mouse.dy) < 0.01) state.mouse.dy = 0;
  }

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
  const json = JSON.stringify(payload);
  const now = performance.now();
  if (json === state.lastInputJson && now - state.lastInputAt < 100) return;
  state.lastInputJson = json;
  state.lastInputAt = now;
  sendWs(payload);
}

function sendWs(payload) {
  if (state.ws && state.ws.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify(payload));
  }
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
  let mask = 0;
  for (const [code, bit] of Object.entries(KEY_BUTTONS)) {
    if (bit && state.keyboard.has(code)) mask |= bit;
  }
  return mask;
}

function keyboardTrigger(code) {
  return state.keyboard.has(code) ? 255 : 0;
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

video.addEventListener("click", () => {
  if (!video.srcObject) return;
  if (document.pointerLockElement !== video) {
    video.requestPointerLock?.();
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
  video.setPointerCapture?.(event.pointerId);
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

/* Rumble */

function vibrate(left, right) {
  const pad = activeGamepad();
  if (!pad || !pad.vibrationActuator) return;
  try {
    pad.vibrationActuator.playEffect("dual-rumble", {
      duration: 120,
      strongMagnitude: Math.min(1, (left || 0) / 255),
      weakMagnitude: Math.min(1, (right || 0) / 255),
    });
  } catch {}
}

/* Buttons */

$("discover-btn").addEventListener("click", discover);
$("reg-pin-clear").addEventListener("click", () => {
  $("reg-pin").value = "";
  $("reg-pin").focus();
});
$("pin-clear").addEventListener("click", () => {
  $("pin-input").value = "";
  $("pin-input").focus();
});
demoBtn.addEventListener("click", startDemo);
$("psn-resolve-btn").addEventListener("click", resolvePsnAccountId);
$("stop-btn").addEventListener("click", stopSession);
$("fullscreen-btn").addEventListener("click", () => {
  if (document.fullscreenElement) {
    document.exitFullscreen();
  } else {
    videoWrap.requestFullscreen?.();
  }
});

window.addEventListener("beforeunload", () => {
  if (state.sessionId) {
    navigator.sendBeacon?.("/api/session/stop", new Blob(["{}"], { type: "application/json" }));
  }
});

loadHosts();
loadStatus();
loadPsnLogin();
