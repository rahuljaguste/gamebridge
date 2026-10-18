// SPDX-License-Identifier: AGPL-3.0-only
//! Owns a live session and the WebRTC bridges for its lifetime.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use chiaki_core::error::QuitReason;
use chiaki_core::media::audio::AudioFrame;
use chiaki_core::media::ingest::VideoFrame;
use chiaki_core::session::{Session, SessionCommand, SessionEvent, SessionPhase};

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
    frame_duration: Duration,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let bridge_cancel = CancellationToken::new();

    let forwarder_cancel = bridge_cancel.clone();
    let forwarder_shared = shared.clone();
    let forwarder_state = state.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = forwarder_cancel.cancelled() => break,
                event = events_rx.recv() => {
                    let Some(event) = event else { break };
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
    let stats_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stats_cancel.cancelled() => break,
                _ = ticker.tick() => {
                    // `get(false)` is the 200 ms congestion window; the UI
                    // needs the cumulative totals instead.
                    let (received, lost) = stats.totals();
                    let latency_ms = *stats_shared.latency_ms.lock().unwrap();
                    stats_state.publish(ServerEvent::Stats { received, lost, latency_ms });
                }
            }
        }
    });

    let outcome = tokio::select! {
        _ = &mut stop_rx => {
            let _ = session.send_command(SessionCommand::Stop);
            timeout(Duration::from_secs(5), session.wait())
                .await
                .unwrap_or_default()
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
            let mut outcome = timeout(Duration::from_secs(5), session.wait())
                .await
                .unwrap_or_default();
            if outcome.quit_reason.is_none() {
                outcome.quit_reason = Some(QuitReason::Stopped);
            }
            if outcome.reason_str.is_none() {
                outcome.reason_str = Some(detail);
            }
            outcome
        }
        outcome = session.wait() => outcome,
    };

    bridge_cancel.cancel();
    let _ = forwarder.await;
    let _ = video.await;
    let _ = audio.await;
    let _ = stats_task.await;
    let _ = peers.peer.close().await;
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

fn forward_event(event: SessionEvent, shared: &MediaShared) -> Option<ServerEvent> {
    match event {
        SessionEvent::Phase(phase) => Some(ServerEvent::Phase {
            phase: phase_string(phase).to_string(),
        }),
        SessionEvent::LoginPinRequest { incorrect } => Some(ServerEvent::PinRequest { incorrect }),
        SessionEvent::Connected => Some(ServerEvent::Connected),
        SessionEvent::ConnectionQuality { rtt_ms } => {
            *shared.latency_ms.lock().unwrap() = Some(rtt_ms);
            None
        }
        SessionEvent::StreamInfo { profiles, audio } => {
            *shared.audio_format.lock().unwrap() = Some(audio);
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
                *shared.param_sets.lock().unwrap() = vec![header];
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
