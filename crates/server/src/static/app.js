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
};

/* ── Sharpening (WebGL2 CAS-style pass) ───────────────────────────────── */

const sharpCanvas = $("video-sharp");
const sharpnessInput = $("opt-sharpness");
const sharpnessValue = $("sharpness-value");

const sharp = {
  gl: null,
  program: null,
  texture: null,
  uTexel: null,
  uStrength: null,
  active: false,
  shown: false,
  raf: 0,
  rvfc: 0,
  viewW: 0,
  viewH: 0,
  unavailable: false,
};

function sharpnessAmount() {
  const n = parseInt(sharpnessInput.value, 10);
  return Number.isFinite(n) ? Math.min(100, Math.max(0, n)) : 0;
}

function sharpnessStrength() {
  return (sharpnessAmount() / 100) * 1.2;
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
  const compile = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      console.warn("sharpness shader:", gl.getShaderInfoLog(shader));
      return null;
    }
    return shader;
  };
  const vs = compile(gl.VERTEX_SHADER, `#version 300 es
    in vec2 a_pos;
    out vec2 v_uv;
    void main() { v_uv = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }`);
  const fs = compile(gl.FRAGMENT_SHADER, `#version 300 es
    precision highp float;
    uniform sampler2D u_tex;
    uniform vec2 u_texel;
    uniform float u_strength;
    in vec2 v_uv;
    out vec4 o_color;
    void main() {
      vec3 c  = texture(u_tex, v_uv).rgb;
      vec3 n  = texture(u_tex, v_uv + vec2(0.0, -u_texel.y)).rgb;
      vec3 s  = texture(u_tex, v_uv + vec2(0.0,  u_texel.y)).rgb;
      vec3 e  = texture(u_tex, v_uv + vec2( u_texel.x, 0.0)).rgb;
      vec3 w  = texture(u_tex, v_uv + vec2(-u_texel.x, 0.0)).rgb;
      vec3 ne = texture(u_tex, v_uv + vec2( u_texel.x, -u_texel.y)).rgb;
      vec3 nw = texture(u_tex, v_uv + vec2(-u_texel.x, -u_texel.y)).rgb;
      vec3 se = texture(u_tex, v_uv + vec2( u_texel.x,  u_texel.y)).rgb;
      vec3 sw = texture(u_tex, v_uv + vec2(-u_texel.x,  u_texel.y)).rgb;
      vec3 blur = c * 0.25 + (n + s + e + w) * 0.125 + (ne + nw + se + sw) * 0.0625;
      vec3 sharpened = c + (c - blur) * u_strength;
      vec3 lo = min(min(min(n, s), min(e, w)), c);
      vec3 hi = max(max(max(n, s), max(e, w)), c);
      o_color = vec4(clamp(sharpened, lo, hi), 1.0);
    }`);
  if (!vs || !fs) {
    sharp.unavailable = true;
    return null;
  }
  const program = gl.createProgram();
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.warn("sharpness program:", gl.getProgramInfoLog(program));
    sharp.unavailable = true;
    return null;
  }
  gl.useProgram(program);
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const pos = gl.getAttribLocation(program, "a_pos");
  gl.enableVertexAttribArray(pos);
  gl.vertexAttribPointer(pos, 2, gl.FLOAT, false, 0, 0);
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
  sharp.gl = gl;
  sharp.program = program;
  sharp.texture = texture;
  sharp.uTexel = gl.getUniformLocation(program, "u_texel");
  sharp.uStrength = gl.getUniformLocation(program, "u_strength");
  return gl;
}

function drawSharp() {
  const gl = sharp.gl;
  if (!gl || video.readyState < 2) return;
  const w = video.videoWidth;
  const h = video.videoHeight;
  if (!w || !h) return;
  if (sharpCanvas.width !== w || sharpCanvas.height !== h) {
    sharpCanvas.width = w;
    sharpCanvas.height = h;
    sharp.viewW = 0;
  }
  if (sharp.viewW !== w || sharp.viewH !== h) {
    gl.viewport(0, 0, w, h);
    gl.uniform2f(sharp.uTexel, 1 / w, 1 / h);
    sharp.viewW = w;
    sharp.viewH = h;
  }
  gl.useProgram(sharp.program);
  gl.uniform1f(sharp.uStrength, sharpnessStrength());
  gl.bindTexture(gl.TEXTURE_2D, sharp.texture);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, video);
  gl.drawArrays(gl.TRIANGLES, 0, 3);
  if (!sharp.shown) {
    sharp.shown = true;
    sharpCanvas.classList.remove("hidden");
    video.classList.add("sharpening");
  }
}

