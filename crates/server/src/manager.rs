// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Owns a live session and the WebRTC bridges for its lifetime.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use chiaki_core::error::QuitReason;
use chiaki_core::media::audio::AudioFrame;
use chiaki_core::media::ingest::VideoFrame;
use chiaki_core::session::{Session, SessionCommand, SessionEvent, SessionOutcome, SessionPhase};

use crate::adaptive::{self, AdaptiveContext, SessionQuality};
use crate::state::{AppState, ProfileView, ServerEvent};
use crate::webrtc::audio_bridge;
use crate::webrtc::video_bridge;
use crate::webrtc::{MediaShared, PeerHandles};

/// A real session owns the console from `Session::start`. If the browser dies
/// before posting its WebRTC answer, or disappears after connecting, nothing
/// else releases that console.
const BROWSER_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const BROWSER_WS_GRACE: Duration = Duration::from_secs(10);
const BROWSER_DISCONNECT_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanReason {
    StartupTimeout,
    BrowserGone,
}

/// Resolves when the session has no usable browser: WebRTC never connected
/// within `startup`, the event WebSocket stayed closed for `ws_grace` after a
/// connection, or WebRTC stayed disconnected for `disconnect_grace`.
async fn browser_orphan_watchdog(
    connected: Arc<AtomicBool>,
    ws_clients: Arc<AtomicUsize>,
    startup: Duration,
    ws_grace: Duration,
    disconnect_grace: Duration,
) -> OrphanReason {
    let deadline = tokio::time::Instant::now() + startup;
    while !connected.load(Ordering::Relaxed) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return OrphanReason::StartupTimeout;
        }
        tokio::time::sleep_until((now + Duration::from_millis(100)).min(deadline)).await;
    }

    let mut ws_absent_since = None;
    let mut disconnected_since = None;
    loop {
        let now = tokio::time::Instant::now();
        if ws_clients.load(Ordering::Relaxed) == 0 {
            let since = *ws_absent_since.get_or_insert(now);
            if now - since >= ws_grace {
                return OrphanReason::BrowserGone;
            }
        } else {
            ws_absent_since = None;
        }
        if connected.load(Ordering::Relaxed) {
            disconnected_since = None;
        } else {
            let since = *disconnected_since.get_or_insert(now);
            if now - since >= disconnect_grace {
                return OrphanReason::BrowserGone;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    state: Arc<AppState>,
    id: Uuid,
    mut session: Session,
    mut events_rx: mpsc::UnboundedReceiver<SessionEvent>,
    video_rx: mpsc::Receiver<VideoFrame>,
    audio_rx: mpsc::Receiver<AudioFrame>,
    peers: PeerHandles,
    shared: Arc<MediaShared>,
    bridge_cancel: CancellationToken,
    frame_duration: Duration,
    adaptive_ctx: Option<AdaptiveContext>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let stream_started: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let forwarder_cancel = bridge_cancel.clone();
    let forwarder_shared = shared.clone();
    let forwarder_state = state.clone();
    let forwarder_started = stream_started.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = forwarder_cancel.cancelled() => break,
                event = events_rx.recv() => {
                    let Some(event) = event else { break };
                    if matches!(
                        &event,
                        SessionEvent::Connected | SessionEvent::Phase(SessionPhase::Streaming)
                    ) {
                        let mut started = forwarder_started
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        started.get_or_insert_with(Instant::now);
                    }
                    if let Some(out) = forward_event(event, &forwarder_shared) {
                        forwarder_state.publish(out);
                    }
                }
            }
        }
    });

    let video_cancel = bridge_cancel.clone();
    let commands = session.commands.clone();
    let video = tokio::spawn(video_bridge::run(
        video_rx,
        peers.video.clone(),
        peers.connected.clone(),
        shared.clone(),
        commands,
        frame_duration,
        video_cancel,
    ));

    let audio_cancel = bridge_cancel.clone();
    let audio = tokio::spawn(audio_bridge::run(
        audio_rx,
        peers.audio.clone(),
        shared.clone(),
        audio_cancel,
    ));

    let stats_cancel = bridge_cancel.clone();
    let stats = session.stats.clone();
    let stats_shared = shared.clone();
    let stats_state = state.clone();
    let quality = Arc::new(Mutex::new(SessionQuality::new()));
    let stats_quality = quality.clone();
    let stats_started = stream_started.clone();
    let stats_task = tokio::spawn(async move {
        let video_queue_capacity = stats_state.video_queue;
        let mut queue_peak_max = 0usize;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stats_cancel.cancelled() => break,
                _ = ticker.tick() => {
                    // `get(false)` is the 200 ms congestion window; the UI
                    // needs the cumulative totals instead.
                    let (received, lost) = stats.totals();
                    let started = *stats_started
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let loss_pct = {
                        let mut quality = stats_quality
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        if let Some(started) = started {
                            quality.mark_started(started);
                        }
                        quality.sample(received, lost, Instant::now());
                        quality.loss_pct()
                    };
                    let latency_ms = *stats_shared.latency_ms.lock().unwrap_or_else(|error| error.into_inner());
                    let video_queue_depth = stats_shared
                        .video_queue_depth
                        .load(Ordering::Relaxed);
                    let video_queue_peak = stats_shared
                        .video_queue_peak
                        .swap(0, Ordering::Relaxed);
                    queue_peak_max = queue_peak_max.max(video_queue_peak);
                    stats_state.publish(ServerEvent::Stats {
                        received,
                        lost,
                        loss_pct,
                        latency_ms,
                        video_queue_depth,
                        video_queue_peak,
                        video_queue_capacity,
                    });
                }
            }
        }
        tracing::info!("video queue peak: {queue_peak_max}/{video_queue_capacity} frames");
    });

    let outcome = tokio::select! {
        _ = &mut stop_rx => {
            let _ = session.send_command(SessionCommand::Stop);
            match timeout(Duration::from_secs(5), session.wait()).await {
                Ok(outcome) => normalize_outcome(outcome),
                Err(_) => {
                    tracing::warn!("session did not stop within 5 s");
                    SessionOutcome {
                        quit_reason: Some(QuitReason::StreamConnectionUnknown),
                        error: Some("session did not stop within 5 s".into()),
                        ..Default::default()
                    }
                }
            }
        }
        reason = browser_orphan_watchdog(
            peers.connected.clone(),
            state.ws_clients.clone(),
            BROWSER_CONNECT_TIMEOUT,
            BROWSER_WS_GRACE,
            BROWSER_DISCONNECT_GRACE,
        ) => {
            let detail = match reason {
                OrphanReason::StartupTimeout => format!(
                    "browser did not connect within {} s",
                    BROWSER_CONNECT_TIMEOUT.as_secs()
                ),
                OrphanReason::BrowserGone => "browser disconnected".to_string(),
            };
            tracing::warn!("{detail}; stopping the session");
            let _ = session.send_command(SessionCommand::Stop);
            let mut outcome = match timeout(Duration::from_secs(5), session.wait()).await {
                Ok(outcome) => normalize_outcome(outcome),
                Err(_) => {
                    tracing::warn!("session did not stop within 5 s after the watchdog tripped");
                    SessionOutcome {
                        quit_reason: Some(QuitReason::StreamConnectionUnknown),
                        error: Some("session stop timed out".into()),
                        ..Default::default()
                    }
                }
            };
            if outcome.reason_str.is_none() {
                outcome.reason_str = Some(detail);
            }
            outcome
        }
        outcome = session.wait() => normalize_outcome(outcome),
    };

    bridge_cancel.cancel();
    stop_task("event forwarder", forwarder).await;
    stop_task("video bridge", video).await;
    stop_task("audio bridge", audio).await;
    stop_task("stats task", stats_task).await;
    let (loss_pct, peak_loss_pct, packets, elapsed) = {
        let quality = quality.lock().unwrap_or_else(|error| error.into_inner());
        (
            quality.overall_loss_pct(),
            quality.peak_loss_pct(),
            quality.observed_packets(),
            quality.elapsed(),
        )
    };
    if let (Some(context), Some(loss)) = (adaptive_ctx.as_ref(), loss_pct) {
        let peak = peak_loss_pct.unwrap_or(loss);
        if let Some(message) =
            adaptive::record(&state.config, context, loss, peak, packets, elapsed)
        {
            tracing::info!("adaptive bitrate: {message}");
            // Publish before Quit so the browser still has the session view up.
            state.publish(ServerEvent::Notice { message });
        }
    }
    if timeout(Duration::from_secs(3), peers.peer.close())
        .await
        .is_err()
    {
        tracing::warn!("WebRTC peer did not close within 3 s");
    }
    session.stop().await;

    let reason = outcome.reason();
    let detail = if reason == QuitReason::SessionRequestRpCrash {
        Some(
            "Remote Play on the console has crashed. Put the console into rest mode \
             and wake it (or restart it), then try again."
                .to_string(),
        )
    } else {
        outcome.error.clone().or(outcome.reason_str.clone())
    };
    if reason.is_error() {
        tracing::warn!(
            "session ended: {reason} (reason: {:?}, error: {:?})",
            outcome.reason_str,
            outcome.error
        );
    } else {
        tracing::info!("session ended: {reason}");
    }
    state.publish(ServerEvent::Quit {
        session_id: id.to_string(),
        reason: reason.as_str().to_string(),
        reason_string: detail.clone(),
    });

    // The browser may still be gathering ICE candidates when the session
    // dies. Keep the real reason so /api/session/answer can report it.
    {
        let mut last = state.last_session_error.lock().await;
        *last = if reason.is_error() {
            Some(match detail.as_deref() {
                Some(detail) if !detail.is_empty() => {
                    format!("{}: {detail}", reason.as_str())
                }
                _ => reason.as_str().to_string(),
            })
        } else {
            None
        };
    }

    let mut active = state.active.lock().await;
    if active.as_ref().map(|a| a.id) == Some(id) {
        *active = None;
    }
}

