# PS Remote Play (browser bridge)

A Rust reimplementation of the PlayStation Remote Play protocol, packaged as a
localhost web server that streams a PS4 or PS5 into any browser over WebRTC.

Browsers cannot open the raw UDP sockets Remote Play requires, so this project
bridges the console protocol to the browser:

```
Browser  ── HTTP + WebSocket ──▶  ps-remoteplay  ── TCP/UDP ──▶  Console
   ▲                                  │
   └──────── WebRTC (H.264/Opus) ◀────┘
```

The server owns registration, discovery, session negotiation, the Takion UDP
transport, FEC, and input; the browser only renders WebRTC tracks and sends
controller input over the WebSocket.

This is a from-scratch Rust port of the protocol implemented by
[Chiaki](https://git.sr.ht/~thestr4ng3r/chiaki) v2.2.0. It is licensed
AGPL-3.0-only; see [Licensing](#licensing).

## What works

- Console registration, discovery (SRCH/SRC2/SRC3), and wake-on-LAN
- Session request with RP-Version negotiation (PS4 8.0/9.0/10.0, PS5 1.0)
- Ctrl channel: session id, console PIN login, heartbeat, standby (`goto bed`)
- Takion transport: handshake, RPCrypt/GKCrypt, GMAC, reordering, retransmit
- H.264 video and Opus audio with Reed–Solomon FEC, gap detection, and IDR resync
- WebRTC delivery with non-trickle ICE (localhost)
- Gamepad, keyboard/mouse, and touch input; rumble events; latency and packet
  stats. Latency is the round-trip time to the console: measured by Senkusha at
  stream start, then updated from the console's own connection-quality reports.
- Demo mode that streams a local H.264 file with no console attached

Deferred/unsupported (see [Limitations](#limitations)): H.265/HDR, Senkusha
(PS5 cloud), DualSense haptics and adaptive triggers, keyboard text entry,
multi-controller, and authenticated remote access (`--allow-remote` exposes the
API without authentication).

## Requirements

- Rust stable (edition 2024)
- A PS4 or PS5 on the same LAN with **Enable Remote Play** turned on
  (Settings → System → Remote Play)
- Your **PSN Account ID**. This is the numeric Account ID shown in PSN account
  info, not your PSN username. You can enter it as a decimal number (converted
  to Sony's 8-byte little-endian form automatically), a hex value, or an
  existing base64 Account ID. If you only know your PSN username, use the
  built-in **Sign in to PSN** helper in the registration form: it runs the same
  OAuth flow as Chiaki's `scripts/psn-account-id.py` and fills in the Account
  ID for you. PS4 system software older than 7.0 can use the PSN Online ID
  instead.
- A browser with H.264 support in WebRTC (Chrome, Edge, Safari, Firefox)

## Build and run

```sh
cargo build --release
./target/release/ps-remoteplay
```

Then open <http://127.0.0.1:8080/>.

The server handles both `Ctrl+C` and `SIGTERM`. If a session is active, it asks
the console to stop before exiting, which matters when it runs under systemd,
launchd, or a Docker stop.

Options:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--bind <addr>` | `127.0.0.1:8080` | Address for the web UI. Non-loopback addresses are refused unless `--allow-remote` is also given. |
| `--allow-remote` | off | Permit a non-loopback bind. The API and stream control have no authentication. |
| `--config-dir <dir>` | platform config dir | Override where `hosts.json` is stored. |
| `--demo-file <file>` | — | Enable the demo endpoint with a local Annex-B H.264 stream. |
| `--log-file <path>` | `<config dir>/server.log` | Also write logs to this file (stderr always gets them too). |

The configuration directory is `$XDG_CONFIG_HOME/ps-remoteplay` (or the
platform equivalent, e.g. `~/Library/Application Support/ps-remoteplay` on
macOS). It is created with mode `0700`, and `hosts.json` with mode `0600`,
because it contains console registration keys.

## Registering a console

1. On the console, open **Settings → System → Remote Play** and note the PIN.
2. In the web UI, expand **Register a console**, enter the console IP, pick the
   console type (PS5, PS4 8.0+, PS4 7.0, or older PS4), the PIN, and either the
   PSN Account ID (decimal, hex, or base64) or (PS4 pre-7.0) the PSN Online ID.
   A decimal Account ID is converted the same way Chiaki's helper does it.
   If you only know your PSN username, click **Sign in to PSN**, log in, copy
   the URL from the address bar after the redirect page, paste it into the
   redirect field, and press **Get ID**. The Account ID field is filled in
   automatically.
3. If a direct request is rejected, enable **Search by broadcast** and retry.
4. After registering, the console appears in the list. Use **Scan** to find it
   over discovery, wake it if needed, then start streaming.

The registration PIN is a one-time pairing code and is **not** stored. What
pairing produces — the console's RegistKey, RP-Key, MAC and nickname — is saved
in `hosts.json` and reused for every later session, so you do not pair again.

If the console asks for a PIN when starting a stream, that is a **session login
PIN** generated by the console and displayed on it; it changes per request and
cannot be reused or saved. Enter it in the in-page modal. (Its wording says
"console login", not registration.)

## Demo mode

To verify the browser/WebRTC path without a console:

```sh
# Any Annex-B H.264 elementary stream works; this creates a small test clip.
ffmpeg -f lavfi -i testsrc=size=1280x720:rate=30 -t 20 \
       -c:v libx264 -profile:v baseline -pix_fmt yuv420p -f h264 demo.h264

cargo run -p ps-remoteplay-server -- --demo-file demo.h264
```

Open the UI and press **Start demo stream** (or `POST /api/demo/start`). The
browser should show the test pattern with no console present; stats update
live.

## Video quality

- **Bitrate** is the main lever. Presets use 10 Mbps at 720p and 15 Mbps at
  1080p; try 20000–30000 for 1080p. Higher bitrate preserves detail and motion
  sharpness at the cost of Wi-Fi/network bandwidth. Leave the field empty to
  use the preset value.
- **Sharpness** (0–100, default 35) runs a two-pass WebGL2 post-process over
  the received video: FSR 1 **EASU** reconstructs the frame at the display's
  native pixel grid (up to a 4K target), then **RCAS** sharpens it in linear
  light. It adds no bandwidth and works on any GPU, including integrated and
  Apple Silicon. Set it to 0 to disable sharpening; the color controls below
  still apply.
- **Brightness / Contrast / Saturation** (each −100–100; defaults +15/+20/+20)
  are graded in linear light inside the same pass. Brightness is exposure in
  ±1.5 EV, contrast pivots around middle gray, and saturation mixes around
  Rec.709 luma. **Reset picture** restores the defaults. Set every picture
  control to neutral (Sharpness 0, Brightness/Contrast/Saturation 0, Temporal
  0) to bypass the post-process entirely and display the WebRTC video directly.
  All values are stored per browser.
- **Temporal** (0–100, default 0) adds contrast-adaptive temporal accumulation
  before RCAS. It clamps the previous frame to the current 3×3 neighbourhood
  and reduces the blend where the correction is large, which settles
  compression shimmer and specular aliasing without motion-vector smearing at
  scene cuts. There are no engine motion vectors in a decoded stream, so very
  fast motion can still ghost slightly; raise it gradually.
- Sharpening cannot recover detail the console encoder discarded — pair it with
  a suitable bitrate. Resolution and FPS are encoded on the console.

## Configuration and privacy

- The server binds to `127.0.0.1` by default and refuses a non-loopback bind
  unless `--allow-remote` is passed. There is no token or TLS, so only enable
  that on a network you trust.
- Loopback alone is not an origin boundary, so every request must carry a
  loopback `Host` (plus the bind address if you deliberately bind elsewhere).
  When a browser sends `Origin`, it must be this server's own loopback origin.
  This blocks cross-site WebSocket control and DNS-rebinding attacks against
  the JSON API.
- Console regist keys and keys are stored locally in `hosts.json`. Treat that
  file as a credential.
- Streaming, registration, and discovery traffic goes only to the console
  (TCP 9295, UDP 9296) and the local network (UDP 987/9302). The optional
  **Sign in to PSN** helper is the one exception: it contacts Sony's OAuth
  endpoints (`auth.api.sonyentertainmentnetwork.com`) to exchange the sign-in
  code for your account ID. Your password stays on Sony's page; this server
  never sees it, and the access token is discarded after the lookup.

## Architecture

```
crates/chiaki-core/
  crypto/      RPCrypt (AES-128-CFB), GKCrypt (AES-128-ECB + GMAC), ECDH
  regist.rs    registration payload + SRC search + HTTP POST
  discovery.rs discovery/wakeup packets
  session.rs   session request, supervisor, event/command channels
  ctrl/        control TCP channel (session id, PIN, heartbeat)
  takion/      UDP transport: handshake, MAC, reorder queue, retransmit
  stream/      launch spec, protobuf messages, stream connection
  media/       AV parsing, frame assembly, Cauchy Reed–Solomon FEC, audio
  feedback.rs  controller state/history encoding
crates/server/
  routes/      JSON API, WebSocket input/events, static UI
  webrtc/      peer setup, H.264/Opus bridges, IDR gating
  config.rs    persistent host store
  demo.rs      console-free H.264 demo path
```

The core uses one owner per stateful protocol object with bounded channels and
cancellation tokens; there are no shared protocol mutexes.

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The suite includes Chiaki's known-answer vectors for RPCrypt, GKCrypt, GMAC key
refresh, ECDH, FEC, and a captured AV packet, plus an end-to-end mock console
that exercises session → ctrl → Takion → encryption → launch spec → stream info
→ video/audio → clean stop.

## Limitations

- **H.264 only.** H.265/HDR profile negotiation is not implemented; PS5 streams
  are requested as H.264.
- **PS5 support is protocol 1.0 only**, matching Chiaki v2.2.0. Newer console
  firmware may change or reject the handshake.
- **No Senkusha** (PS5 cloud streaming) and no PSN cloud session support.
- **DualSense features are not forwarded.** Rumble is supported; haptics,
  adaptive triggers, touchpad gestures beyond basic touches, and the DualSense
  microphone are not.
- **Keyboard text entry** from the console is consumed but not surfaced in the
  browser.
- **One active session** at a time and **localhost only**; no multiuser auth.
  A started session waits 30 seconds for the browser's WebRTC peer and stops
  itself if it never connects (for example, if the tab died before answering).
- Release builds use `panic = "abort"`: an unexpected panic in any task ends
  the whole bridge rather than leaving a half-working stream. Run it under a
  process supervisor for unattended use; `SIGTERM` shuts down cleanly.
- The PSN sign-in helper uses Sony's legacy Remote Play OAuth client and may
  stop working if Sony changes those endpoints, just like Chiaki's helper.
- Video is forwarded as complete access units; a lost slice or a frame that
  fails FEC is dropped and the client requests a new IDR. Expect a short
  recovery hitch under heavy packet loss.
- Audio is Opus as sent by the console; no transcoding or volume normalization.
- The console may refuse a stream while another Remote Play client (including
  the official app) is connected.

## Troubleshooting

- **Registration fails with a direct request**: use **Search by broadcast**, and
  verify the console type (PS4 pre-8.0 uses a different registration path).
- **PIN prompt never appears**: the console shows the PIN dialog when a session
  starts; if you miss it, stop and start the session again.
- **Black video / no frames**: check the log for `detected missing/corrupt
  frames` or FEC failures. The bridge requests an IDR after a gap; if the
  console does not send one, stop and restart the stream.
- **No audio in Safari**: Safari requires the page to be interacted with before
  starting audio; click the page once, then start the stream.
- **Console not found by Scan**: discovery uses UDP broadcast; some networks
  block it. Register by IP instead.
- **“Remote Play on Console has crashed” (0x80108b15)**: this is the console's
  own Remote Play server crashing, not a pairing failure; the official app
  fails the same way until it restarts. Put the console into rest mode and
  wake it again (the **Wake** button in the console list), or restart it.
  Leave **DualSense feedback (experimental)** off: extended DualSense features
  are a known trigger for this PS5 crash.

Registration failures include the console's HTTP status and `RP-Application-Reason`
(for example "Regist failed, probably invalid PIN"). The same details are written
to `server.log` in the configuration directory, alongside stderr.

If you enable WebRTC logs (`RUST_LOG=rtc=warn`), you may see
`IceHandler.handle_read got error: password is empty` and DTLS "Unsupported
Extension"/mDNS warnings. They come from the first STUN checks arriving before
the browser's answer is installed in non-trickle signaling and are harmless;
they are hidden at the default log level.

Set `RUST_LOG=debug` (or `RUST_LOG=chiaki_core=debug`) for protocol-level logs.

## Licensing

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License, version 3 only, as
published by the Free Software Foundation. See [COPYING](COPYING).

It is a derivative work of **Chiaki v2.2.0** (AGPL-3.0-only-OpenSSL),
Copyright (C) the Chiaki contributors, and includes protocol structures and
cryptographic key tables generated from that source. Chiaki:
<https://git.sr.ht/~thestr4ng3r/chiaki>.

The browser sharpening pass is ported from **AMD FidelityFX Super Resolution
1.0** (EASU and RCAS), Copyright (c) 2021 Advanced Micro Devices, Inc.,
released under the MIT licence. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

If you run a modified version of this program as a network service, the AGPL
requires you to offer users the corresponding source code.

PlayStation, PS4, PS5, PSN and Remote Play are trademarks of Sony Interactive
Entertainment. This project is unofficial and is not affiliated with or
endorsed by Sony.
