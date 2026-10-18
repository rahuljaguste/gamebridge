// SPDX-License-Identifier: AGPL-3.0-only
//! Stream connection: Takion handshake, launch spec, media forwarding,
//! feedback and congestion.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, watch};
use tokio::time::{MissedTickBehavior, interval, timeout};
use tokio_util::sync::CancellationToken;

use crate::controller::*;
use crate::crypto::ecdh::Ecdh;
use crate::crypto::gkcrypt::GKCrypt;
use crate::crypto::rpcrypt::RPCrypt;
use crate::error::{Error, QuitReason};
use crate::feedback::{FeedbackState, HistoryBuffer, history_button, history_touch};
use crate::media::audio::AudioFrame;
use crate::media::ingest::{self, IngestCommand, MediaEvent, VideoFrame, VideoProfileInfo};
use crate::packet_stats::PacketStats;
use crate::session::{SessionEvent, StreamCommand};
use crate::stream::{launchspec, protobuf};
use crate::takion::packet::CongestionPacket;
use crate::takion::{TakionCommand, TakionEvent, TakionHandle};
use crate::target::{Target, VideoProfile};
use crate::util::base64;

pub struct StreamOutcome {
    pub quit_reason: QuitReason,
    pub reason_str: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    takion: TakionHandle,
    session_id: String,
    target: Target,
    video_profile: VideoProfile,
    mtu: u32,
    fragment_mtu: u32,
    rtt_ms: u32,
    crypt: RPCrypt,
    handshake_key: [u8; 16],
    ecdh: Ecdh,
    verify_ecdh: bool,
    enable_dualsense: bool,
    events: mpsc::UnboundedSender<SessionEvent>,
    video_tx: mpsc::Sender<VideoFrame>,
    audio_tx: mpsc::Sender<AudioFrame>,
    input: watch::Receiver<ControllerState>,
    stats: Arc<PacketStats>,
    mut commands: mpsc::Receiver<StreamCommand>,
    cancel: CancellationToken,
) -> StreamOutcome {
    match run_inner(
        takion,
        session_id,
        target,
        video_profile,
        mtu,
        fragment_mtu,
        rtt_ms,
        crypt,
        handshake_key,
        ecdh,
        verify_ecdh,
        enable_dualsense,
        &events,
        video_tx,
        audio_tx,
        input,
        stats,
        &mut commands,
        &cancel,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::error!("stream connection failed: {e}");
            let reason = if e == Error::Disconnected {
                QuitReason::StreamConnectionRemoteDisconnected
            } else {
                QuitReason::StreamConnectionUnknown
            };
            StreamOutcome {
                quit_reason: reason,
                reason_str: Some(e.to_string()),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    mut takion: TakionHandle,
    session_id: String,
    target: Target,
    video_profile: VideoProfile,
    mtu: u32,
    fragment_mtu: u32,
    rtt_ms: u32,
    crypt: RPCrypt,
    handshake_key: [u8; 16],
    ecdh: Ecdh,
    verify_ecdh: bool,
    enable_dualsense: bool,
    events: &mpsc::UnboundedSender<SessionEvent>,
    video_tx: mpsc::Sender<VideoFrame>,
    audio_tx: mpsc::Sender<AudioFrame>,
    input: watch::Receiver<ControllerState>,
    stats: Arc<PacketStats>,
    commands: &mut mpsc::Receiver<StreamCommand>,
    cancel: &CancellationToken,
) -> std::result::Result<StreamOutcome, Error> {
    // Media ingest owns frame assembly and emits media format events.
    let (ingest_cmd_tx, ingest_cmd_rx) = mpsc::channel(8);
    let (av_tx, av_rx) = mpsc::channel(4096);
    let (media_tx, mut media_rx) = mpsc::unbounded_channel();
    let ingest_cancel = CancellationToken::new();
    let ingest_join = tokio::spawn(ingest::run(
        ingest_cmd_rx,
        av_rx,
        video_tx,
        audio_tx,
        media_tx,
        takion.commands.clone(),
        stats.clone(),
        ingest_cancel.clone(),
    ));

    let result = async {
        // Chiaki starts congestion feedback right after the Takion handshake,
        // before the launch spec. The console expects these liveness packets
        // during session startup, not only once media is flowing.
        let congestion_handle = tokio::spawn(congestion_loop(
            stats.clone(),
            takion.commands.clone(),
            cancel.child_token(),
        ));

        // BIG / launch spec.
        let mut launch = launchspec::format(
            target,
            mtu,
            rtt_ms,
            &handshake_key,
            video_profile.width,
            video_profile.height,
            video_profile.max_fps,
            video_profile.codec,
            video_profile.bitrate_kbps,
        )
        .into_bytes();
        launch.push(0);
        let encrypted = encrypt_launch_spec(&crypt, &launch)?;
        let launch_b64 = base64::encode(&encrypted);
        let pubkey = ecdh.public_key();
        let sig = ecdh.sign_public_key(&handshake_key);
        let big = protobuf::encode_big(
            target.takion_protocol_version() as u32,
            &session_id,
            &launch_b64,
            &pubkey,
            &sig,
        );
        let big_len = big.len();
        if std::env::var_os("PSRP_DUMP_STREAM").is_some() {
            fn hex(bytes: &[u8]) -> String {
                bytes.iter().map(|b| format!("{b:02x}")).collect()
            }
            tracing::info!(
                "dump: launch spec {}",
                String::from_utf8_lossy(&launch[..launch.len().saturating_sub(1)])
            );
            tracing::info!("dump: launch b64 {launch_b64}");
            tracing::info!("dump: session id {session_id:?}");
            tracing::info!("dump: handshake key {}", hex(&handshake_key));
            tracing::info!("dump: ecdh pubkey {}", hex(&pubkey));
            tracing::info!("dump: ecdh sig {}", hex(&sig));
            tracing::info!("dump: BIG {big_len} {}", hex(&big));
        }
        send_big(&mut takion, big, fragment_mtu).await?;
        tracing::info!(
            "stream: queued BIG launch spec ({big_len} bytes, fragment mtu {fragment_mtu})"
        );

        // BANG.
        let bang = wait_for_bang(&mut takion, cancel).await?;
        tracing::info!(
            "stream: received BANG (version_accepted={}, key_accepted={})",
            bang.version_accepted,
            bang.encrypted_key_accepted
        );
        if !bang.version_accepted {
            return Err(Error::Protocol("console rejected protocol version".into()));
        }
        if !bang.encrypted_key_accepted {
            return Err(Error::Protocol("console rejected encrypted key".into()));
        }
        if bang.ecdh_pub_key.is_empty() || bang.ecdh_sig.is_empty() {
            return Err(Error::Protocol("BANG missing ECDH key/signature".into()));
        }
        let secret = ecdh.derive_secret(
            &bang.ecdh_pub_key,
            &handshake_key,
            &bang.ecdh_sig,
            verify_ecdh,
        )?;

        let local = Box::new(GKCrypt::new(2, &handshake_key, &secret)?);
        let remote = Box::new(GKCrypt::new(3, &handshake_key, &secret)?);
        takion
            .commands
            .send(TakionCommand::SetCrypt { local, remote })
            .await
            .map_err(|_| Error::Disconnected)?;
        tracing::info!("stream: encryption keys installed");

        // STREAMINFO.
        let info = wait_for_stream_info(&mut takion, cancel).await?;
        tracing::info!(
            "stream: received stream info ({} profile(s), audio header {} bytes)",
            info.resolutions.len(),
            info.audio_header.len()
        );
        let profiles: Vec<VideoProfileInfo> = info
            .resolutions
            .iter()
            .map(|r| VideoProfileInfo {
                width: r.width,
                height: r.height,
                header: Bytes::copy_from_slice(&r.header),
            })
            .collect();
        if profiles.is_empty() {
            return Err(Error::Protocol("stream info has no video profiles".into()));
        }
        let _ = ingest_cmd_tx
            .send(IngestCommand::StreamInfo {
                profiles,
                audio_header: info.audio_header,
            })
            .await;
        let ack = protobuf::encode_streaminfo_ack();
        takion
            .commands
            .send(TakionCommand::SendMessageData {
                flags: 1,
                channel: 9,
                payload: ack,
            })
            .await
            .map_err(|_| Error::Disconnected)?;

        // chiaki-ng sends the controller connection only after STREAMINFO and
        // its ACK. The PS5 Remote Play server crashes if this first encrypted
        // message arrives during startup ("invalid GMAC", CE-108262-9).
        let controller = protobuf::encode_controller_connection(enable_dualsense);
        takion
            .commands
            .send(TakionCommand::SendMessageData {
                flags: 1,
                channel: 1,
                payload: controller,
            })
            .await
            .map_err(|_| Error::Disconnected)?;
        tracing::info!("stream: sent controller connection");

        let _ = events.send(SessionEvent::Connected);

        let feedback_handle = spawn_feedback(
            input,
            takion.commands.clone(),
            cancel.child_token(),
        );
        let mut heartbeat = interval(Duration::from_secs(1));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut outcome = StreamOutcome {
            quit_reason: QuitReason::StreamConnectionUnknown,
            reason_str: None,
        };
        // The console reports either seconds or milliseconds, consistently for
        // a session; decide from the first valid sample and keep that unit.
        let mut rtt_seconds = None;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    outcome.quit_reason = QuitReason::Stopped;
                    break;
                }
                _ = heartbeat.tick() => {
                    let payload = protobuf::encode_heartbeat();
                    let _ = takion.commands.try_send(TakionCommand::SendMessageData {
                        flags: 1,
                        channel: 1,
                        payload,
                    });
                }
                event = takion.events.recv() => {
                    let Some(event) = event else {
                        outcome.quit_reason = QuitReason::StreamConnectionUnknown;
                        break;
                    };
                    match event {
                        TakionEvent::MessageData { data_type: 0, payload } => {
                            match protobuf::message_type(&payload) {
                                Ok(protobuf::TYPE_DISCONNECT) => {
                                    if let Ok(Some(reason)) = protobuf::parse_disconnect_reason(&payload) {
                                        tracing::info!("console disconnected: {reason}");
                                        let _ = events.send(SessionEvent::RemoteDisconnect { reason: reason.clone() });
                                        outcome.reason_str = Some(reason.clone());
                                        outcome.quit_reason = if reason == "Server shutting down" {
                                            QuitReason::StreamConnectionRemoteShutdown
                                        } else {
                                            QuitReason::StreamConnectionRemoteDisconnected
                                        };
                                        break;
                                    }
                                }
                                Ok(protobuf::TYPE_CONNECTIONQUALITY) => {
                                    match protobuf::parse_connection_quality(&payload) {
                                        Ok(quality) => {
                                            if let Some(rtt_ms) = quality.rtt_ms(&mut rtt_seconds) {
                                                tracing::debug!(
                                                    raw = ?quality.rtt,
                                                    rtt_ms,
                                                    "stream: console connection quality"
                                                );
                                                let _ = events.send(SessionEvent::ConnectionQuality { rtt_ms });
                                            }
                                        }
                                        Err(e) => {
                                            tracing::debug!("stream: invalid connection quality: {e}");
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        TakionEvent::MessageData { data_type: 7, payload } => {
                            if let Some((unknown, left, right)) = protobuf::parse_rumble(&payload) {
                                let _ = events.send(SessionEvent::Rumble { unknown, left, right });
                            }
                        }
                        TakionEvent::MessageData { data_type: 11, payload } => {
                            if let Some((left, right, type_left, type_right)) =
                                protobuf::parse_trigger_effects(&payload)
                            {
                                let _ = events.send(SessionEvent::TriggerEffects {
                                    type_left,
                                    type_right,
                                    left,
                                    right,
                                });
                            }
                        }
                        TakionEvent::Av { packet, data } => {
                            if av_tx.try_send((packet, data)).is_err() {
                                tracing::debug!("AV queue full; dropping packet");
                            }
                        }
                        TakionEvent::Disconnected => {
                            outcome.quit_reason = QuitReason::StreamConnectionUnknown;
                            break;
                        }
                        _ => {}
                    }
                }
                Some(media) = media_rx.recv() => {
                    match media {
                        MediaEvent::StreamInfo { profiles, audio } => {
                            let _ = events.send(SessionEvent::StreamInfo {
                                profiles,
                                audio,
                            });
                        }
                        MediaEvent::VideoFormat { profile_index, width, height, header } => {
                            let _ = events.send(SessionEvent::VideoFormat {
                                profile_index,
                                width,
                                height,
                                header,
                            });
                        }
                    }
                }
                command = commands.recv() => {
                    match command {
                        Some(StreamCommand::RequestKeyframe) => {
                            tracing::info!("stream: requesting IDR frame");
                            if takion.commands.send(TakionCommand::RequestIdr).await.is_err() {
                                tracing::warn!("stream: failed to queue IDR request");
                            }
                        }
                        Some(StreamCommand::Stop) | None => {
                            outcome.quit_reason = QuitReason::Stopped;
                            break;
                        }
                    }
                }
            }
        }

        let _ = takion.commands.try_send(TakionCommand::SendMessageData {
            flags: 1,
            channel: 1,
            payload: protobuf::encode_disconnect("Client Disconnecting"),
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        feedback_handle.abort();
        congestion_handle.abort();
        Ok::<StreamOutcome, Error>(outcome)
    }
    .await;

    ingest_cancel.cancel();
    ingest_join.abort();
    let _ = takion.stop().await;
    result
}

/// Chunk plan for the launch spec: `(offset, size, continuation)`.
///
/// Mirrors chiaki-ng's `stream_connection_send_big` loop: chunks are capped at
/// `min(mtu_in, mtu_out) - 50`; the first chunk leaves room for the 9-byte data
/// header + 17-byte Takion preamble and later chunks for 8 + 17 bytes.
fn big_chunk_plan(total: usize, mtu: u32) -> Vec<(usize, usize, bool)> {
    let mtu = (mtu as usize).saturating_sub(50).max(27);
    let mut plan = Vec::new();
    let mut buf_pos = 0;
    let mut remaining = total;
    let mut first = true;
    while (mtu < remaining + 26) || (mtu < remaining + 25 && !first) {
        let (size, continuation) = if first {
            (mtu - 26, false)
        } else {
            (mtu - 25, true)
        };
        plan.push((buf_pos, size, continuation));
        buf_pos += size;
        remaining -= size;
        first = false;
    }
    if remaining > 0 {
        plan.push((buf_pos, remaining, !first));
    }
    plan
}

/// Send the launch spec exactly like chiaki-ng's `stream_connection_send_big`:
/// every chunk but the last uses DATA flags 0 and the final chunk uses flags 1.
/// Continuation chunks carry an 8-byte data header instead of the normal
/// 9-byte one.
async fn send_big(
    takion: &mut TakionHandle,
    buf: Vec<u8>,
    mtu: u32,
) -> std::result::Result<(), Error> {
    let plan = big_chunk_plan(buf.len(), mtu);
    let chunks = plan.len();
    for (index, (offset, size, continuation)) in plan.into_iter().enumerate() {
        let flags = if index + 1 == chunks { 1 } else { 0 };
        let payload = buf[offset..offset + size].to_vec();
        let command = if continuation {
            TakionCommand::SendMessageDataCont {
                flags,
                channel: 1,
                payload,
            }
        } else {
            TakionCommand::SendMessageData {
                flags,
                channel: 1,
                payload,
            }
        };
        takion
            .commands
            .send(command)
            .await
            .map_err(|_| Error::Disconnected)?;
    }
    if chunks > 1 {
        tracing::info!(
            "stream: fragmented BIG into {chunks} chunks ({} bytes)",
            buf.len()
        );
    }
    Ok(())
}

/// Encrypt the launch spec the way Chiaki does: generate the RPCrypt
/// keystream by encrypting a zero buffer at counter 0, then XOR the
/// plaintext into it.
///
/// RPCrypt is CFB-128, so this is **not** the same as encrypting the
/// plaintext directly beyond the first 16-byte block: CFB chains on the
/// previous ciphertext block, and Chiaki's construction chains on the
/// previous keystream block (OFB-like). Consoles decrypt the launch spec
/// with this keystream construction, so encrypting the JSON directly yields
/// garbage after the first block and the console aborts the stream.
fn encrypt_launch_spec(crypt: &RPCrypt, launch: &[u8]) -> std::result::Result<Vec<u8>, Error> {
    let mut encrypted = vec![0u8; launch.len()];
    crypt.encrypt(0, &mut encrypted)?;
    for (byte, plain) in encrypted.iter_mut().zip(launch) {
        *byte ^= plain;
    }
    Ok(encrypted)
}

async fn wait_for_bang(
    takion: &mut TakionHandle,
    cancel: &CancellationToken,
) -> std::result::Result<protobuf::Bang, Error> {
    timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Canceled),
                event = takion.events.recv() => {
                    let Some(event) = event else { return Err(Error::Disconnected) };
                    if let TakionEvent::MessageData { data_type: 0, payload } = event {
                        match protobuf::message_type(&payload) {
                            Ok(protobuf::TYPE_BANG) => return protobuf::parse_bang(&payload),
                            Ok(protobuf::TYPE_DISCONNECT) => {
                                let reason = protobuf::parse_disconnect_reason(&payload)?
                                    .unwrap_or_default();
                                tracing::warn!("stream: console sent disconnect: {reason}");
                                return Err(Error::Protocol(reason));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| {
        tracing::warn!("stream: timed out waiting for BANG after the launch spec");
        Error::Timeout
    })?
}

async fn wait_for_stream_info(
    takion: &mut TakionHandle,
    cancel: &CancellationToken,
) -> std::result::Result<protobuf::StreamInfo, Error> {
    timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Canceled),
                event = takion.events.recv() => {
                    let Some(event) = event else { return Err(Error::Disconnected) };
                    if let TakionEvent::MessageData { data_type: 0, payload } = event {
                        match protobuf::message_type(&payload) {
                            Ok(protobuf::TYPE_STREAMINFO) => return protobuf::parse_stream_info(&payload),
                            Ok(protobuf::TYPE_DISCONNECT) => {
                                let reason = protobuf::parse_disconnect_reason(&payload)?
                                    .unwrap_or_default();
                                tracing::warn!("stream: console sent disconnect: {reason}");
                                return Err(Error::Protocol(reason));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| {
        tracing::warn!("stream: timed out waiting for stream info");
        Error::Timeout
    })?
}

fn spawn_feedback(
    mut input: watch::Receiver<ControllerState>,
    takion: mpsc::Sender<TakionCommand>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut prev = ControllerState::idle();
        let mut state_seq: u16 = 0;
        let mut history_seq: u16 = 0;
        let mut ticker = interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            let changed = tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => false,
                result = input.changed() => {
                    if result.is_err() {
                        break;
                    }
                    true
                }
            };
            let now = *input.borrow_and_update();
            let send_state = !changed || !feedback_state_equal(&prev, &now);
            if send_state {
                let state = FeedbackState::from(&now);
                let _ = takion.try_send(TakionCommand::SendFeedbackState {
                    seq: state_seq,
                    state,
                });
                state_seq = state_seq.wrapping_add(1);
            }
            if changed {
                let mut history = HistoryBuffer::new(16);
                collect_history(&prev, &now, &mut history);
                if !history.is_empty() {
                    let payload = history.format();
                    let _ = takion.try_send(TakionCommand::SendFeedbackHistory {
                        seq: history_seq,
                        payload,
                    });
                    history_seq = history_seq.wrapping_add(1);
                }
                prev = now;
            }
        }
    })
}

fn feedback_state_equal(a: &ControllerState, b: &ControllerState) -> bool {
    let close = |x: f32, y: f32| (x - y).abs() < 1e-7;
    a.left_x == b.left_x
        && a.left_y == b.left_y
        && a.right_x == b.right_x
        && a.right_y == b.right_y
        && close(a.gyro_x, b.gyro_x)
        && close(a.gyro_y, b.gyro_y)
        && close(a.gyro_z, b.gyro_z)
        && close(a.accel_x, b.accel_x)
        && close(a.accel_y, b.accel_y)
        && close(a.accel_z, b.accel_z)
        && close(a.orient_x, b.orient_x)
        && close(a.orient_y, b.orient_y)
        && close(a.orient_z, b.orient_z)
        && close(a.orient_w, b.orient_w)
}

fn collect_history(prev: &ControllerState, now: &ControllerState, out: &mut HistoryBuffer) {
    for i in 0..BUTTONS_COUNT {
        let bit = 1u32 << i;
        let a = prev.buttons & bit != 0;
        let b = now.buttons & bit != 0;
        if a != b
            && let Ok(event) = history_button(bit as u64, if b { 0xff } else { 0 })
        {
            out.push(event);
        }
    }
    if prev.l2_state != now.l2_state
        && let Ok(event) = history_button(ANALOG_BUTTON_L2 as u64, now.l2_state)
    {
        out.push(event);
    }
    if prev.r2_state != now.r2_state
        && let Ok(event) = history_button(ANALOG_BUTTON_R2 as u64, now.r2_state)
    {
        out.push(event);
    }
    for i in 0..TOUCHES_MAX {
        let p = prev.touches[i];
        let n = now.touches[i];
        if p.id != n.id && p.id >= 0 {
            out.push(history_touch(false, p.id as u8, p.x, p.y));
        } else if n.id >= 0 && (p.id != n.id || p.x != n.x || p.y != n.y) {
            out.push(history_touch(true, n.id as u8, n.x, n.y));
        }
    }
}

async fn congestion_loop(
    stats: Arc<PacketStats>,
    takion: mpsc::Sender<TakionCommand>,
    cancel: CancellationToken,
) {
    // chiaki-ng's congestion thread waits one full interval before its first
    // packet. Do the same: sending a congestion packet in the BIG -> BANG
    // window makes current PS5 firmware drop the stream.
    let mut ticker = interval(Duration::from_millis(200));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;

    let mut first = true;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                if first {
                    first = false;
                    tracing::info!("stream: sending congestion feedback");
                }
                let (received, lost) = stats.get(true);
                let _ = takion.try_send(TakionCommand::SendCongestion(CongestionPacket {
                    word_0: 0,
                    received: received as u16,
                    lost: lost as u16,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::big_chunk_plan;
    use super::encrypt_launch_spec;
    use crate::crypto::rpcrypt::RPCrypt;
    use crate::target::Target;

    #[test]
    fn launch_spec_uses_zero_keystream_xor() {
        // Vector generated with chiaki-ng's lib/src/rpcrypt.c: encrypt a zero
        // buffer at counter 0, then XOR the JSON. Direct CFB encryption
        // differs after the first 16-byte block.
        let nonce: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let morning: [u8; 16] = [
            0x8a, 0xfc, 0xf5, 0xf1, 0xdf, 0xf6, 0xdb, 0x7a, 0x82, 0xa5, 0x72, 0xcc, 0x8d, 0xca,
            0xc9, 0x94,
        ];
        let crypt = RPCrypt::init_auth(Target::Ps5_1, &nonce, &morning).unwrap();
        let mut plain = b"{\"a\":1,\"b\":\"hello\"}".to_vec();
        plain.push(0);
        let encrypted = encrypt_launch_spec(&crypt, &plain).unwrap();
        let hex: String = encrypted.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "da8101394caf7f1782c1f9ecf6104444462ada35");
    }

    #[test]
    fn big_fragmentation_matches_chiaki_ng() {
        // Measured PS5 MTU 1454 leaves 1404 usable bytes; our 74-character
        // session-id BIG is 1409 bytes, so chiaki-ng sends a 1378-byte first
        // chunk (flags 0) and a 31-byte continuation (flags 1).
        let plan = big_chunk_plan(1409, 1454);
        assert_eq!(plan, vec![(0, 1378, false), (1378, 31, true)]);
    }

    #[test]
    fn big_that_fits_is_one_chunk() {
        let plan = big_chunk_plan(1000, 1454);
        assert_eq!(plan, vec![(0, 1000, false)]);
    }

    #[test]
    fn big_chunks_cover_the_whole_buffer() {
        for total in [1405, 1409, 2500, 3000, 5000] {
            let plan = big_chunk_plan(total, 1050);
            let mut next = 0;
            for (offset, size, _) in &plan {
                assert_eq!(*offset, next);
                next += size;
            }
            assert_eq!(next, total);
            assert!(plan.len() > 1);
        }
    }
}
