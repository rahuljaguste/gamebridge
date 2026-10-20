// SPDX-License-Identifier: AGPL-3.0-only
//! JSON API.

use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use chiaki_core::discovery;
use chiaki_core::regist::{self, RegistInfo};
use chiaki_core::session::{ConnectInfo, Session, SessionOptions};
use chiaki_core::target::{Codec, Fps, Resolution, VideoProfile};
use rtc::peer_connection::configuration::RTCOfferOptions;
use rtc::peer_connection::sdp::RTCSessionDescription;

use crate::adaptive::{self, AdaptiveContext};
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

impl From<discovery::DiscoveryHost> for DiscoveryView {
    fn from(host: discovery::DiscoveryHost) -> Self {
        let target = target_string(host.target()).to_string();
        Self {
            host_addr: host.host_addr,
            host_id: host.host_id,
            host_name: host.host_name,
            state: host
                .state
                .map(|state| state.as_str().to_string())
                .unwrap_or_default(),
            system_version: host.system_version,
            target,
            running_app_name: host.running_app_name,
            running_app_titleid: host.running_app_titleid,
        }
    }
}

pub async fn discover(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<Vec<DiscoveryView>>, ApiError> {
    let hosts = discovery::discover(Duration::from_secs(2))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(hosts.into_iter().map(DiscoveryView::from).collect()))
}

#[derive(Debug, Deserialize)]
pub struct DetectRequest {
    pub host: String,
}

/// Unicast SRC lookup used by the registration form. The console's discovery
/// response carries `system-version` and the discovery protocol version, which
/// is enough to pick the correct registration target.
pub async fn detect(
    State(_state): State<Arc<AppState>>,
    Json(request): Json<DetectRequest>,
) -> Result<Json<DiscoveryView>, ApiError> {
    let host = request.host.trim();
    if host.is_empty() {
        return Err(ApiError::bad_request("host is required"));
    }
    let ip = tokio::net::lookup_host((host, 0))
        .await
        .map_err(|_| ApiError::bad_request("could not resolve host"))?
        .find(|addr| addr.is_ipv4())
        .map(|addr| addr.ip())
        .ok_or_else(|| ApiError::bad_request("no IPv4 address for host"))?;
    let found = discovery::discover_host(ip, Duration::from_millis(1200))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("no console responded to discovery"))?;
    Ok(Json(found.into()))
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
    /// "h264" (default), "h265" or "h265-hdr".
    #[serde(default)]
    pub codec: Option<String>,
    /// Step the *next* session's starting bitrate down after a lossy session.
    #[serde(default = "default_true")]
    pub adaptive_bitrate: bool,
    #[serde(default)]
    pub enable_dualsense: bool,
}

