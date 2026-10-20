# Changelog

All notable changes to Setu are documented here. The project follows semantic
versioning; it is a Chiaki derivative and remains AGPL-3.0-only.

## [0.5.1] - 2026-10-19

### Changed

- Takion UDP sockets request 4 MiB send/receive buffers (best-effort; the OS
  may cap them) and the receive loop drains up to 64 queued datagrams per
  wake-up instead of one, so a burst no longer overflows the kernel queue while
  a packet is being assembled.
- A run of lost control acknowledgements no longer ends the session. Stuck
  control packets are dropped after 25 retransmissions, while a separate
  inbound-silence watchdog fails the session only after 15 seconds with no
  packets from the console at all. Media can continue through a short ACK-loss
  burst that previously killed a live stream after roughly five seconds.
- Adaptive bitrate now also reacts to a single one-second loss burst at or
  above 5%, not only to the session average. Stepping back up requires both a
  low average and no burst.

## [0.5.0] - 2026-10-19

### Added

- **Configurable keyboard bindings and profiles.** Every button, trigger, and
  left-stick direction can be rebound in **Settings → Input**, cleared, or
  reset. A key can drive only one action; moving it releases the previous
  owner. Profiles are named, duplicated with **New**, deleted, and saved in
  browser storage.
- **H.265/HEVC delivery.** PS5 sessions can negotiate Main profile H.265 or
  Main10 HDR. The Stream settings offer the codec only when the browser
  advertises hardware HEVC over WebRTC, the server offers a single video codec
  per peer, and the SDP answer is checked for that codec before the stream
  starts. PS4 remains H.264-only.
- **Adaptive bitrate across sessions.** Packet loss is measured after a
  warm-up, reported live in the player stats, and persisted per console. A
  session at or above 1.5% loss lowers the next session's starting bitrate one
  ladder step; two sessions at or below 0.3% raise it. The launch spec fixes
  the rate for one session, so changes apply on the next connection.
- Keyboard and stream settings (resolution, FPS, bitrate, codec, adaptive
  toggle) are remembered in the browser.

### Changed

- The WebSocket input path resolves the current session's channels per
  message, so a replaced session can never leave it sending into a closed
  channel.

### Documentation

- README and UI explain browser/console HEVC requirements, HDR's Main10 and
  HDR-display requirement, and the between-sessions limit of adaptive bitrate.

## [0.4.4] - 2026-10-19

### Added

- **Resync** in the player bar requests a fresh keyframe, so a corrupt or
  stalled picture can recover without restarting the stream.
- **Rest mode** in the player bar puts the console to sleep after a
  confirmation.
- Adaptive trigger packets are mapped to the Gamepad API's `trigger-rumble`
  effect when the connected browser and controller support it.
- Motion controls read `Gamepad.pose` gyro, accelerometer, and orientation
  data, convert acceleration to g, normalize the orientation, and send it in
  the input stream. A Settings → Input toggle disables it. Current Chrome does
  not expose DualSense pose through the Gamepad API, so this path activates
  only in browsers that provide it.

### Fixed

- Dual-rumble and trigger-rumble promise rejections are caught.

## [0.4.3] - 2026-10-19

### Changed

- Console pairing and login PIN fields are masked by default. An eye button
  reveals the digits locally, and both fields return to masked whenever the
  panel or modal is reopened.

## [0.4.2] - 2026-10-19

### Fixed

- A rapid double-click on a console tile can no longer start two sessions and
  let the loser tear down the winner's player view.
- A `quit` event from an earlier session no longer cleans up a newly started
  session; events now carry and verify the session id.
- The event WebSocket reconnects after a transient close, and WebRTC
  connection failures perform an ICE restart before the session is abandoned.
  ICE restart attempts are capped even when renegotiation itself succeeds.
- A strict Content-Security-Policy and related security headers are sent with
  every response; `connect-src` is limited to the page's own origin.
- An expired auth cookie during the detached-session status poll now closes
  the stale player view instead of leaving it behind the login modal.
- Failed logins are counted and throttled per source address, like protected
  API requests.
- Protobuf length fields use checked arithmetic, so a hostile varint cannot
  panic a debug build.
- Frame allocation validates packet geometry before committing state, and the
  ingest loop no longer advances the frame index before allocation succeeds.
- Frame assembly grows its per-unit buffer when a later unit (for example a FEC
  unit, or a source unit delivered after UDP reordering) is larger than the
  first packet, and late units after an early flush are ignored instead of
  producing a warning per packet. A forged padding field can no longer reserve
  more than 8 MiB for one frame.
- Pointer-lock and fullscreen promise rejections are handled instead of
  surfacing as uncaught page errors.
