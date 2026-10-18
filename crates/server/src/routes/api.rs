// SPDX-License-Identifier: AGPL-3.0-only
//! JSON API.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use chiaki_core::discovery;
use chiaki_core::regist::{self, RegistInfo};
use chiaki_core::session::{ConnectInfo, Session, SessionOptions};
use chiaki_core::target::{Fps, Resolution, VideoProfile};
use rtc::peer_connection::sdp::RTCSessionDescription;

use crate::config::{StoredHost, parse_target, target_string};
use crate::manager;
use crate::psn;
use crate::state::{ActiveSession, AppState};
use crate::webrtc::{self, MediaShared};

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        ApiError::internal(value.to_string())
    }
}

#[derive(Debug, Serialize)]
pub struct HostView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub target: String,
    pub nickname: String,
}

impl From<StoredHost> for HostView {
    fn from(h: StoredHost) -> Self {
        Self {
            id: h.id,
            name: h.name,
            host: h.host,
            target: h.target,
            nickname: h.nickname,
        }
    }
}

pub async fn list_hosts(State(state): State<Arc<AppState>>) -> Json<Vec<HostView>> {
    Json(
        state
            .config
            .hosts()
            .into_iter()
            .map(HostView::from)
            .collect(),
    )
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub host: String,
    pub target: String,
    pub pin: String,
    #[serde(default)]
    pub psn_account_id: Option<String>,
    #[serde(default)]
    pub psn_online_id: Option<String>,
    #[serde(default)]
    pub broadcast: bool,
    #[serde(default)]
    pub name: Option<String>,
}

pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<HostView>, ApiError> {
    let target = parse_target(&request.target).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let pin: u32 = request
        .pin
        .trim()
        .parse()
        .map_err(|_| ApiError::bad_request("PIN must be numeric"))?;
    let psn_account_id = match request.psn_account_id.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            Some(regist::parse_psn_account_id(value).map_err(|_| {
                ApiError::bad_request(
                    "PSN Account ID must be the numeric account ID (not the PSN username), a \
                     hex value, or base64 of the 8-byte ID",
                )
            })?)
        }
        _ => None,
    };
    let psn_online_id = request
        .psn_online_id
        .clone()
        .filter(|s| !s.trim().is_empty());
    if psn_account_id.is_none() && psn_online_id.is_none() {
        return Err(ApiError::bad_request(
            "provide either a PSN Account ID or an online ID",
        ));
    }

    let info = RegistInfo {
        target,
        host: request.host.trim().to_string(),
        broadcast: request.broadcast,
        psn_online_id,
        psn_account_id,
        pin,
    };
    let registered = regist::register(&info).await.map_err(|e| {
        tracing::warn!("registration failed: {e}");
        ApiError::bad_request(format!("registration failed: {e}"))
    })?;

    let id = Uuid::new_v4().to_string();
    let nickname = registered.server_nickname.clone();
    let host = StoredHost {
        id: id.clone(),
        name: request
            .name
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some(nickname.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| request.host.clone()),
        host: request.host.trim().to_string(),
        target: target_string(registered.target).to_string(),
        regist_key: hex::encode(registered.rp_regist_key),
        morning: hex::encode(registered.rp_key),
        rp_key_type: registered.rp_key_type,
        server_mac: hex::encode(registered.server_mac),
        nickname,
    };
    state.config.add(host.clone())?;
    Ok(Json(HostView::from(host)))
}

pub async fn delete_host(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let removed = state.config.remove(&id)?;
    if !removed {
        return Err(ApiError::not_found("host not found"));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Serialize)]
pub struct DiscoveryView {
    pub host_addr: String,
    pub host_id: String,
    pub host_name: String,
    pub state: String,
    pub system_version: String,
    pub target: String,
    pub running_app_name: String,
    pub running_app_titleid: String,
}