fn default_true() -> bool {
    true
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
    /// Bitrate the console is being asked to encode at.
    pub bitrate_kbps: u32,
    /// Whether adaptive bitrate is enabled for this session.
    pub adaptive: bool,
    /// True when a previous lossy session lowered this start below the
    /// configured base bitrate.
    pub adaptive_stepped_down: bool,
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

    // The PS4 launch spec and ctrl request have no HEVC fields. Only PS5
    // sessions can negotiate it.
    if video.codec != Codec::H264 && !target.is_ps5() {
        return Err(ApiError::bad_request("H.265 requires a PS5 console"));
    }

    // The launch spec fixes the encoder bitrate for the whole session. Adapt
    // the next session's starting point from the previous session's loss.
    let base_bitrate = video.bitrate_kbps;
    let mut adaptive_ctx = None;
    let mut adaptive_stepped_down = false;
    if request.adaptive_bitrate {
        let (adaptive_state, stepped) =
            adaptive::resolve(&state.config, &request.host_id, base_bitrate);
        video.bitrate_kbps = adaptive_state.current_kbps;
        adaptive_stepped_down = stepped;
        adaptive_ctx = Some(AdaptiveContext {
            host_id: request.host_id.clone(),
            state: adaptive_state,
        });
    }

    let mut info = ConnectInfo::new(target, host.host.clone(), regist_key, morning, video);
    info.enable_dualsense = request.enable_dualsense;
    // The peer was built for one video codec. If the console rejects HEVC,
    // fail with a clear error instead of silently sending H.264 on an H.265
    // track.
    info.allow_codec_fallback = video.codec == Codec::H264;

    // Build the peer before opening a console session. If any WebRTC setup
    // step fails, no console session exists yet, so there is nothing to leak
    // or to stop gracefully.
    let peers = webrtc::create_peer(state.events.clone(), video.codec)
        .await
        .map_err(|e| ApiError::internal(format!("WebRTC setup failed: {e:#}")))?;
    let offer_sdp = peers.offer_sdp.clone();
    let shared = Arc::new(MediaShared::default());
    let id = Uuid::new_v4();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

    let mut session = match Session::start(info, SessionOptions::default()) {
        Ok(session) => session,
        Err(e) => {
            let _ = peers.peer.close().await;
            return Err(ApiError::internal(e.to_string()));
        }
    };
    let events_rx = session.take_events();
    let video_rx = session.take_video();
    let audio_rx = session.take_audio();

    let active = ActiveSession {
        id,
        host_id: request.host_id.clone(),
        codec: video.codec,
        input: session.input.clone(),
        commands: session.commands.clone(),
        peer: peers.peer.clone(),
        peer_connected: peers.connected.clone(),
        gathering: peers.gathering.clone(),
        stop_tx: Some(stop_tx),
        started: Instant::now(),
    };
    {
        let mut slot = state.active.lock().await;
        if slot.is_some() {
            drop(slot);
            let _ = peers.peer.close().await;
            // A losing start race must release any session the supervisor
            // opened, rather than dropping it and aborting the cleanup path.
            session.stop().await;
            return Err(ApiError::conflict("a session is already active"));
        }
        *slot = Some(active);
    }
    start.disarm();

    let manager_state = state.clone();
    let panic_state = state.clone();
    let session_id = id.to_string();
    let manager_session_id = session_id.clone();
    let bridge_cancel = CancellationToken::new();
    let panic_cancel = bridge_cancel.clone();
    let panic_peer = peers.peer.clone();
    let manager_peers = peers;
    tokio::spawn(async move {
        let result = AssertUnwindSafe(manager::run(
            manager_state,
            id,
            session,
            events_rx,
            video_rx,
            audio_rx,
            manager_peers,
            shared,
            bridge_cancel,
            frame_duration,
            adaptive_ctx,
            stop_rx,
        ))
        .catch_unwind()
        .await;
        if result.is_err() {
            tracing::error!("session manager panicked; cleaning up bridge tasks and peer");
            // The manager's own cleanup path did not run. Cancel its bridge
            // tasks (including the 1 Hz stats reporter) and close the peer so
            // no task or UDP socket is left behind.
            panic_cancel.cancel();
            panic_state.publish(crate::state::ServerEvent::Quit {
                session_id: manager_session_id.clone(),
                reason: "Unknown Error in Stream Connection".to_string(),
                reason_string: Some("internal session error".to_string()),
            });
            *panic_state.last_session_error.lock().await =
                Some("internal session error".to_string());
            {
                let mut active = panic_state.active.lock().await;
                if active.as_ref().map(|a| a.id) == Some(id) {
                    *active = None;
                }
            }
            let _ = timeout(Duration::from_secs(5), panic_peer.close()).await;
        }
    });

    Ok(Json(StartResponse {
        session_id,
        offer: RtcDescription {
            kind: "offer".to_string(),
            sdp: offer_sdp,
        },
        bitrate_kbps: video.bitrate_kbps,
        adaptive: request.adaptive_bitrate,
        adaptive_stepped_down,
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
pub struct PsnPublicLookupRequest {
    pub online_id: String,
}

pub async fn psn_public_lookup(
    Json(request): Json<PsnPublicLookupRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account_id = psn::public_lookup(&request.online_id)
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(serde_json::json!({ "account_id": account_id })))
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
    let (peer, codec) = {
        let active = state.active.lock().await;
        match active.as_ref() {
            Some(session) if session.id.to_string() == request.session_id => {
                (session.peer.clone(), session.codec)
            }
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
    // The peer advertises exactly one video codec. An answer without it means
    // the browser cannot decode this stream; fail before the console session
    // runs with a track that will never display.
    if !sdp_accepts_codec(&request.sdp, codec) {
        let name = if codec.is_h265() { "H.265" } else { "H.264" };
        return Err(ApiError::bad_request(format!(
            "this browser did not accept {name}; choose H.264 in Settings → Stream"
        )));
    }
    let answer = RTCSessionDescription::answer(request.sdp)
        .map_err(|e| ApiError::bad_request(format!("invalid answer SDP: {e}")))?;
    peer.set_remote_description(answer)
        .await
        .map_err(|e| ApiError::internal(format!("set remote description failed: {e}")))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct RestartRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct RestartResponse {
    pub offer: RtcDescription,
}

/// Re-offer the existing WebRTC peer with fresh ICE credentials. The browser
/// answers through `/api/session/answer`; the console session is untouched.
pub async fn restart_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RestartRequest>,
) -> Result<Json<RestartResponse>, ApiError> {
    let (peer, gathering) = {
        let active = state.active.lock().await;
        match active.as_ref() {
            Some(session) if session.id.to_string() == request.session_id => {
                (session.peer.clone(), session.gathering.clone())
            }
            Some(_) => {
                return Err(ApiError::conflict(
                    "restart does not match the active session",
                ));
            }
            None => return Err(ApiError::not_found("no active session")),
        }
    };
    let offer = peer
        .create_offer(Some(RTCOfferOptions { ice_restart: true }))
        .await
        .map_err(|e| ApiError::internal(format!("ICE restart offer failed: {e}")))?;
    peer.set_local_description(offer)
        .await
        .map_err(|e| ApiError::internal(format!("ICE restart description failed: {e}")))?;
    let _ = tokio::time::timeout(Duration::from_secs(3), gathering.notified()).await;
    let local = peer
        .local_description()
        .await
        .ok_or_else(|| ApiError::internal("missing ICE restart description"))?;
    Ok(Json(RestartResponse {
        offer: RtcDescription {
            kind: "offer".to_string(),
            sdp: local.sdp,
        },
    }))
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

/// Check that the answer accepted the offered codec on a live video m-line.
/// A rejected m-line (port 0) can still carry rtpmap attributes, so scanning
/// the whole SDP for the codec name is not sufficient.
fn sdp_accepts_codec(sdp: &str, codec: Codec) -> bool {
    let encoding = if codec.is_h265() {
        "H265/90000"
    } else {
        "H264/90000"
    };
    let mut video_active = false;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("m=") {
            video_active = rest.starts_with("video ")
                && rest
                    .split_whitespace()
                    .nth(1)
                    .and_then(|port| port.parse::<u16>().ok())
                    .is_some_and(|port| port != 0);
            continue;
        }
        if !video_active {
            continue;
        }
        let Some(rest) = line.strip_prefix("a=rtpmap:") else {
            continue;
        };
        if rest
            .split_whitespace()
            .nth(1)
            .is_some_and(|name| name.eq_ignore_ascii_case(encoding))
        {
            return true;
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use super::sdp_accepts_codec;
    use chiaki_core::target::Codec;

    const H264_ANSWER: &str =
        "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\n";
    const H265_ANSWER: &str =
        "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 103\r\na=rtpmap:103 H265/90000\r\n";
    const AUDIO_ONLY: &str = "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\n";
    const REJECTED_VIDEO_WITH_RTPMAP: &str =
        "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 103\r\na=rtpmap:103 H265/90000\r\n";
    const AUDIO_FIRST: &str = "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\n";

    #[test]
    fn answer_must_accept_the_offered_codec() {
        assert!(sdp_accepts_codec(H264_ANSWER, Codec::H264));
        assert!(!sdp_accepts_codec(H264_ANSWER, Codec::H265));
        assert!(!sdp_accepts_codec(H264_ANSWER, Codec::H265Hdr));
        assert!(sdp_accepts_codec(H265_ANSWER, Codec::H265));
        assert!(sdp_accepts_codec(H265_ANSWER, Codec::H265Hdr));
        assert!(!sdp_accepts_codec(H265_ANSWER, Codec::H264));
        assert!(!sdp_accepts_codec(AUDIO_ONLY, Codec::H264));
        assert!(!sdp_accepts_codec(AUDIO_ONLY, Codec::H265));
        // Port 0 means the browser rejected the video m-line even if its
        // rtpmap attributes are still present.
        assert!(!sdp_accepts_codec(REJECTED_VIDEO_WITH_RTPMAP, Codec::H265));
        assert!(sdp_accepts_codec(AUDIO_FIRST, Codec::H264));
    }
}