- NAL inspection is codec-aware (H.264 IDR/SPS/PPS and H.265 IRAP/VPS/SPS/PPS);
  the start endpoint rejects H.265 with a clear error because the WebRTC bridge
  is H.264-only.
- The ctrl receive counter advances even when a payload fails to decrypt, so a
  bad frame cannot silently desynchronize every later ctrl message.
- A console that stops acknowledging Takion DATA or goes silent on the control
  channel now fails the session instead of hanging forever.
- Failed Senkusha DATA-ACK waits are reported immediately instead of degrading
  into a later generic timeout.

### Changed

- WebRTC peers close their UDP sockets if setup fails after the peer is built.
  A losing start race releases the console session gracefully, and dropping a
  ctrl or Takion handle now stops its driver task and socket.
- The session manager is supervised: a panic cancels the bridge tasks, closes
  the peer, clears the active slot, and notifies the browser instead of leaving
  the bridge wedged or leaking the stats task.
- Graceful HTTP shutdown now closes upgraded WebSockets.
- Authentication cookies contain random, revocable server-side session ids
  instead of a static token hash. Logout revokes the session, and repeated
  failed authentication attempts from one address are delayed.
- CI now tests Linux, macOS, and Windows; checks JavaScript syntax, the pinned
  Rust 1.97 MSRV, and dependency advisories; and pins the CI toolchain.
- The release workflow now runs a mandatory format, clippy, test, JavaScript,
  and license quality gate before building or publishing any artifact.
- README corrections for Senkusha, UDP 9297, H.265, authentication, and panic
  behavior.

## [0.4.1] - 2026-10-19

### Changed

- Rebuilt the home screen as a PlayStation-style tile grid. **Add console** is
  the first tile, followed by paired consoles and any consoles found on the
  network. Tiles show an abstract per-console artwork, type badge, name, and
  address; hovering or focusing a paired tile reveals Play.
- Added a refined Setu mark. Context messages and session notices now appear as
  toasts in the bottom-right instead of under the logo or at the top of the
  page.
- **Add console** is now a non-modal glass panel that slides in from the right.
  It opens automatically on first load when no paired or nearby consoles are
  found. Console type is a radio group, with inline **?** help for finding the
  console address and the pairing PIN.
- PSN account fields are always visible instead of collapsed. A previously
  saved PSN Account ID is restored automatically. The PSN sign-in modal and
  Sign-In ID field were removed; paste a PSN redirect URL or code into the
  inline field and the Account ID is fetched automatically. A **Get a code**
  link beside the field opens Sony's sign-in page and explains the redirect
  copy step. An optional **Public Lookup** by PSN Online ID uses
  `psn.flipscreen.games` and requires PSN privacy settings that allow public
  search.
- The top-right gear now opens **Settings** directly in a glass panel that
  slides in from the right. **Lock** is a separate top-right icon, and the
  network refresh button sits at the right of the **Your consoles** heading.
- Discovery now runs automatically when the home screen loads and refreshes
  after adding or removing a console.

## [0.4.0] - 2026-10-19

### Added

- Optional token authentication for the JSON API and the session WebSocket
  (`--auth-token`, `--auth-token-file`), with an HttpOnly/SameSite cookie,
  bearer-token support for scripts, and a Lock/Unlock UI.
- Automatic console-type detection from the SRC discovery response, including
  a unicast **Detect** action for manually entered IPs and a manual override.
- A PSN Account ID can be entered directly and is remembered per
  browser/device in `localStorage`, with a **Forget** control.
- Prebuilt GitHub Release binaries for macOS arm64/x64, Linux x86_64 (static
  musl), and Windows x86_64.
- Redesigned the UI around a home screen and an immersive player: an
  Add console panel, a three-tab Settings panel, and an auto-hiding player bar.
  The left sidebar is gone; streaming controls stay out of the way and the
  video fills the window.
- LAN access documentation, including the trusted-network warning and the
  no-TLS caveat.
- Legal and provenance documentation, a UI footer disclaimer, `SECURITY.md`,
  `CONTRIBUTING.md`, and a generated dependency-license inventory.
- CI for formatting, clippy, tests, release builds, and license-inventory drift.

### Changed

- Renamed the project, binary, crate, config directory, browser storage keys,
  and UI title to **Setu**. Existing `ps-remoteplay/hosts.json` registrations
  are copied into the new `setu` config directory on first run.

### Fixed

- The demo `ffmpeg` command now emits AUD NALs so generated test clips advance
  instead of appearing as a single static frame.

## [0.2.5] - 2026-10-18

- Added contrast-adaptive temporal upscaling alongside the FSR1 EASU/RCAS pass.

## [0.2.4] - 2026-10-18

- Quality pipeline, localhost hardening, latency reporting, and session
  lifecycle fixes.
