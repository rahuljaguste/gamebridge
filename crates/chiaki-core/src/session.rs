// SPDX-License-Identifier: AGPL-3.0-only
//! Session request and top-level session lifecycle.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, lookup_host};
use tokio::time::timeout;

use crate::crypto::rpcrypt::RPCrypt;
use crate::error::{Error, QuitReason, Result, rp_application_reason_string};
use crate::http;
use crate::target::{Target, VideoProfile};
use crate::util::{base64, rng};

pub const SESSION_PORT: u16 = 9295;
pub const STREAM_PORT: u16 = 9296;
pub const SENKUSHA_PORT: u16 = 9297;
const SESSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct ConnectInfo {
    pub target: Target,
    pub host: String,
    pub regist_key: [u8; 16],
    /// The RP-Key ("morning") returned by registration.
    pub morning: [u8; 16],
    pub video: VideoProfile,
    pub auto_downgrade: bool,
    pub enable_dualsense: bool,
    /// Device id sent to the console.
    pub did: [u8; 32],
}

impl ConnectInfo {
    pub fn new(
        target: Target,
        host: String,
        regist_key: [u8; 16],
        morning: [u8; 16],
        video: VideoProfile,
    ) -> Self {
        let mut did = [0u8; 32];
        const PREFIX: [u8; 10] = [0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80];
        did[..10].copy_from_slice(&PREFIX);
        rng::fill(&mut did[10..26]);
        Self {
            target,
            host,
            regist_key,
            morning,
            video,
            auto_downgrade: true,
            enable_dualsense: false,
            did,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SessionRequestResult {
    pub nonce: [u8; 16],
}

#[derive(Debug)]
pub enum SessionRequestError {
    VersionMismatch(Target),
    Quit(QuitReason),
    Error(Error),
}

impl From<Error> for SessionRequestError {
    fn from(e: Error) -> Self {
        SessionRequestError::Error(e)
    }
}

pub fn session_path(target: Target) -> &'static str {
    match target {
        Target::Ps4_8 | Target::Ps4_9 => "/sce/rp/session",
        Target::Ps5Unknown | Target::Ps5_1 => "/sie/ps5/rp/sess/init",
        _ => "/sie/ps4/rp/sess/init",
    }
}

pub fn ctrl_path(target: Target) -> &'static str {
    match target {
        Target::Ps4_8 | Target::Ps4_9 => "/sce/rp/session/ctrl",
        Target::Ps5Unknown | Target::Ps5_1 => "/sie/ps5/rp/sess/ctrl",
        _ => "/sie/ps4/rp/sess/ctrl",
    }
}

pub async fn resolve_host(host: &str) -> Result<SocketAddr> {
    let mut addrs = lookup_host((host, SESSION_PORT))
        .await
        .map_err(|_| Error::ParseAddr)?;
    addrs.next().ok_or(Error::ParseAddr)
}

fn regist_key_hex(key: &[u8; 16]) -> String {
    let end = key.iter().position(|&b| b == 0).unwrap_or(16);
    hex::encode(&key[..end])
}

fn parse_application_reason(value: &str) -> u32 {
    let v = value.trim();
    let v = v
        .strip_prefix("0x")
        .or_else(|| v.strip_prefix("0X"))
        .unwrap_or(v);
    u32::from_str_radix(v, 16).unwrap_or(0)
}

/// Request a session from the console. This mirrors the three-attempt
/// RP-Version negotiation in `session.c`; the caller retries on
/// [`SessionRequestError::VersionMismatch`].
pub async fn request_session(
    info: &ConnectInfo,
) -> std::result::Result<SessionRequestResult, SessionRequestError> {
    let addr = resolve_host(&info.host)
        .await
        .map_err(SessionRequestError::Error)?;
    let mut tcp = timeout(SESSION_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| SessionRequestError::Quit(QuitReason::SessionRequestUnknown))?
        .map_err(|_| SessionRequestError::Quit(QuitReason::SessionRequestConnectionRefused))?;
    tcp.set_nodelay(true).ok();

    let version = info
        .target
        .rp_version()
        .ok_or(SessionRequestError::Error(Error::InvalidData))?;
    let hostname = addr.ip().to_string();
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: remoteplay Windows\r\nConnection: close\r\nContent-Length: 0\r\nRP-Registkey: {}\r\nRp-Version: {}\r\n\r\n",
        session_path(info.target),
        hostname,
        SESSION_PORT,
        regist_key_hex(&info.regist_key),
        version
    );
    tcp.write_all(request.as_bytes())
        .await
        .map_err(|_| SessionRequestError::Error(Error::Network))?;