/// A session that ends without an outcome (supervisor panic/abort) must not
/// be reported as a clean stop.
fn normalize_outcome(mut outcome: SessionOutcome) -> SessionOutcome {
    if outcome.quit_reason.is_none() {
        outcome.quit_reason = Some(QuitReason::StreamConnectionUnknown);
    }
    outcome
}

/// Wait briefly for a bridge task, then abort it. A track writer can stall
/// forever if the peer's transport is wedged; teardown must not inherit that.
async fn stop_task(name: &'static str, mut handle: tokio::task::JoinHandle<()>) {
    const GRACE: Duration = Duration::from_secs(2);
    if timeout(GRACE, &mut handle).await.is_err() {
        tracing::warn!("{name} did not stop within {GRACE:?}; aborting it");
        handle.abort();
    }
}

fn forward_event(event: SessionEvent, shared: &MediaShared) -> Option<ServerEvent> {
    match event {
        SessionEvent::Phase(phase) => Some(ServerEvent::Phase {
            phase: phase_string(phase).to_string(),
        }),
        SessionEvent::LoginPinRequest { incorrect } => Some(ServerEvent::PinRequest { incorrect }),
        SessionEvent::Connected => Some(ServerEvent::Connected),
        SessionEvent::ConnectionQuality { rtt_ms } => {
            *shared
                .latency_ms
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(rtt_ms);
            None
        }
        SessionEvent::StreamInfo { profiles, audio } => {
            *shared
                .audio_format
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(audio);
            Some(ServerEvent::StreamInfo {
                profiles: profiles
                    .iter()
                    .map(|p| ProfileView {
                        width: p.width,
                        height: p.height,
                    })
                    .collect(),
                audio: crate::state::AudioView {
                    channels: audio.channels,
                    bits: audio.bits,
                    sample_rate: audio.sample_rate,
                    frame_size: audio.frame_size,
                },
            })
        }
        SessionEvent::VideoFormat {
            profile_index,
            width,
            height,
            header,
        } => {
            if !header.is_empty() {
                *shared
                    .param_sets
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = vec![header];
            }
            Some(ServerEvent::VideoFormat {
                profile_index,
                width,
                height,
            })
        }
        SessionEvent::Rumble {
            unknown,
            left,
            right,
        } => Some(ServerEvent::Rumble {
            unknown,
            left,
            right,
        }),
        SessionEvent::TriggerEffects {
            type_left,
            type_right,
            left,
            right,
        } => Some(ServerEvent::TriggerEffects {
            type_left,
            type_right,
            left,
            right,
        }),
        SessionEvent::RemoteDisconnect { reason } => Some(ServerEvent::RemoteDisconnect { reason }),
        SessionEvent::Error(message) => Some(ServerEvent::Error { message }),
        SessionEvent::Quit { .. } => None,
    }
}

