// SPDX-License-Identifier: AGPL-3.0-only
//! Owns a live session and the WebRTC bridges for its lifetime.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::broadcast;
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
    mut stop_rx: oneshot::Receiver<()>,
) {
    let events_tx = state.events.clone();
    let bridge_cancel = CancellationToken::new();

    let forwarder_cancel = bridge_cancel.clone();
    let forwarder_shared = shared.clone();
    let forwarder_tx = events_tx.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = forwarder_cancel.cancelled() => break,
                event = events_rx.recv() => {
                    let Some(event) = event else { break };
                    forward_event(event, &forwarder_tx, &forwarder_shared);
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
    let stats_tx = events_tx.clone();
    let stats_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stats_cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let (received, lost) = stats.get(false);
                    let _ = stats_tx.send(ServerEvent::Stats { received, lost });
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
    let _ = events_tx.send(ServerEvent::Quit {
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
    let _ = peers.connected.load(Ordering::Relaxed);
}

fn forward_event(event: SessionEvent, tx: &broadcast::Sender<ServerEvent>, shared: &MediaShared) {
    let out = match event {
        SessionEvent::Phase(phase) => Some(ServerEvent::Phase {
            phase: phase_string(phase).to_string(),
        }),
        SessionEvent::LoginPinRequest { incorrect } => Some(ServerEvent::PinRequest { incorrect }),
        SessionEvent::Connected => Some(ServerEvent::Connected),
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
    };
    if let Some(out) = out {
        let _ = tx.send(out);
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
