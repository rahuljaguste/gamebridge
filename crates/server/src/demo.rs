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
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use chiaki_core::controller::ControllerState;
use chiaki_core::media::video::scan_annex_b;
use chiaki_core::session::SessionCommand;
use chiaki_core::target::Codec;

use crate::routes::api::{ApiError, RtcDescription, StartResponse};
use crate::state::{ActiveSession, AppState, ServerEvent};
use crate::webrtc::{self, MediaShared};

pub async fn start(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<Json<StartResponse>, ApiError> {
    let start = state
        .begin_start()
        .ok_or_else(|| ApiError::conflict("a session is already active"))?;
    {
        let active = state.active.lock().await;
        if active.is_some() {
            return Err(ApiError::conflict("a session is already active"));
        }
    }
    state.clear_replay();
    *state.last_session_error.lock().await = None;
    let path: PathBuf = state
        .demo_file
        .clone()
        .ok_or_else(|| ApiError::bad_request("demo mode was not enabled"))?;
    let data = tokio::fs::read(&path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let access_units = split_access_units(&data);
    if access_units.is_empty() {
        return Err(ApiError::bad_request("demo file has no H.264 access units"));
    }

    let peers = webrtc::create_peer(state.events.clone())
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

    tokio::spawn(run(
        state,
        id,
        peers,
        shared,
        access_units,
        input_rx,
        cmd_rx,
        stop_rx,
    ));

    Ok(Json(StartResponse {
        session_id: id.to_string(),
        offer: RtcDescription {
            kind: "offer".to_string(),
            sdp: offer_sdp,
        },
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
        *shared.param_sets.lock().unwrap() = param_sets;
    }

    // Wait briefly for the browser to connect. Every exit from here must clear
    // the active slot, otherwise the UI keeps receiving "session already
    // active" until the server is restarted.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut stopped = false;
    while !connected.load(Ordering::Relaxed) {
        if tokio::time::Instant::now() >= deadline {
            finish(&state, id, &peers, "browser did not connect").await;
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
        finish(&state, id, &peers, "demo stream stopped").await;
        return;
    }

    let mut index = first_idr;
    loop {
        let au = access_units[index].clone();
        let scan = scan_annex_b(&au, Codec::H264);
        let mut data = au;
        if scan.is_idr && !(scan.has_sps || scan.has_pps) {
            let params = shared.param_sets.lock().unwrap().clone();
            if !params.is_empty() {
                let mut combined =
                    Vec::with_capacity(data.len() + params.iter().map(|p| p.len()).sum::<usize>());
                for p in &params {
                    combined.extend_from_slice(p);
                }
                combined.extend_from_slice(&data);
                data = Bytes::from(combined);
            }
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

    finish(&state, id, &peers, "demo stream stopped").await;
}

async fn finish(state: &AppState, id: Uuid, peers: &webrtc::PeerHandles, detail: &str) {
    state.publish(ServerEvent::Quit {
        session_id: id.to_string(),
        reason: "Stopped".to_string(),
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
    let _ = peers.peer.close().await;
}

fn start_code_len(data: &[u8], i: usize) -> Option<usize> {
    if i + 3 <= data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
        Some(3)
    } else if i + 4 <= data.len()
        && data[i] == 0
        && data[i + 1] == 0
        && data[i + 2] == 0
        && data[i + 3] == 1
    {
        Some(4)
    } else {
        None
    }
}

/// Split an Annex-B byte stream into access units at AUD NALs.
fn split_access_units(data: &[u8]) -> Vec<Bytes> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if let Some(len) = start_code_len(data, i) {
            starts.push((i, len));
            i += len;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        return Vec::new();
    }
    let mut units = Vec::new();
    let mut current = Vec::new();
    for (idx, &(pos, sc_len)) in starts.iter().enumerate() {
        let end = starts
            .get(idx + 1)
            .map(|(next, _)| *next)
            .unwrap_or(data.len());
        let nal = &data[pos..end];
        let header = data.get(pos + sc_len).copied().unwrap_or(0);
        let nal_type = header & 0x1f;
        if nal_type == 9 && !current.is_empty() {
            units.push(Bytes::from(std::mem::take(&mut current)));
        }
        current.extend_from_slice(nal);
    }
    if !current.is_empty() {
        units.push(Bytes::from(current));
    }
    units
}

fn collect_param_sets(units: &[Bytes]) -> Vec<Bytes> {
    let mut out = Vec::new();
    for au in units {
        let mut i = 0;
        while i < au.len() {
            if let Some(sc_len) = start_code_len(au, i) {
                let start = i;
                let mut end = au.len();
                let mut j = i + sc_len;
                while j < au.len() {
                    if start_code_len(au, j).is_some() {
                        end = j;
                        break;
                    }
                    j += 1;
                }
                if let Some(&header) = au.get(i + sc_len) {
                    let ty = header & 0x1f;
                    if ty == 7 || ty == 8 {
                        out.push(Bytes::copy_from_slice(&au[start..end]));
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}