pub async fn discover(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<Vec<DiscoveryView>>, ApiError> {
    let hosts = discovery::discover(Duration::from_secs(2))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(
        hosts
            .into_iter()
            .map(|h| {
                let target = target_string(h.target()).to_string();
                DiscoveryView {
                    host_addr: h.host_addr,
                    host_id: h.host_id,
                    host_name: h.host_name,
                    state: h.state.map(|s| s.as_str().to_string()).unwrap_or_default(),
                    system_version: h.system_version,
                    target,
                    running_app_name: h.running_app_name,
                    running_app_titleid: h.running_app_titleid,
                }
            })
            .collect(),
    ))
}

pub async fn wakeup(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let host = state
        .config
        .get(&id)
        .ok_or_else(|| ApiError::not_found("host not found"))?;
    let target = host
        .target_enum()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let key = host
        .regist_key_bytes()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let ip: IpAddr = tokio::net::lookup_host((host.host.as_str(), 0))
        .await
        .map_err(|_| ApiError::bad_request("could not resolve host"))?
        .next()
        .map(|a| a.ip())
        .ok_or_else(|| ApiError::bad_request("could not resolve host"))?;
    discovery::wakeup(ip, &key, target.is_ps5())
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    pub host_id: String,
    #[serde(default = "default_resolution")]
    pub resolution: String,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
    /// "h264" (default), "h265" or "h265-hdr". H.265 is a diagnostic for
    /// PS5 consoles whose firmware rejects AVC launch specs.
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub enable_dualsense: bool,
}

fn default_resolution() -> String {
    "1080p".to_string()
}
fn default_fps() -> u32 {
    60
}

#[derive(Debug, Serialize)]
pub struct RtcDescription {
    #[serde(rename = "type")]
    pub kind: String,
    pub sdp: String,
}

#[derive(Debug, Serialize)]
pub struct StartResponse {
    pub session_id: String,
    pub offer: RtcDescription,
}

pub async fn start_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartRequest>,
) -> Result<Json<StartResponse>, ApiError> {
    // Hold the reservation for the whole setup so a double-click cannot start
    // two console sessions before either reaches the `active` check.
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
    let host = state
        .config
        .get(&request.host_id)
        .ok_or_else(|| ApiError::not_found("host not found"))?;
    let target = host
        .target_enum()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let regist_key = host
        .regist_key_bytes()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let morning = host
        .morning_bytes()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let resolution = Resolution::parse(&request.resolution)
        .map_err(|_| ApiError::bad_request("bad resolution"))?;
    let fps = match request.fps {
        30 => Fps::Fps30,
        60 => Fps::Fps60,
        _ => return Err(ApiError::bad_request("FPS must be 30 or 60")),
    };
    // The console encodes at this nominal rate, so RTP timestamps step by
    // exactly 1/fps regardless of network jitter.
    let frame_duration = Duration::from_secs_f64(1.0 / request.fps as f64);
    let mut video =
        VideoProfile::preset(resolution, fps).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if let Some(bitrate) = request.bitrate_kbps {
        if !(100..=100_000).contains(&bitrate) {
            return Err(ApiError::bad_request("bitrate out of range"));
        }
        video.bitrate_kbps = bitrate;
    }
    if let Some(codec) = request.codec.as_deref() {
        video.codec = match codec.to_ascii_lowercase().as_str() {
            "h264" | "avc" => chiaki_core::target::Codec::H264,
            "h265" | "hevc" => chiaki_core::target::Codec::H265,
            "h265-hdr" | "hevc-hdr" => chiaki_core::target::Codec::H265Hdr,
            _ => {
                return Err(ApiError::bad_request(
                    "codec must be h264, h265 or h265-hdr",
                ));
            }
        };
    }

    let mut info = ConnectInfo::new(target, host.host.clone(), regist_key, morning, video);
    info.enable_dualsense = request.enable_dualsense;

    let mut session = Session::start(info, SessionOptions::default())
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let events_rx = session.take_events();
    let video_rx = session.take_video();
    let audio_rx = session.take_audio();

    let peers = webrtc::create_peer(state.events.clone())
        .await
        .map_err(|e| ApiError::internal(format!("WebRTC setup failed: {e:#}")))?;
    let offer_sdp = peers.offer_sdp.clone();
    let shared = Arc::new(MediaShared::default());
    let id = Uuid::new_v4();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

    let active = ActiveSession {
        id,
        host_id: request.host_id.clone(),
        input: session.input.clone(),
        commands: session.commands.clone(),
        peer: peers.peer.clone(),
        peer_connected: peers.connected.clone(),
        stop_tx: Some(stop_tx),
        started: Instant::now(),
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

    let manager_state = state.clone();
    let manager_peers = peers;
    tokio::spawn(async move {
        manager::run(
            manager_state,
            id,
            session,
            events_rx,
            video_rx,
            audio_rx,
            manager_peers,
            shared,
            frame_duration,
            stop_rx,
        )
        .await;
    });

    Ok(Json(StartResponse {
        session_id: id.to_string(),
        offer: RtcDescription {
            kind: "offer".to_string(),
            sdp: offer_sdp,
        },
    }))
}

pub async fn psn_login() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "url": psn::login_url() }))
}

