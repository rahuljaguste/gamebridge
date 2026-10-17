// SPDX-License-Identifier: AGPL-3.0-only
//! Ctrl channel: the TCP control connection on port 9295.

use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::crypto::rpcrypt::RPCrypt;
use crate::error::{Error, Result};
use crate::http;
use crate::session::{ConnectInfo, SESSION_PORT, ctrl_path};
use crate::target::{Codec, Target};
use crate::util::{base64, rng};

pub const MSG_SESSION_ID: u16 = 0x33;
pub const MSG_HEARTBEAT_REQ: u16 = 0xfe;
pub const MSG_HEARTBEAT_REP: u16 = 0x1fe;
pub const MSG_LOGIN_PIN_REQ: u16 = 0x4;
pub const MSG_LOGIN_PIN_REP: u16 = 0x8004;
pub const MSG_LOGIN: u16 = 0x5;
pub const MSG_GOTO_BED: u16 = 0x50;
pub const MSG_KEYBOARD_ENABLE_TOGGLE: u16 = 0x20;
pub const MSG_KEYBOARD_OPEN: u16 = 0x21;
pub const MSG_KEYBOARD_CLOSE_REMOTE: u16 = 0x22;
pub const MSG_KEYBOARD_TEXT_CHANGE_REQ: u16 = 0x23;
pub const MSG_KEYBOARD_TEXT_CHANGE_RES: u16 = 0x24;
pub const MSG_KEYBOARD_CLOSE_REQ: u16 = 0x25;
const MSG_ENABLE_DUALSENSE: u16 = 0x13;
const MSG_DUALSENSE_CONNECT: u16 = 0x11;
const MSG_MIC_TOGGLE: u16 = 0x36;
const MSG_DISPLAY_DEVICES: u16 = 0x910;

const CTRL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME: usize = 1 << 20;

#[derive(Debug)]
pub enum CtrlCommand {
    Send { ty: u16, payload: Vec<u8> },
    SetLoginPin(Vec<u8>),
    GotoBed,
    KeyboardSetText(String),
    KeyboardAccept,
    KeyboardReject,
    Stop,
}

#[derive(Debug)]
pub enum CtrlEvent {
    SessionId(String),
    LoginPinRequest,
    LoginSuccess,
    LoginPinIncorrect,
    Error(Error),
    Closed,
}

pub struct CtrlHandle {
    pub commands: mpsc::Sender<CtrlCommand>,
    pub events: mpsc::UnboundedReceiver<CtrlEvent>,
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

impl CtrlHandle {
    pub async fn stop(mut self) {
        let _ = self.commands.send(CtrlCommand::Stop).await;
        self.cancel.cancel();
        let _ = timeout(Duration::from_secs(2), &mut self.join).await;
        self.join.abort();
    }
}

pub struct CtrlConnection {
    pub stream: TcpStream,
    pub crypt: RPCrypt,
    pub local_counter: u64,
    pub remote_counter: u64,
    pub server_type: Option<u8>,
    pub buffered: BytesMut,
    pub enable_dualsense: bool,
}

fn encrypt_b64(crypt: &RPCrypt, counter: &mut u64, data: &[u8]) -> Result<String> {
    let mut buf = data.to_vec();
    crypt.encrypt(*counter, &mut buf)?;
    *counter += 1;
    Ok(base64::encode(&buf))
}

/// Open the ctrl TCP connection and consume the HTTP response.
pub async fn connect(
    addr: std::net::SocketAddr,
    info: &mut ConnectInfo,
    crypt: RPCrypt,
) -> Result<CtrlConnection> {
    let mut stream = timeout(CTRL_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::ConnectionRefused)?;
    stream.set_nodelay(true).ok();

    let mut local_counter = 0u64;
    let mut remote_counter = 0u64;

    let auth = encrypt_b64(&crypt, &mut local_counter, &info.regist_key)?;
    let did = encrypt_b64(&crypt, &mut local_counter, &info.did)?;
    let ostype = encrypt_b64(&crypt, &mut local_counter, b"Win10.0.0\0")?;

    let mut extra = String::new();
    if info.target >= Target::Ps4_10 {
        let bitrate = encrypt_b64(&crypt, &mut local_counter, &[0u8; 4])?;
        extra.push_str(&format!("RP-StartBitrate: {bitrate}\r\n"));
    }
    if info.target.is_ps5() {
        let streaming_type: u32 = match info.video.codec {
            Codec::H265 => 2,
            Codec::H265Hdr => 3,
            Codec::H264 => 1,
        };
        let bytes = streaming_type.to_le_bytes();
        let value = encrypt_b64(&crypt, &mut local_counter, &bytes)?;
        extra.push_str(&format!("RP-StreamingType: {value}\r\n"));
    }

    let version = info.target.rp_version().ok_or(Error::InvalidData)?;
    let hostname = addr.ip().to_string();
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: remoteplay Windows\r\nConnection: keep-alive\r\nContent-Length: 0\r\nRP-Auth: {}\r\nRP-Version: {}\r\nRP-Did: {}\r\nRP-ControllerType: 3\r\nRP-ClientType: 11\r\nRP-OSType: {}\r\nRP-ConPath: 1\r\n{}\r\n",
        ctrl_path(info.target),
        hostname,
        SESSION_PORT,
        auth,
        version,
        did,
        ostype,
        extra
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| Error::Network)?;

