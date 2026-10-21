# Changelog

All notable changes to GameBridge are documented here. The project follows
semantic versioning; it is a Chiaki derivative and remains AGPL-3.0-only.

## [0.8.5] - 2026-10-21

### Fixed

- The Linux musl release binary is built as a conventional static executable
  instead of static-PIE, avoiding a `_start` segfault seen with musl-gcc
  wrappers in the release smoke test.

## [0.8.4] - 2026-10-21

### Fixed

- Release builds now install the cross-compilation target explicitly and pin
  the toolchain to 1.97.1, fixing missing `core` errors on the Linux musl and
  macOS Intel release jobs.

## [0.8.3] - 2026-10-21

### Documentation

- Link the project source and AGPL-3.0-only license from the home screen,
  Settings drawer, and touch stream menu so network users can find the
  Corresponding Source.
- Add bug-report and feature-request issue templates, a pull-request checklist,
  and weekly Dependabot update configuration for Cargo and GitHub Actions.

## [0.8.2] - 2026-10-21

### Security

- Updated `rustls-webpki` to 0.103.16 and `webpki-roots` to 1.0.10 to address
  RUSTSEC-2026-0382 and RUSTSEC-2026-0381 (distrusted emSign intermediates).

## [0.8.1] - 2026-10-21

### Documentation

- Added a high-level runtime topology flowchart to the README as a PNG, with
  an editable SVG source. It covers browsers, the GameBridge process,
  chiaki-core, the console link, optional PSN services, and the future
  multi-source adapter boundary. The existing module layout is retained as the
  code-layout reference.

## [0.8.0] - 2026-10-21

### Added

- A dedicated **Menu** button on touch devices opens a stream menu with
  pause/resume, volume, mute, screenshot, resync, settings, fullscreen,
  controls on/off, rest mode, and stop. The menu lives inside the video
  container, so it works in fullscreen, and its backdrop prevents accidental
  controller input while it is open.

### Changed

- Touch streaming no longer shows the desktop player bar. This removes the
  bar-versus-controller overlap in fullscreen and makes every action reachable
  from the touch menu instead of a clipped horizontal strip.

## [0.7.3] - 2026-10-21

### Fixed

- Releasing a video touchpad contact outside the video no longer leaves a
  phantom finger held down when the browser drops pointer capture. Touchpad
  contacts are cleared by the same document-level pointer cleanup used by the
  virtual controller.

### Changed

- Share, PS, Options, and Recenter now use 44px-tall touch targets. The touch
  player bar moves down slightly to preserve the gap above it.
- The touch player bar shows a **Swipe →** hint while its actions overflow
  horizontally, making the off-screen buttons discoverable.

## [0.7.2] - 2026-10-21

### Fixed

- Touch sticks and the D-pad now read target-local pointer coordinates and
  fall back to drag deltas when the browser reports client coordinates in a
  different viewport space. This fixes Android Chrome fullscreen, where every
  stick direction previously resolved to up.

## [0.7.1] - 2026-10-21

### Fixed

- The player bar no longer covers the on-screen sticks or face buttons on
  phones and tablets. It docks at the top centre while the virtual controller
  is active, stays within the viewport, and scrolls horizontally. Transient
  toasts are moved clear of the bottom control clusters so they cannot
  intercept joystick or button touches.
- Touch sticks and the D-pad now follow document-level pointer events, so
  dragging outside the control or a browser that drops pointer capture no
  longer freezes or leaves the movement stuck. Release events are likewise
  guaranteed to clear the axis and button state.

## [0.7.0] - 2026-10-21

### Added

- Full on-screen controller for phones and tablets: left/right sticks, D-pad,
  face buttons, L1/R1/L2/R2, Options/Share/PS, and motion Recenter. The video
  remains a two-finger touchpad, and empty areas still send touchpad touches.
- Phone/tablet motion input from `DeviceMotion`/`DeviceOrientation`, including
  sensor permission handling, screen-orientation mapping, recenter, stale-data
  timeout, and Gamepad pose fallback.
- Touch controls auto-show on touch devices, with a persisted **Settings →
  Input** toggle and a player-bar show/hide button.
- Landscape-first touch UI with a non-blocking portrait rotate hint.
- An insecure-origin warning for plain HTTP on non-local origins, explaining
  that phones and tablets may block WebRTC, Gamepad, and motion sensors.

### Changed

- The motion setting is persisted and now covers both phone sensors and
  Gamepad pose.
- The player-bar auto-hide timer no longer hides while touch controls are held,
  and opening Settings exits fullscreen so the mobile toggles are reachable.
- Permissions-Policy explicitly allows accelerometer, gyroscope, and
  magnetometer for the page itself.
- The Host/Origin guard now accepts an HTTPS origin with the default port 443
  when the proxy preserves a port-less Host header.

### Documentation

- Added phone/tablet guidance, including the secure-context requirement for
  full WebRTC, Gamepad, and sensor support.

## [0.6.4] - 2026-10-21

### Added

- The server startup banner now prints reachable LAN URLs and a scannable QR
  code for the first address when bound to a LAN address or `0.0.0.0`. A
  loopback bind prints the command needed to enable LAN access instead.

## [0.6.3] - 2026-10-21

### Added

- On macOS, **Settings → Input** can reserve the controller PS/Home button for
  GameBridge by disabling the system Games app shortcut. The change is
  system-wide and may require logging out and back in. The toggle is shown only
  when the server runs on macOS.

## [0.6.2] - 2026-10-21

### Fixed

- The player's fullscreen button now changes to **Exit fullscreen** while
  fullscreen is active, and returns to **Fullscreen** when it closes.

### Documentation

- Added a macOS troubleshooting note for the PS/Home button being consumed by
  the Games app before the browser receives it.