    let mut buf = BytesMut::with_capacity(4096);
    let header_size = timeout(
        SESSION_TIMEOUT,
        http::read_http_header(&mut tcp, &mut buf, 4096),
    )
    .await
    .map_err(|_| SessionRequestError::Quit(QuitReason::SessionRequestUnknown))?
    .map_err(|_| SessionRequestError::Quit(QuitReason::SessionRequestUnknown))?;

    let response = http::parse_response(&buf[..header_size])
        .map_err(|_| SessionRequestError::Quit(QuitReason::SessionRequestUnknown))?;

    let nonce_b64 = response.get("RP-Nonce");
    let rp_version = response.get_ci("RP-Version");
    let error_code = response
        .get("RP-Application-Reason")
        .map(parse_application_reason)
        .unwrap_or(0);

    if response.code == 200 {
        let nonce_b64 =
            nonce_b64.ok_or(SessionRequestError::Quit(QuitReason::SessionRequestUnknown))?;
        let decoded = base64::decode(nonce_b64)
            .map_err(|_| SessionRequestError::Quit(QuitReason::SessionRequestUnknown))?;
        if decoded.len() != 16 {
            return Err(SessionRequestError::Quit(QuitReason::SessionRequestUnknown));
        }
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&decoded);
        return Ok(SessionRequestResult { nonce });
    }

    let is_version_reason = matches!(
        error_code,
        crate::error::RP_APPLICATION_REASON_RP_VERSION
            | crate::error::RP_APPLICATION_REASON_UNKNOWN
    );
    if is_version_reason
        && let Some(server_version) = rp_version
        && server_version != version
    {
        let mut target = Target::parse_rp_version(server_version, info.target.is_ps5());
        if target.is_unknown() && server_version == "5.0" {
            target = Target::Ps4_9;
        }
        if !target.is_unknown() {
            return Err(SessionRequestError::VersionMismatch(target));
        }
        return Err(SessionRequestError::Quit(
            QuitReason::SessionRequestRpVersionMismatch,
        ));
    }

    tracing::warn!(
        "session request rejected: HTTP {} application reason {error_code:#x} ({})",
        response.code,
        rp_application_reason_string(error_code)
    );
    let reason = match error_code {
        crate::error::RP_APPLICATION_REASON_IN_USE => QuitReason::SessionRequestRpInUse,
        crate::error::RP_APPLICATION_REASON_CRASH => QuitReason::SessionRequestRpCrash,
        crate::error::RP_APPLICATION_REASON_RP_VERSION => {
            QuitReason::SessionRequestRpVersionMismatch
        }
        _ => QuitReason::SessionRequestUnknown,
    };
    Err(SessionRequestError::Quit(reason))
}