    let mut buf = BytesMut::with_capacity(4096);
    let header_size = timeout(
        CTRL_TIMEOUT,
        http::read_http_header(&mut stream, &mut buf, 4096),
    )
    .await
    .map_err(|_| Error::Timeout)??;
    let response = http::parse_response(&buf[..header_size])?;
    if response.code != 200 {
        return Err(Error::InvalidResponse);
    }

    let mut server_type = None;
    if let Some(value) = response.get("RP-Server-Type") {
        let value = value.trim_end_matches('\0');
        if let Ok(decoded) = base64::decode(value)
            && decoded.len() == 16
        {
            let mut data = decoded;
            if crypt.decrypt(remote_counter, &mut data).is_ok() {
                remote_counter += 1;
                server_type = Some(data[0]);
            }
        }
    }

    if let Some(server_type) = server_type {
        if server_type == 0 && info.auto_downgrade && info.video.height == 1080 {
            tracing::info!("server is a base PS4; downgrading to 720p");
            info.video = info.video.downgrade_to_720p();
        }
        if (server_type == 0 || server_type == 1) && info.video.codec != Codec::H264 {
            tracing::info!("server does not support HEVC; falling back to H.264");
            info.video.codec = Codec::H264;
        }
    } else {
        tracing::warn!("no valid RP-Server-Type in ctrl response");
    }

    let mut buffered = BytesMut::new();
    if buf.len() > header_size {
        buffered.extend_from_slice(&buf[header_size..]);
    }

    Ok(CtrlConnection {
        stream,
        crypt,
        local_counter,
        remote_counter,
        server_type,
        buffered,
        enable_dualsense: info.enable_dualsense,
    })
}

pub fn spawn(connection: CtrlConnection) -> CtrlHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let join = tokio::spawn(run(connection, cmd_rx, evt_tx, task_cancel));
    CtrlHandle {
        commands: cmd_tx,
        events: evt_rx,
        cancel,
        join,
    }
}