fn phase_string(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::RequestingSession => "requesting_session",
        SessionPhase::StartingCtrl => "starting_ctrl",
        SessionPhase::WaitingPin => "waiting_pin",
        SessionPhase::StartingStream => "starting_stream",
        SessionPhase::Streaming => "streaming",
        SessionPhase::Stopped => "stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(value: bool) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(value))
    }

    fn clients(value: usize) -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(value))
    }

    #[test]
    fn missing_outcome_is_an_error() {
        let outcome = normalize_outcome(SessionOutcome::default());
        assert_eq!(outcome.reason(), QuitReason::StreamConnectionUnknown);
        assert!(outcome.reason().is_error());
    }

    #[tokio::test]
    async fn watchdog_times_out_without_webrtc() {
        let started = tokio::time::Instant::now();
        let reason = browser_orphan_watchdog(
            flag(false),
            clients(1),
            Duration::from_millis(50),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(reason, OrphanReason::StartupTimeout);
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn watchdog_stays_pending_while_the_browser_is_present() {
        let result = timeout(
            Duration::from_millis(150),
            browser_orphan_watchdog(
                flag(true),
                clients(1),
                Duration::from_millis(20),
                Duration::from_millis(500),
                Duration::from_millis(500),
            ),
        )
        .await;
        assert!(result.is_err(), "a present browser must not be stopped");
    }

    #[tokio::test]
    async fn watchdog_reports_a_closed_websocket() {
        let reason = browser_orphan_watchdog(
            flag(true),
            clients(0),
            Duration::from_millis(10),
            Duration::from_millis(50),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(reason, OrphanReason::BrowserGone);
    }

    #[tokio::test]
    async fn watchdog_reports_a_lost_webrtc_peer() {
        let connected = flag(true);
        let lost = connected.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            lost.store(false, Ordering::Relaxed);
        });
        let reason = browser_orphan_watchdog(
            connected,
            clients(1),
            Duration::from_millis(10),
            Duration::from_secs(1),
            Duration::from_millis(60),
        )
        .await;
        assert_eq!(reason, OrphanReason::BrowserGone);
    }

    #[tokio::test]
    async fn watchdog_observes_a_late_connect() {
        let connected = flag(false);
        let late = connected.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            late.store(true, Ordering::Relaxed);
        });
        let result = timeout(
            Duration::from_millis(200),
            browser_orphan_watchdog(
                connected,
                clients(1),
                Duration::from_millis(500),
                Duration::from_millis(500),
                Duration::from_millis(500),
            ),
        )
        .await;
        assert!(result.is_err(), "a late connect must cancel the deadline");
    }
}