/// Build the RPCrypt used by a session after the nonce is known.
pub fn auth_crypt(info: &ConnectInfo, nonce: &[u8; 16]) -> Result<RPCrypt> {
    RPCrypt::init_auth(info.target, nonce, &info.morning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(session_path(Target::Ps4_9), "/sce/rp/session");
        assert_eq!(session_path(Target::Ps4_10), "/sie/ps4/rp/sess/init");
        assert_eq!(session_path(Target::Ps5_1), "/sie/ps5/rp/sess/init");
        assert_eq!(ctrl_path(Target::Ps4_8), "/sce/rp/session/ctrl");
        assert_eq!(ctrl_path(Target::Ps5_1), "/sie/ps5/rp/sess/ctrl");
    }

    #[test]
    fn application_reason_hex() {
        assert_eq!(
            parse_application_reason("0x80108b10"),
            crate::error::RP_APPLICATION_REASON_IN_USE
        );
        assert_eq!(
            parse_application_reason("80108b15"),
            crate::error::RP_APPLICATION_REASON_CRASH
        );
    }

    #[test]
    fn did_layout() {
        let info = ConnectInfo::new(
            Target::Ps5_1,
            "127.0.0.1".into(),
            [0; 16],
            [0; 16],
            VideoProfile::preset(crate::target::Resolution::P1080, crate::target::Fps::Fps60)
                .unwrap(),
        );
        assert_eq!(
            &info.did[..10],
            &[0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]
        );
        assert_eq!(&info.did[26..], &[0; 6]);
    }
}