async fn run(
    connection: CtrlConnection,
    mut commands: mpsc::Receiver<CtrlCommand>,
    events: mpsc::UnboundedSender<CtrlEvent>,
    cancel: CancellationToken,
) {
    let (mut reader, mut writer) = connection.stream.into_split();
    let crypt = connection.crypt;
    let mut local_counter = connection.local_counter;
    let mut remote_counter = connection.remote_counter;
    let mut buf = connection.buffered;
    let mut login_pin_requested = false;
    let mut keyboard_counter: u32 = 0;
    let enable_dualsense = connection.enable_dualsense;

    loop {
        // Process any complete frames already buffered.
        while let Some((ty, payload)) = take_frame(&mut buf, &crypt, &mut remote_counter) {
            match ty {
                MSG_SESSION_ID => {
                    let id = parse_session_id(&payload).unwrap_or_else(|| {
                        let id = fallback_session_id();
                        tracing::warn!(
                            "ctrl session id was invalid or shorter than 24 characters; using a {} character fallback",
                            id.len()
                        );
                        id
                    });
                    let _ = events.send(CtrlEvent::SessionId(id));
                    if let Err(e) = send_session_features(
                        &mut writer,
                        &crypt,
                        &mut local_counter,
                        enable_dualsense,
                    )
                    .await
                    {
                        let _ = events.send(CtrlEvent::Error(e));
                        return;
                    }
                }
                MSG_HEARTBEAT_REQ => {
                    if let Err(e) = send_message(
                        &mut writer,
                        &crypt,
                        &mut local_counter,
                        MSG_HEARTBEAT_REP,
                        &[],
                    )
                    .await
                    {
                        let _ = events.send(CtrlEvent::Error(e));
                        return;
                    }
                }
                MSG_LOGIN_PIN_REQ => {
                    login_pin_requested = true;
                    let _ = events.send(CtrlEvent::LoginPinRequest);
                }
                MSG_LOGIN => match payload.first().copied() {
                    Some(0) => {
                        let _ = events.send(CtrlEvent::LoginSuccess);
                    }
                    Some(1) => {
                        let _ = events.send(CtrlEvent::LoginPinIncorrect);
                        if login_pin_requested {
                            let _ = events.send(CtrlEvent::LoginPinRequest);
                        }
                    }
                    _ => {}
                },
                MSG_KEYBOARD_OPEN | MSG_KEYBOARD_TEXT_CHANGE_RES | MSG_KEYBOARD_CLOSE_REMOTE => {
                    // Keyboard UI is out of scope in v1; consume the message.
                    tracing::debug!("ctrl keyboard message {ty:#x} ignored");
                }
                _ => tracing::debug!("ctrl message {ty:#x} ignored"),
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => break,
            read = reader.read_buf(&mut buf) => {
                match read {
                    Ok(0) => {
                        tracing::info!("ctrl: console closed the connection");
                        let _ = events.send(CtrlEvent::Closed);
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("ctrl: read failed: {e}");
                        let _ = events.send(CtrlEvent::Error(Error::Network));
                        break;
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                let result = match command {
                    CtrlCommand::Send { ty, payload } => {
                        send_message(&mut writer, &crypt, &mut local_counter, ty, &payload).await
                    }
                    CtrlCommand::SetLoginPin(pin) => {
                        login_pin_requested = false;
                        send_message(&mut writer, &crypt, &mut local_counter, MSG_LOGIN_PIN_REP, &pin).await
                    }
                    CtrlCommand::GotoBed => {
                        send_message(&mut writer, &crypt, &mut local_counter, MSG_GOTO_BED, &[]).await
                    }
                    CtrlCommand::KeyboardSetText(text) => {
                        let payload = keyboard_text_payload(&text, &mut keyboard_counter);
                        send_message(&mut writer, &crypt, &mut local_counter, MSG_KEYBOARD_TEXT_CHANGE_REQ, &payload).await
                    }
                    CtrlCommand::KeyboardAccept => {
                        send_message(&mut writer, &crypt, &mut local_counter, MSG_KEYBOARD_CLOSE_REQ, &[0, 0, 0, 0]).await
                    }
                    CtrlCommand::KeyboardReject => {
                        send_message(&mut writer, &crypt, &mut local_counter, MSG_KEYBOARD_CLOSE_REQ, &[0, 0, 0, 1]).await
                    }
                    CtrlCommand::Stop => break,
                };
                if let Err(e) = result {
                    let _ = events.send(CtrlEvent::Error(e));
                    break;
                }
            }
        }
    }
}

fn keyboard_text_payload(text: &str, counter: &mut u32) -> Vec<u8> {
    // CtrlKeyboardTextRequestMessage: counter, len, 8 unknown, 16 unknown, len.
    let mut payload = vec![0u8; 0x24];
    *counter = counter.wrapping_add(1);
    payload[0..4].copy_from_slice(&counter.to_be_bytes());
    let len = text.len() as u32;
    payload[4..8].copy_from_slice(&len.to_be_bytes());
    payload[0x20..0x24].copy_from_slice(&len.to_be_bytes());
    payload.extend_from_slice(text.as_bytes());
    payload
}

fn parse_session_id(payload: &[u8]) -> Option<String> {
    if payload.len() < 2 {
        return None;
    }
    if payload[0] != 0x4a {
        tracing::warn!(
            "ctrl session id has unexpected first byte {:#04x} (payload length {})",
            payload[0],
            payload.len()
        );
    }
    let body = &payload[1..];
    // chiaki-ng treats ids shorter than 24 characters as invalid and uses a
    // generated fallback; newer consoles send longer ids.
    if body.len() >= 79 || body.len() < 24 {
        return None;
    }
    if !body.iter().all(|c| c.is_ascii_alphanumeric()) {
        tracing::warn!("ctrl session id contains non-alphanumeric characters");
        return None;
    }
    tracing::info!("ctrl: session id received ({} characters)", body.len());
    Some(String::from_utf8_lossy(body).to_string())
}

/// Generated when the console sends a short/invalid session id, matching
/// chiaki-ng: a 7-digit monotonic timestamp prefix plus 48 random bytes in
/// base64, for a total of 71 characters. The exact length matters: current
/// PS5 firmware accepts chiaki-ng's 71-character fallback but drops the
/// stream when the key is longer.
fn fallback_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 10_000_000;
    let mut bytes = [0u8; 48];
    rng::fill(&mut bytes);
    format!("{secs:07}{}", base64::encode(&bytes))
}

async fn send_session_features(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    crypt: &RPCrypt,
    local_counter: &mut u64,
    enable_dualsense: bool,
) -> Result<()> {
    if enable_dualsense {
        tracing::info!("ctrl: enabling DualSense features (experimental)");
        send_message(
            writer,
            crypt,
            local_counter,
            MSG_ENABLE_DUALSENSE,
            &[0x00, 0x40, 0x00],
        )
        .await?;
        send_message(
            writer,
            crypt,
            local_counter,
            MSG_DUALSENSE_CONNECT,
            &[
                0xa0, 0xab, 0x51, 0xbd, 0xd1, 0x7e, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
                0x00, 0x00,
            ],
        )
        .await?;
    }

    // chiaki-ng sends these unconditionally after the session id. Newer
    // console firmware expects the mic and display services to be initialized
    // before the stream starts.
    for _ in 0..2 {
        send_message(writer, crypt, local_counter, MSG_MIC_TOGGLE, &[0, 1, 1, 89]).await?;
    }
    send_message(
        writer,
        crypt,
        local_counter,
        MSG_DISPLAY_DEVICES,
        &[0, 0, 0, 0],
    )
    .await?;
    tracing::info!("ctrl: sent session feature initialization");
    Ok(())
}

fn take_frame(
    buf: &mut BytesMut,
    crypt: &RPCrypt,
    remote_counter: &mut u64,
) -> Option<(u16, Vec<u8>)> {
    if buf.len() < 8 {
        return None;
    }
    let payload_size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if payload_size > MAX_FRAME {
        tracing::error!("ctrl frame too large: {payload_size}");
        buf.clear();
        return None;
    }
    if buf.len() < 8 + payload_size {
        return None;
    }
    let ty = u16::from_be_bytes([buf[4], buf[5]]);
    let mut frame = buf.split_to(8 + payload_size);
    let payload = &mut frame[8..];
    if !payload.is_empty() {
        if crypt.decrypt(*remote_counter, payload).is_err() {
            tracing::error!("ctrl payload decryption failed for type {ty:#x}");
            return None;
        }
        *remote_counter += 1;
    }
    Some((ty, payload.to_vec()))
}

async fn send_message(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    crypt: &RPCrypt,
    local_counter: &mut u64,
    ty: u16,
    payload: &[u8],
) -> Result<()> {
    let mut enc = payload.to_vec();
    if !payload.is_empty() {
        crypt.encrypt(*local_counter, &mut enc)?;
        *local_counter += 1;
    }
    let mut header = [0u8; 8];
    header[0..4].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    header[4..6].copy_from_slice(&ty.to_be_bytes());
    writer
        .write_all(&header)
        .await
        .map_err(|_| Error::Network)?;
    if !enc.is_empty() {
        writer.write_all(&enc).await.map_err(|_| Error::Network)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_validation() {
        let id = "abcdefghijklmnopqrstuvwx"; // 24 characters
        let mut payload = vec![0x4a];
        payload.extend_from_slice(id.as_bytes());
        assert_eq!(parse_session_id(&payload), Some(id.into()));
        // Short ids are rejected so the caller can generate a fallback.
        assert_eq!(parse_session_id(&[0x4a, b'a', b'B', b'9']), None);
        assert_eq!(parse_session_id(&[0x4a, b'-']), None);
        assert_eq!(parse_session_id(&[0x4a]), None);
    }
}