#[derive(Debug, Deserialize)]
pub struct PsnAccountIdRequest {
    pub redirect_url: String,
}

pub async fn psn_account_id(
    Json(request): Json<PsnAccountIdRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account = psn::resolve_account_id(&request.redirect_url)
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(serde_json::json!({
        "user_id": account.user_id,
        "account_id": chiaki_core::util::base64::encode(&account.account_id),
    })))
}

#[derive(Debug, Deserialize)]
pub struct AnswerRequest {
    pub session_id: String,
    pub sdp: String,
}

pub async fn answer(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AnswerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let peer = {
        let active = state.active.lock().await;
        match active.as_ref() {
            Some(session) if session.id.to_string() == request.session_id => session.peer.clone(),
            Some(_) => {
                return Err(ApiError::conflict(
                    "answer does not match the active session",
                ));
            }
            None => {
                let last = state.last_session_error.lock().await.clone();
                return Err(match last {
                    Some(error) => ApiError::bad_request(format!("session failed: {error}")),
                    None => ApiError::not_found("no active session"),
                });
            }
        }
    };
    let answer = RTCSessionDescription::answer(request.sdp)
        .map_err(|e| ApiError::bad_request(format!("invalid answer SDP: {e}")))?;
    peer.set_remote_description(answer)
        .await
        .map_err(|e| ApiError::internal(format!("set remote description failed: {e}")))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn stop_session(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut active = state.active.lock().await;
    if let Some(session) = active.as_mut()
        && let Some(tx) = session.stop_tx.take()
    {
        let _ = tx.send(());
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub active: bool,
    pub session_id: Option<String>,
    pub host_id: Option<String>,
    pub uptime_seconds: u64,
    pub peer_connected: Option<bool>,
    pub demo: bool,
    pub last_error: Option<String>,
}

pub async fn status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let (active, session_id, host_id, uptime_seconds, peer_connected) = {
        let slot = state.active.lock().await;
        match slot.as_ref() {
            Some(session) => (
                true,
                Some(session.id.to_string()),
                Some(session.host_id.clone()),
                session.started.elapsed().as_secs(),
                Some(
                    session
                        .peer_connected
                        .load(std::sync::atomic::Ordering::Relaxed),
                ),
            ),
            None => (false, None, None, 0, None),
        }
    };
    // Drop the session lock before taking the error lock. Other sites use the
    // same order, but scoping makes the invariant explicit and independent.
    let last_error = state.last_session_error.lock().await.clone();
    Json(StatusResponse {
        active,
        session_id,
        host_id,
        uptime_seconds,
        peer_connected,
        demo: state.demo_file.is_some(),
        last_error,
    })
}
