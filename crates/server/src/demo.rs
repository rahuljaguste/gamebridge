// SPDX-License-Identifier: AGPL-3.0-only
//! Optional demo mode: stream a local Annex-B H.264 file through the same
//! WebRTC bridge, without a console. Useful for verifying the browser path.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use axum::Json;
use axum::extract::State;
use bytes::Bytes;
use futures_util::FutureExt;
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use chiaki_core::controller::ControllerState;
use chiaki_core::error::QuitReason;
use chiaki_core::media::video::{annex_b_units, scan_annex_b};
use chiaki_core::session::SessionCommand;
use chiaki_core::target::Codec;

use crate::routes::api::{ApiError, RtcDescription, StartResponse};
use crate::state::{ActiveSession, AppState, ServerEvent};
use crate::webrtc::{self, MediaShared};

/// A short synthetic test pattern compiled into the binary so `--demo` needs
/// no console, no ffmpeg, and no external file.
const EMBEDDED_DEMO: &[u8] = include_bytes!("../assets/demo.h264");

/// Where the demo stream comes from.
#[derive(Clone, Debug)]
pub enum DemoSource {
    /// The built-in test pattern.
    Embedded,
    /// A user-supplied Annex-B H.264 file.
    File(PathBuf),
}

pub async fn start(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<StartResponse>, ApiError> {
    let start = state
        .reserve_session_start()
        .await
        .ok_or_else(|| ApiError::conflict("a session is already active"))?;
    let data = match state.demo.as_ref() {
        Some(DemoSource::Embedded) => std::borrow::Cow::Borrowed(EMBEDDED_DEMO),
        Some(DemoSource::File(path)) => std::borrow::Cow::Owned(
            tokio::fs::read(path)
                .await
                .with_context(|| format!("failed to read {}", path.display()))
                .map_err(|e| ApiError::internal(e.to_string()))?,
        ),
        None => return Err(ApiError::bad_request("demo mode was not enabled")),
    };
    let access_units = split_access_units(&data);
    if access_units.is_empty() {
        return Err(ApiError::bad_request("demo file has no H.264 access units"));
    }

    let peers = webrtc::create_peer(state.events.clone(), Codec::H264)
        .await
        .map_err(|e| ApiError::internal(format!("WebRTC setup failed: {e:#}")))?;
    let offer_sdp = peers.offer_sdp.clone();
    let shared = Arc::new(MediaShared::default());
    let id = Uuid::new_v4();
    let (input_tx, input_rx) = watch::channel(ControllerState::idle());
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (stop_tx, stop_rx) = oneshot::channel();

    let active = ActiveSession {
        id,
        host_id: "demo".to_string(),
        codec: Codec::H264,
        input: input_tx,
        commands: cmd_tx,
        peer: peers.peer.clone(),
        peer_connected: peers.connected.clone(),
        gathering: peers.gathering.clone(),
        stop_tx: Some(stop_tx),
        started: std::time::Instant::now(),
    };
    {
        let mut slot = state.active.lock().await;
        if slot.is_some() {
            drop(slot);
            let _ = peers.peer.close().await;
            return Err(ApiError::conflict("a session is already active"));
        }
        *slot = Some(active);
    }
    start.disarm();

    let panic_state = state.clone();
    let panic_peers = peers.clone();
    tokio::spawn(async move {
        let outcome = std::panic::AssertUnwindSafe(run(
            state,
            id,
            peers,
            shared,
            access_units,
            input_rx,
            cmd_rx,
            stop_rx,
        ))
        .catch_unwind()
        .await;
        if outcome.is_err() {
            tracing::error!("demo session task panicked");
            finish(&panic_state, id, &panic_peers, "demo session crashed", true).await;
        }
    });

    Ok(Json(StartResponse {
        session_id: id.to_string(),
        offer: RtcDescription {
            kind: "offer".to_string(),
            sdp: offer_sdp,
        },
        bitrate_kbps: 0,
        adaptive: false,
        adaptive_stepped_down: false,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn run(
    state: Arc<AppState>,
    id: Uuid,
    peers: webrtc::PeerHandles,
    shared: Arc<MediaShared>,
    access_units: Vec<Bytes>,
    _input_rx: watch::Receiver<ControllerState>,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let connected = peers.connected.clone();
    let first_idr = access_units
        .iter()
        .position(|au| scan_annex_b(au, Codec::H264).is_idr)
        .unwrap_or(0);

    // Cache SPS/PPS from the stream for IDR injection.
    let param_sets: Vec<Bytes> = collect_param_sets(&access_units);
    if !param_sets.is_empty() {
        *shared
            .param_sets
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = param_sets;
    }

    // Wait briefly for the browser to connect. Every exit from here must clear
    // the active slot, otherwise the UI keeps receiving "session already
    // active" until the server is restarted.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut stopped = false;
    while !connected.load(Ordering::Relaxed) {
        if tokio::time::Instant::now() >= deadline {
            finish(&state, id, &peers, "browser did not connect", true).await;
            return;
        }
        tokio::select! {
            _ = &mut stop_rx => {
                stopped = true;
                break;
            }
            command = cmd_rx.recv() => {
                match command {
                    Some(SessionCommand::Stop) | None => {
                        stopped = true;
                        break;
                    }
                    Some(_) => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
    }
    if stopped {
        finish(&state, id, &peers, "demo stream stopped", false).await;
        return;
    }

    let mut index = first_idr;
    loop {
        let au = access_units[index].clone();
        let scan = scan_annex_b(&au, Codec::H264);
        let mut data = au;
        if scan.is_idr && !(scan.has_sps || scan.has_pps) {
            data = shared.with_param_sets(data);
        }
        let write = peers.video.write_sample(data, Duration::from_millis(33));
        tokio::pin!(write);
        let wrote = tokio::select! {
            _ = &mut stop_rx => break,
            result = &mut write => result.is_ok(),
        };
        if !wrote {
            break;
        }
        if scan.is_idr {
            shared.first_idr_sent.store(true, Ordering::Relaxed);
        }
        tokio::select! {
            _ = &mut stop_rx => break,
            command = cmd_rx.recv() => {
                match command {
                    Some(SessionCommand::Stop) | None => break,
                    Some(_) => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(33)) => {}
        }
        index += 1;
        if index >= access_units.len() {
            index = first_idr;
        }
    }

    finish(&state, id, &peers, "demo stream stopped", false).await;
}

async fn finish(
    state: &AppState,
    id: Uuid,
    peers: &webrtc::PeerHandles,
    detail: &str,
    is_error: bool,
) {
    if is_error {
        *state.last_session_error.lock().await = Some(detail.to_string());
    }
    state.publish(ServerEvent::Quit {
        session_id: id.to_string(),
        reason: if is_error {
            QuitReason::StreamConnectionUnknown.as_str().to_string()
        } else {
            "Stopped".to_string()
        },
        reason_string: Some(detail.to_string()),
    });
    {
        // Release the slot before awaiting the peer shutdown so a stuck
        // transport can never leave the UI permanently "already active".
        let mut active = state.active.lock().await;
        if active.as_ref().map(|a| a.id) == Some(id) {
            *active = None;
        }
    }
    if tokio::time::timeout(Duration::from_secs(3), peers.peer.close())
        .await
        .is_err()
    {
        tracing::warn!("demo WebRTC peer did not close within 3 s");
    }
}

/// Split an Annex-B byte stream into access units at AUD NALs.
fn split_access_units(data: &[u8]) -> Vec<Bytes> {
    let units = annex_b_units(data);
    // `annex_b_units` also accepts a bare NAL; a demo file must contain real
    // start codes, otherwise any text/binary file would be accepted.
    if units.is_empty() || units.iter().all(|unit| unit.start == unit.header) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current = Vec::new();
    for unit in units {
        let nal_type = data.get(unit.header).copied().unwrap_or(0) & 0x1f;
        if nal_type == 9 && !current.is_empty() {
            out.push(Bytes::from(std::mem::take(&mut current)));
        }
        current.extend_from_slice(&data[unit.start..unit.end]);
    }
    if !current.is_empty() {
        out.push(Bytes::from(current));
    }
    out
}

fn collect_param_sets(units: &[Bytes]) -> Vec<Bytes> {
    for au in units {
        let mut out = Vec::new();
        for unit in annex_b_units(au) {
            let header = au.get(unit.header).copied().unwrap_or(0);
            if matches!(header & 0x1f, 7 | 8) {
                out.push(Bytes::copy_from_slice(&au[unit.start..unit.end]));
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_access_units_and_collects_parameter_sets() {
        let data = [
            0, 0, 0, 1, 0x09, 0xf0, // AUD
            0, 0, 0, 1, 0x67, 0x42, // SPS
            0, 0, 1, 0x68, 0xce, // PPS
            0, 0, 0, 1, 0x65, 0x88, // IDR
            0, 0, 0, 1, 0x09, 0xf0, // AUD (next access unit)
            0, 0, 0, 1, 0x65, 0x99, // IDR
        ];
        let units = split_access_units(&data);
        assert_eq!(units.len(), 2);
        assert_eq!(&units[0][..], &data[..23]);
        assert_eq!(&units[1][..], &data[23..]);

        let params = collect_param_sets(&units);
        assert_eq!(params.len(), 2);
        assert_eq!(&params[0][..], &[0, 0, 0, 1, 0x67, 0x42]);
        assert_eq!(&params[1][..], &[0, 0, 1, 0x68, 0xce]);

        // Files without an Annex-B start code are rejected.
        assert!(split_access_units(b"not an annex-b stream").is_empty());
    }

    #[test]
    fn embedded_demo_is_a_valid_h264_stream() {
        let units = split_access_units(EMBEDDED_DEMO);
        assert!(!units.is_empty());
        let first = scan_annex_b(&units[0], Codec::H264);
        assert!(first.has_sps && first.has_pps && first.is_idr);
        assert!(
            units
                .iter()
                .any(|unit| scan_annex_b(unit, Codec::H264).is_idr)
        );
    }
}