function sharpFrame() {
  if (!sharp.active) return;
  drawSharp();
  if (video.requestVideoFrameCallback) {
    sharp.rvfc = video.requestVideoFrameCallback(sharpFrame);
  } else {
    sharp.raf = requestAnimationFrame(sharpFrame);
  }
}

function startSharpening() {
  if (sharp.active || sharpnessAmount() === 0) return;
  if (!initSharpGl()) return;
  sharp.active = true;
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

function applySharpness() {
  const value = sharpnessAmount();
  sharpnessValue.textContent = String(value);
  if (value === 0) stopSharpening();
  else if (state.stream) startSharpening();
}

sharpnessInput.addEventListener("input", () => {
  try {
    localStorage.setItem("psrp.sharpness", String(sharpnessAmount()));
  } catch {}
  applySharpness();
});

(function initSharpness() {
  let value = 35;
  try {
    const stored = localStorage.getItem("psrp.sharpness");
    if (stored !== null) value = Math.min(100, Math.max(0, parseInt(stored, 10) || 0));
  } catch {}
  sharpnessInput.value = String(value);
  sharpnessValue.textContent = String(value);
})();


function setStatus(text, kind = "") {
  statusPill.textContent = text;
  statusPill.className = "status-pill" + (kind ? " " + kind : "");
}

let toastTimer = null;
function toast(message, bad = false) {
  toastEl.textContent = message;
  toastEl.className = "toast" + (bad ? " bad" : "");
  toastTimer = setTimeout(() => toastEl.classList.add("hidden"), 4500);
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
      await api(`/api/hosts/${encodeURIComponent(host.id)}`, { method: "DELETE" });
      await loadHosts();
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
  if (state.pc || state.sessionId) {
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
    state.sessionId = response.session_id;
    await setupPeer(response.offer);
    setBusy(true);
    connectSocket();
    startInputLoop();
  } catch (e) {
    showOverlay("Could not start", e.message);
    setStatus("Error", "bad");
    toast(e.message, true);
    await cleanupSession(false);
  }
}

async function startDemo() {
  if (state.pc || state.sessionId) {
    toast("A session is already active", true);
    return;
  }
  showOverlay("Starting demo…", "Opening a local H.264 test stream.");
  setStatus("Starting", "");
  try {
    const response = await api("/api/demo/start", { method: "POST" });
    state.sessionId = response.session_id;
    await setupPeer(response.offer);
    setBusy(true);
    connectSocket();
    startInputLoop();
  } catch (e) {
    showOverlay("Could not start demo", e.message);
    setStatus("Error", "bad");
    toast(e.message, true);
    await cleanupSession(false);
  }
}

async function loadStatus() {
  try {
    const status = await api("/api/status");
    demoBtn.classList.toggle("hidden", !status.demo);
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

async function setupPeer(offer) {
  const pc = new RTCPeerConnection({ iceServers: [] });
  state.pc = pc;
  state.stream = new MediaStream();
  pc.ontrack = (event) => {
    state.stream.addTrack(event.track);
    video.srcObject = state.stream;
    video.muted = false;
    applySharpness();
    video.play().catch(() => {
      toast("Click the video to enable audio");
    });
  };
  pc.onconnectionstatechange = () => {
    const s = pc.connectionState;
    if (s === "connected") {
      hideOverlay();
      setStatus("Live", "live");
      state.streaming = true;
      hintOverlay.classList.add("hidden");
      setStatus("Live", "live");
    } else if (s === "failed" || s === "disconnected" || s === "closed") {
      setStatus(s, "bad");
    } else {
      setStatus(s, "");
    }
  };
  await pc.setRemoteDescription({ type: "offer", sdp: offer.sdp });
  const answer = await pc.createAnswer();
  await pc.setLocalDescription(answer);
  await waitForIceGathering(pc);
  await api("/api/session/answer", {
    method: "POST",
    body: JSON.stringify({ sdp: pc.localDescription.sdp }),
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
      statsEl.textContent = `${event.received} pkts · ${event.lost} lost`;
      break;
    case "remote_disconnect":
      toast(`Console disconnected: ${event.reason}`, true);
      setStatus("Remote disconnected", "bad");
      break;
    case "error":
      toast(event.message, true);
      break;
    case "quit":
      setStatus(event.reason || "Ended", event.reason === "Stopped" ? "" : "bad");
      toast(event.reason_string || event.reason || "Session ended", event.reason !== "Stopped");
      showOverlay("Session ended", event.reason_string || event.reason || "");
      cleanupSession(true);
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
  state.keyboard.clear();
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

function touchpadPoint(event) {
  const rect = video.getBoundingClientRect();
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