// ── Session supervisor ──────────────────────────────────────────────────────

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::controller::ControllerState;
use crate::crypto::ecdh::Ecdh;
use crate::ctrl::{self, CtrlCommand, CtrlEvent, CtrlHandle};
use crate::media::VideoProfileInfo;
use crate::media::audio::{AudioFormat, AudioFrame};
use crate::media::ingest::VideoFrame;
use crate::packet_stats::PacketStats;
use crate::stream::connection::{self, StreamOutcome};
use crate::takion::{self, TakionConnectInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    RequestingSession,
    StartingCtrl,
    WaitingPin,
    StartingStream,
    Streaming,
    Stopped,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Phase(SessionPhase),
    LoginPinRequest {
        incorrect: bool,
    },
    Connected,
    /// Round-trip time to the console, normalized to milliseconds. Sent once
    /// after Senkusha and again whenever the console reports connection
    /// quality during the stream.
    ConnectionQuality {
        rtt_ms: f64,
    },
    StreamInfo {
        profiles: Vec<VideoProfileInfo>,
        audio: AudioFormat,
    },
    VideoFormat {
        profile_index: u8,
        width: u32,
        height: u32,
        header: Bytes,
    },
    Rumble {
        unknown: u8,
        left: u8,
        right: u8,
    },
    TriggerEffects {
        type_left: u8,
        type_right: u8,
        left: [u8; 10],
        right: [u8; 10],
    },
    RemoteDisconnect {
        reason: String,
    },
    Error(String),
    Quit {
        reason: QuitReason,
        reason_str: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum SessionCommand {
    SubmitPin(Vec<u8>),
    RequestKeyframe,
    GotoBed,
    Stop,
}

#[derive(Debug, Clone, Copy)]
pub enum StreamCommand {
    RequestKeyframe,
    Stop,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    pub verify_ecdh: bool,
    pub senkusha: bool,
    pub video_queue: usize,
    pub audio_queue: usize,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            verify_ecdh: true,
            senkusha: true,
            video_queue: 8,
            audio_queue: 256,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionOutcome {
    pub quit_reason: Option<QuitReason>,
    pub reason_str: Option<String>,
    pub error: Option<String>,
}

impl SessionOutcome {
    pub fn reason(&self) -> QuitReason {
        self.quit_reason.unwrap_or(QuitReason::None)
    }
}

pub struct Session {
    pub events: mpsc::UnboundedReceiver<SessionEvent>,
    pub video: mpsc::Receiver<VideoFrame>,
    pub audio: mpsc::Receiver<AudioFrame>,
    pub input: watch::Sender<ControllerState>,
    pub stats: Arc<PacketStats>,
    pub commands: mpsc::Sender<SessionCommand>,
    outcome: watch::Receiver<Option<SessionOutcome>>,
    cancel: CancellationToken,
    join: Option<JoinHandle<()>>,
}

impl Session {
    pub fn start(info: ConnectInfo, options: SessionOptions) -> Result<Self> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (video_tx, video_rx) = mpsc::channel(options.video_queue.max(1));
        let (audio_tx, audio_rx) = mpsc::channel(options.audio_queue.max(1));
        let (input_tx, input_rx) = watch::channel(ControllerState::idle());
        let (commands_tx, commands_rx) = mpsc::channel(32);
        let (outcome_tx, outcome_rx) = watch::channel(None);
        let stats = Arc::new(PacketStats::new());
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_stats = stats.clone();
        let join = tokio::spawn(async move {
            run_supervisor(
                info,
                options,
                events_tx,
                video_tx,
                audio_tx,
                input_rx,
                task_stats,
                commands_rx,
                outcome_tx,
                task_cancel,
            )
            .await;
        });
        Ok(Self {
            events: events_rx,
            video: video_rx,
            audio: audio_rx,
            input: input_tx,
            stats,
            commands: commands_tx,
            outcome: outcome_rx,
            cancel,
            join: Some(join),
        })
    }

    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<SessionEvent> {
        std::mem::replace(&mut self.events, mpsc::unbounded_channel().1)
    }

    pub fn take_video(&mut self) -> mpsc::Receiver<VideoFrame> {
        std::mem::replace(&mut self.video, mpsc::channel(1).1)
    }

    pub fn take_audio(&mut self) -> mpsc::Receiver<AudioFrame> {
        std::mem::replace(&mut self.audio, mpsc::channel(1).1)
    }

    pub fn set_input(&self, state: ControllerState) {
        let _ = self.input.send(state);
    }

    pub fn send_command(&self, command: SessionCommand) -> bool {
        self.commands.try_send(command).is_ok()
    }

    pub async fn wait(&mut self) -> SessionOutcome {
        loop {
            if let Some(outcome) = self.outcome.borrow().clone() {
                return outcome;
            }
            if self.outcome.changed().await.is_err() {
                return self.outcome.borrow().clone().unwrap_or_default();
            }
        }
    }

    pub async fn stop(mut self) {
        let _ = self.commands.send(SessionCommand::Stop).await;
        self.cancel.cancel();
        if let Some(mut join) = self.join.take() {
            let _ = timeout(Duration::from_secs(3), &mut join).await;
            join.abort();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A dropped session must never leave its supervisor (and the console
        // ctrl/Takion connections it owns) running. Cancellation lets the
        // supervisor run its cleanup path (`ctrl.stop`, Takion close) instead
        // of aborting it outright. Keep a short reaper so a supervisor stuck
        // in a non-cancellable await cannot leak forever.
        self.cancel.cancel();
        let Some(mut join) = self.join.take() else {
            return;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if timeout(Duration::from_secs(5), &mut join).await.is_err() {
                    join.abort();
                }
            });
        } else {
            join.abort();
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_supervisor(
    mut info: ConnectInfo,
    options: SessionOptions,
    events: mpsc::UnboundedSender<SessionEvent>,
    video_tx: mpsc::Sender<VideoFrame>,
    audio_tx: mpsc::Sender<AudioFrame>,
    input_rx: watch::Receiver<ControllerState>,
    stats: Arc<PacketStats>,
    mut commands: mpsc::Receiver<SessionCommand>,
    outcome_tx: watch::Sender<Option<SessionOutcome>>,
    cancel: CancellationToken,
) {
    macro_rules! finish {
        ($reason:expr, $reason_str:expr, $error:expr) => {{
            let outcome = SessionOutcome {
                quit_reason: Some($reason),
                reason_str: $reason_str,
                error: $error,
            };
            if $reason.is_error() {
                tracing::warn!(
                    "session ended: {} (reason: {:?}, error: {:?})",
                    $reason,
                    outcome.reason_str,
                    outcome.error
                );
            } else {
                tracing::info!("session ended: {}", $reason);
            }
            let _ = events.send(SessionEvent::Quit {
                reason: $reason,
                reason_str: outcome.reason_str.clone(),
            });
            let _ = outcome_tx.send(Some(outcome));
            return;
        }};
    }

    let _ = events.send(SessionEvent::Phase(SessionPhase::RequestingSession));
    tracing::info!(
        "session: requesting a session from {} (target {:?}, {}x{}@{} {}kbps codec {:?}, dualsense={})",
        info.host,
        info.target,
        info.video.width,
        info.video.height,
        info.video.max_fps,
        info.video.bitrate_kbps,
        info.video.codec,
        info.enable_dualsense
    );

    // Request a session, retrying with the console's RP-Version if needed.
    let mut target = info.target;
    let mut nonce = None;
    for attempt in 0..3 {
        info.target = target;
        let request = request_session(&info);
        let result = tokio::select! {
            _ = cancel.cancelled() => finish!(QuitReason::Stopped, None, None),
            result = request => result,
        };
        match result {
            Ok(r) => {
                nonce = Some(r.nonce);
                break;
            }
            Err(SessionRequestError::VersionMismatch(server_target)) => {
                if attempt >= 2 {
                    finish!(
                        QuitReason::SessionRequestRpVersionMismatch,
                        None,
                        Some("RP-Version mismatch".into())
                    );
                }
                tracing::info!("retrying session with server RP-Version target {server_target:?}");
                target = server_target;
            }
            Err(SessionRequestError::Quit(reason)) => {
                finish!(reason, None, None);
            }
            Err(SessionRequestError::Error(e)) => {
                finish!(QuitReason::SessionRequestUnknown, None, Some(e.to_string()));
            }
        }
    }
    let Some(nonce) = nonce else {
        finish!(QuitReason::SessionRequestUnknown, None, None);
    };
    let crypt = match auth_crypt(&info, &nonce) {
        Ok(crypt) => crypt,
        Err(e) => finish!(QuitReason::SessionRequestUnknown, None, Some(e.to_string())),
    };
    tracing::info!("session: request accepted");
    let crypt_for_stream = crypt.clone();

    let _ = events.send(SessionEvent::Phase(SessionPhase::StartingCtrl));
    let addr = match resolve_host(&info.host).await {
        Ok(addr) => addr,
        Err(e) => finish!(QuitReason::SessionRequestUnknown, None, Some(e.to_string())),
    };
    let ctrl_connect = ctrl::connect(addr, &mut info, crypt.clone());
    let connection = tokio::select! {
        _ = cancel.cancelled() => finish!(QuitReason::Stopped, None, None),
        result = ctrl_connect => result,
    };
    let connection = match connection {
        Ok(connection) => connection,
        Err(e) => {
            let reason = if e == Error::ConnectionRefused {
                QuitReason::CtrlConnectionRefused
            } else {
                QuitReason::CtrlConnectFailed
            };
            finish!(reason, None, Some(e.to_string()));
        }
    };
    tracing::info!("session: ctrl connected");
    let mut ctrl: CtrlHandle = ctrl::spawn(connection);

    // Wait for the session id, handling any PIN request in between.
    let mut pin_incorrect = false;
    let session_id: String = 'wait_ctrl: loop {
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => finish!(QuitReason::Stopped, None, None),
                _ = &mut deadline => {
                    finish!(QuitReason::CtrlUnknown, None, Some("ctrl did not send a session id".into()));
                }
                command = commands.recv() => {
                    match command {
                        Some(SessionCommand::Stop) | None => finish!(QuitReason::Stopped, None, None),
                        Some(SessionCommand::GotoBed) => {
                            let _ = ctrl.commands.send(CtrlCommand::GotoBed).await;
                        }
                        _ => {}
                    }
                }
                event = ctrl.events.recv() => {
                    match event {
                        Some(CtrlEvent::SessionId(id)) => {
                            break 'wait_ctrl id;
                        }
                        event
                        @ (Some(CtrlEvent::LoginPinRequest)
                        | Some(CtrlEvent::LoginPinIncorrect)) => {
                            // Chiaki re-requests the PIN on PIN_INCORRECT instead
                            // of waiting for the console to ask again.
                            let incorrect = pin_incorrect
                                || matches!(event, Some(CtrlEvent::LoginPinIncorrect));
                            pin_incorrect = true;
                            let _ = events.send(SessionEvent::Phase(SessionPhase::WaitingPin));
                            let _ = events.send(SessionEvent::LoginPinRequest { incorrect });
                            // Wait indefinitely for the browser's PIN.
                            loop {
                                tokio::select! {
                                    _ = cancel.cancelled() => finish!(QuitReason::Stopped, None, None),
                                    command = commands.recv() => {
                                        match command {
                                            Some(SessionCommand::SubmitPin(pin)) => {
                                                let _ = ctrl.commands.send(CtrlCommand::SetLoginPin(pin)).await;
                                                break;
                                            }
                                            Some(SessionCommand::Stop) | None => {
                                                finish!(QuitReason::Stopped, None, None)
                                            }
                                            Some(SessionCommand::GotoBed) => {
                                                let _ = ctrl.commands.send(CtrlCommand::GotoBed).await;
                                            }
                                            _ => {}
                                        }
                                    }
                                    event = ctrl.events.recv() => {
                                        match event {
                                            Some(CtrlEvent::LoginPinIncorrect) => pin_incorrect = true,
                                            Some(CtrlEvent::LoginPinRequest) => {
                                                let _ = events.send(SessionEvent::LoginPinRequest { incorrect: true });
                                            }
                                            Some(CtrlEvent::Closed) => finish!(QuitReason::CtrlUnknown, None, None),
                                            Some(CtrlEvent::Error(e)) => finish!(QuitReason::CtrlUnknown, None, Some(e.to_string())),
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            // Re-enter the session-id wait with a fresh deadline.
                            break;
                        }
                        Some(CtrlEvent::Closed) => {
                            finish!(QuitReason::CtrlUnknown, None, Some("ctrl closed before session id".into()));
                        }
                        Some(CtrlEvent::Error(e)) => {
                            finish!(QuitReason::CtrlUnknown, None, Some(e.to_string()));
                        }
                        _ => {}
                    }
                }
            }
        }
    };

    let _ = events.send(SessionEvent::Phase(SessionPhase::StartingStream));

    // Senkusha (MTU/RTT probing) runs before the stream connection. Current
    // PS5 firmware expects this initialization; chiaki-ng always runs it.
    let mut mtu_in = 1454u32;
    let mut mtu_out = 1454u32;
    let mut rtt_us = 1000u64;
    let mut measured_rtt_ms = None;
    if options.senkusha {
        let senkusha = tokio::select! {
            _ = cancel.cancelled() => finish!(QuitReason::Stopped, None, None),
            result = timeout(
                Duration::from_secs(40),
                crate::senkusha::run(addr.ip(), &cancel),
            ) => result,
        };
        match senkusha {
            Ok(Ok(result)) => {
                mtu_in = result.mtu_in;
                mtu_out = result.mtu_out;
                rtt_us = result.rtt_us;
                measured_rtt_ms = Some(result.rtt_us as f64 / 1000.0);
                tracing::info!("senkusha: mtu_in={mtu_in} mtu_out={mtu_out} rtt={rtt_us}us");
            }
            Ok(Err(e)) => {
                tracing::warn!("senkusha failed ({e}); using fallback MTU/RTT");
            }
            Err(_) => {
                tracing::warn!("senkusha timed out; using fallback MTU/RTT");
            }
        }
    }
    if let Some(rtt_ms) = measured_rtt_ms {
        let _ = events.send(SessionEvent::ConnectionQuality { rtt_ms });
    }
    let rtt_ms = (rtt_us / 1000) as u32;
    // The launch spec advertises mtu_in; Takion message fragmentation uses the
    // smaller of the two measured directions, as chiaki-ng does.
    let fragment_mtu = mtu_in.min(mtu_out);
    tracing::info!("session: starting stream connection");
    let mut handshake_key = [0u8; 16];
    rng::fill(&mut handshake_key);
    let ecdh = match Ecdh::new() {
        Ok(ecdh) => ecdh,
        Err(e) => finish!(QuitReason::CryptoFailed, None, Some(e.to_string())),
    };

    let stream_addr = std::net::SocketAddr::new(addr.ip(), STREAM_PORT);
    let takion_info = TakionConnectInfo {
        version: target.takion_protocol_version(),
        enable_crypt: true,
        enable_dualsense: info.enable_dualsense,
    };
    let takion_connect = takion::connect(stream_addr, takion_info);
    let takion = tokio::select! {
        _ = cancel.cancelled() => finish!(QuitReason::Stopped, None, None),
        result = takion_connect => result,
    };
    let takion = match takion {
        Ok(takion) => takion,
        Err(e) => finish!(
            QuitReason::StreamConnectionUnknown,
            None,
            Some(e.to_string())
        ),
    };

    let (stream_cmd_tx, stream_cmd_rx) = mpsc::channel(8);
    let stream_cancel = cancel.child_token();
    let stream_events = events.clone();
    let stream_video = video_tx.clone();
    let stream_audio = audio_tx.clone();
    let stream_input = input_rx.clone();
    let stream_stats = stats.clone();
    let verify = options.verify_ecdh;
    let mut stream_join = tokio::spawn(connection::run(
        takion,
        session_id,
        target,
        info.video,
        mtu_in,
        fragment_mtu,
        rtt_ms,
        crypt_for_stream,
        handshake_key,
        ecdh,
        verify,
        info.enable_dualsense,
        stream_events,
        stream_video,
        stream_audio,
        stream_input,
        stream_stats,
        stream_cmd_rx,
        stream_cancel.clone(),
    ));

    let outcome: StreamOutcome = loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                stream_cancel.cancel();
                let _ = stream_cmd_tx.send(StreamCommand::Stop).await;
                match timeout(Duration::from_secs(2), &mut stream_join).await {
                    Ok(Ok(o)) => break o,
                    _ => {
                        stream_join.abort();
                        break StreamOutcome { quit_reason: QuitReason::Stopped, reason_str: None };
                    }
                }
            }
            result = &mut stream_join => {
                break match result {
                    Ok(outcome) => outcome,
                    Err(e) => StreamOutcome {
                        quit_reason: QuitReason::StreamConnectionUnknown,
                        reason_str: Some(e.to_string()),
                    },
                };
            }
            command = commands.recv() => {
                match command {
                    Some(SessionCommand::RequestKeyframe) => {
                        let _ = stream_cmd_tx.try_send(StreamCommand::RequestKeyframe);
                    }
                    Some(SessionCommand::GotoBed) => {
                        let _ = ctrl.commands.send(CtrlCommand::GotoBed).await;
                    }
                    Some(SessionCommand::SubmitPin(_)) => {}
                    Some(SessionCommand::Stop) | None => {
                        stream_cancel.cancel();
                        let _ = stream_cmd_tx.try_send(StreamCommand::Stop);
                    }
                }
            }
            event = ctrl.events.recv() => {
                match event {
                    Some(CtrlEvent::Closed) => {
                        tracing::warn!("ctrl: console closed the control connection while streaming");
                        stream_cancel.cancel();
                        let _ = stream_cmd_tx.try_send(StreamCommand::Stop);
                        break StreamOutcome {
                            quit_reason: QuitReason::CtrlUnknown,
                            reason_str: Some("control connection closed by the console".into()),
                        };
                    }
                    Some(CtrlEvent::Error(e)) => {
                        tracing::warn!("ctrl: control connection failed while streaming: {e}");
                        stream_cancel.cancel();
                        let _ = stream_cmd_tx.try_send(StreamCommand::Stop);
                        break StreamOutcome {
                            quit_reason: QuitReason::CtrlUnknown,
                            reason_str: Some(e.to_string()),
                        };
                    }
                    None => {
                        tracing::warn!("ctrl: control task ended while streaming");
                        stream_cancel.cancel();
                        let _ = stream_cmd_tx.try_send(StreamCommand::Stop);
                        break StreamOutcome {
                            quit_reason: QuitReason::CtrlUnknown,
                            reason_str: Some("control task ended".into()),
                        };
                    }
                    _ => {}
                }
            }
        }
    };

    ctrl.stop().await;
    let _ = events.send(SessionEvent::Phase(SessionPhase::Stopped));
    finish!(outcome.quit_reason, outcome.reason_str, None);
}