## [0.6.1] - 2026-10-21

### Fixed

- Leaving or stopping a stream now exits browser fullscreen, rather than
  leaving the home screen in fullscreen mode.

## [0.6.0] - 2026-10-20

### Changed

- Renamed the project, binary, server crate, configuration directory, browser
  storage keys, auth cookie, UI, WebRTC track names, user agent, and release
  artifacts from Setu to GameBridge. The GitHub repository has been renamed to
  `gamebridge`; the old URL redirects.
- Existing `setu` registrations are copied into the new `gamebridge`
  configuration directory on first run, with `ps-remoteplay` as a fallback.
  `SETU_CONFIG_DIR` and `SETU_DUMP_STREAM` remain accepted as legacy aliases.
- Browser settings saved under `setu.*` are copied to `gamebridge.*` on first
  load, so volume, bindings, stream settings, and the PSN account ID survive
  the rename.
- GitHub releases now publish `gamebridge-<version>-source.tar.gz` containing
  the exact Corresponding Source for the binaries, alongside `COPYING` and the
  third-party notices.
- README now documents the multi-platform direction: PlayStation today, with
  Xbox, Steam/PC, and Nintendo under evaluation.

## [0.5.11] - 2026-10-20

### Fixed

- Gyro, accelerometer, and compressed orientation in feedback state are encoded
  little-endian like the console expects; they had been big-endian while the
  sticks were correctly big-endian. An idle controller therefore read as about
  +30 rad/s on every gyro axis and ~5 g on two accelerometer axes, so motion
  input could not work. A byte-exact test now pins the reference layout.

## [0.5.10] - 2026-10-20

### Fixed

- Audio controls no longer initialize before the shared `state` object,
  which caused `Cannot access 'state' before initialization` on page load.

## [0.5.9] - 2026-10-20

### Changed

- The Setu brand mark and favicon now use a gamepad silhouette with a
  D-pad and face buttons, while keeping the existing blue gradient tile.

## [0.5.8] - 2026-10-20

### Fixed

- Pause no longer calls `HTMLMediaElement.pause()`. It mutes the stream and
  covers the view while the element keeps draining WebRTC, preventing browser
  backpressure from filling the server's video queue and triggering avoidable
  frame-gap/IDR churn. Resume restores the saved mute state.

## [0.5.7] - 2026-10-20

### Added

- **Screenshot** in the player bar saves the current decoded frame as a PNG.
- **Pause/Resume** freezes or resumes this view locally; the console keeps
  running, and the player explains that before resuming to live video.
- After a remote disconnect or console standby, a player-view panel offers
  **Wake and resume** (reusing the automatic wake preflight) or **Back to
  consoles**.
- PSN sign-in opens in a popup and focuses the redirect field when it closes.
  Sony redirects to a cross-origin page, so the URL still has to be pasted;
  a popup cannot read it.

### Changed

- Rumble now holds the console's motor state with short overlapping effects
  until the console reports zero or the session ends, instead of replaying a
  fixed 120 ms burst per event.
- When ICE recovery reaches its retry limit, the browser now ends the session
  automatically after a short grace period instead of waiting for the user or
  the 30-second watchdog.

## [0.5.6] - 2026-10-20

### Added

- A stream volume slider and mute toggle in the player bar. **M** toggles mute
  while streaming and is skipped when the user has bound M to a controller
  action. Volume and mute state persist per browser.
- **Settings → Stream → Audio output** selects a speaker with
  `HTMLMediaElement.setSinkId` when the browser supports it (Chromium).
  The list refreshes on `devicechange`; Safari keeps the system output.

## [0.5.5] - 2026-10-20

### Added

- Starting a session now preflights the console. Discovery `Ready` proceeds
  immediately; `Standby` or an unreachable session port sends a wakeup packet,
  then polls TCP 9295 for up to 30 seconds before opening the session. An awake
  console with the port closed is not woken and still receives the normal
  session error.
- The connecting overlay explains that Setu wakes a sleeping console first.

## [0.5.4] - 2026-10-20

### Changed

- HEVC frames are considered self-contained only when VPS, SPS, and PPS are
  all present; H.264 requires SPS and PPS. Otherwise the bridge prepends the
  cached stream-info parameter sets before the access unit, so recovery after
  a gap cannot start with an incomplete set.
- Stream settings detect HEVC Main10 (`profile-id=2`) separately from H.265
  itself. HDR is disabled when the browser does not advertise Main10, and the
  hint notes when the current display does not report HDR and will tone-map.

## [0.5.3] - 2026-10-20

### Fixed

- H.265 offers no longer send an explicit `tx-mode=SRS`. Chrome advertises
  H.265 with SRST semantics and rejected the video m-line (`m=video 0`) when
  SRS was offered, so the browser either errored or connected audio-only.
  `tx-mode` is now omitted, which RFC 7798 defines as SRST; Chrome accepts
  both Main (`profile-id=1`) and Main10 HDR (`profile-id=2`).
- A WebRTC setup failure after a successful start response now stops the
  console session immediately instead of leaving it to the 30-second browser
  watchdog. The stop is sent only when this browser owns the session.
- The answer route logs whether the browser accepted or rejected the offered
  codec, making future codec negotiation failures visible in `server.log`.

## [0.5.2] - 2026-10-20

### Fixed

- Late FEC units that arrive after a frame has flushed are counted as
  received again for packet statistics. The late-unit handling introduced in
  0.4.2 correctly stopped them from modifying frame data, but also stopped
  counting them, so a clean stream could report up to 90% loss, send that
  figure to the console as congestion feedback, and drive adaptive bitrate
  down. Frame data is still never modified by late units.

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
