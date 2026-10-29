// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Create the server-side peer connection, video/Opus tracks and offer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_H264, MIME_TYPE_HEVC, MIME_TYPE_OPUS, MediaEngine,
};
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use tokio::sync::{Notify, broadcast};
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};
use webrtc::rtp_transceiver::RtpSender;
use webrtc::runtime::{Runtime, default_runtime};

use chiaki_core::target::Codec;

use crate::state::ServerEvent;

pub const H264_PAYLOAD_TYPE: u8 = 102;
pub const H265_PAYLOAD_TYPE: u8 = 103;
pub const OPUS_PAYLOAD_TYPE: u8 = 111;
const H264_FMTP: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";
/// Main profile. `tx-mode` is omitted because RFC 7798 defaults to SRST,
/// which Chrome expects; an explicit `tx-mode=SRS` made Chrome reject the
/// video m-line (port 0) even though it advertises H.265.
const H265_FMTP: &str = "profile-id=1;tier-flag=0;level-asymmetry-allowed=1";
/// Main10 profile for HDR.
const H265_HDR_FMTP: &str = "profile-id=2;tier-flag=0;level-asymmetry-allowed=1";
const OPUS_FMTP: &str = "minptime=10;useinbandfec=1";

fn runtime() -> Result<Arc<dyn Runtime>> {
    static RUNTIME: OnceLock<Arc<dyn Runtime>> = OnceLock::new();
    Ok(RUNTIME
        .get_or_init(|| default_runtime().expect("runtime-tokio feature is enabled"))
        .clone())
}

/// ICE gathers on loopback plus every non-loopback IPv4 interface. The HTTP UI
/// is loopback-only, so the offer SDP (and these candidates) cannot be obtained
/// from another host today. Advertising LAN candidates is deliberate: the SDP
/// can be handed to a browser on another machine if the UI is exposed later.
/// Revisit this if non-local access is ever enabled without authentication.
fn udp_addrs() -> Vec<String> {
    let mut out = vec!["127.0.0.1:0".to_string()];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                if v4.ip.is_loopback() || v4.ip.is_link_local() {
                    continue;
                }
                let addr = format!("{}:0", v4.ip);
                if !out.contains(&addr) {
                    out.push(addr);
                }
            }
        }
    }
    out
}

#[derive(Clone)]
pub struct TrackWriter {
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    payload_type: u8,
}

impl TrackWriter {
    pub fn payload_type(&self) -> u8 {
        self.payload_type
    }

    pub async fn write_sample(&self, data: bytes::Bytes, duration: Duration) -> Result<()> {
        let sample = Sample {
            data,
            duration,
            ..Sample::new(Instant::now())
        };
        self.track
            .sample_writer(self.ssrc, self.payload_type())
            .write_sample(&sample)
            .await
            .context("write_sample failed")
    }
}

#[derive(Clone)]
pub struct PeerHandles {
    pub peer: Arc<dyn PeerConnection>,
    pub video: TrackWriter,
    pub audio: TrackWriter,
    pub connected: Arc<AtomicBool>,
    pub gathering: Arc<Notify>,
    pub offer_sdp: String,
}

struct Handler {
    gathering: Arc<Notify>,
    connected: Arc<AtomicBool>,
    events: broadcast::Sender<ServerEvent>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathering.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let state_str = format!("{state:?}").to_lowercase();
        let _ = self
            .events
            .send(ServerEvent::PeerState { state: state_str });
        match state {
            RTCPeerConnectionState::Connected => {
                self.connected.store(true, Ordering::Relaxed);
            }
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
                self.connected.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

async fn payload_type(sender: &Arc<dyn RtpSender>, fallback: u8) -> u8 {
    match sender.get_parameters().await {
        Ok(params) => params
            .rtp_parameters
            .codecs
            .first()
            .map(|c| c.payload_type)
            .unwrap_or(fallback),
        Err(_) => fallback,
    }
}

/// Build the peer, add tracks, and create a non-trickle offer. Only the
/// selected video codec is advertised, so the browser's answer either accepts
/// it or the caller can fail the start instead of forwarding an undecodable
/// track.
pub async fn create_peer(
    events: broadcast::Sender<ServerEvent>,
    codec: Codec,
) -> Result<PeerHandles> {
    let mut media_engine = MediaEngine::default();
    let (mime_type, video_payload_type, fmtp) = match codec {
        Codec::H264 => (MIME_TYPE_H264, H264_PAYLOAD_TYPE, H264_FMTP),
        Codec::H265 => (MIME_TYPE_HEVC, H265_PAYLOAD_TYPE, H265_FMTP),
        Codec::H265Hdr => (MIME_TYPE_HEVC, H265_PAYLOAD_TYPE, H265_HDR_FMTP),
    };

    let video_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: mime_type.to_string(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: fmtp.to_string(),
            rtcp_feedback: vec![],
        },
        payload_type: video_payload_type,
    };
    let audio_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_string(),
            clock_rate: 48000,
            channels: 2,
            sdp_fmtp_line: OPUS_FMTP.to_string(),
            rtcp_feedback: vec![],
        },
        payload_type: OPUS_PAYLOAD_TYPE,
    };
    media_engine
        .register_codec(video_codec.clone(), RtpCodecKind::Video)
        .with_context(|| format!("register {:?} codec", codec))?;
    media_engine
        .register_codec(audio_codec.clone(), RtpCodecKind::Audio)
        .context("register Opus codec")?;

    let registry =
        register_default_interceptors(rtc::interceptor::Registry::new(), &mut media_engine)?;
    let config = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![])
        .build();

    let gathering = Arc::new(Notify::new());
    let connected = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(Handler {
        gathering: gathering.clone(),
        connected: connected.clone(),
        events: events.clone(),
    });

    let peer = PeerConnectionBuilder::<String>::new()
        .with_configuration(config)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(runtime()?)
        .with_udp_addrs(udp_addrs())
        .build()
        .await
        .context("build peer connection")?;
    let peer: Arc<dyn PeerConnection> = Arc::new(peer);

    // Everything after `build()` can still fail. Keep the peer handle alive
    // and close it on any error; otherwise its UDP sockets remain bound on
    // every advertised interface.
    let handle_result: Result<PeerHandles> = async {
        let stream_id = "gamebridge-stream".to_string();
        let video_ssrc = rand::random::<u32>();
        let video_track = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    stream_id.clone(),
                    "gamebridge-video".to_string(),
                    "GameBridge Video".to_string(),
                    RtpCodecKind::Video,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters {
                            ssrc: Some(video_ssrc),
                            ..Default::default()
                        },
                        codec: video_codec.rtp_codec.clone(),
                        ..Default::default()
                    }],
                ),
            )
            .context("create video track")?,
        );
        let video_sender = peer
            .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal>)
            .await
            .context("add video track")?;

        let audio_ssrc = rand::random::<u32>();
        let audio_track = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    stream_id,
                    "gamebridge-audio".to_string(),
                    "GameBridge Audio".to_string(),
                    RtpCodecKind::Audio,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters {
                            ssrc: Some(audio_ssrc),
                            ..Default::default()
                        },
                        codec: audio_codec.rtp_codec.clone(),
                        ..Default::default()
                    }],
                ),
            )
            .context("create audio track")?,
        );
        let audio_sender = peer
            .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal>)
            .await
            .context("add audio track")?;

        let video_pt = payload_type(&video_sender, video_payload_type).await;
        let audio_pt = payload_type(&audio_sender, OPUS_PAYLOAD_TYPE).await;

        let offer = peer.create_offer(None).await.context("create offer")?;
        peer.set_local_description(offer)
            .await
            .context("set local description")?;
        let _ = tokio::time::timeout(Duration::from_secs(3), gathering.notified()).await;
        let local = peer
            .local_description()
            .await
            .context("missing local description")?;

        Ok(PeerHandles {
            peer: Arc::clone(&peer),
            video: TrackWriter {
                track: video_track,
                ssrc: video_ssrc,
                payload_type: video_pt,
            },
            audio: TrackWriter {
                track: audio_track,
                ssrc: audio_ssrc,
                payload_type: audio_pt,
            },
            connected,
            gathering: Arc::clone(&gathering),
            offer_sdp: local.sdp,
        })
    }
    .await;
    if handle_result.is_err() {
        let _ = peer.close().await;
    }
    handle_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn h265_offer_advertises_hevc() {
        let (events, _) = broadcast::channel(1);
        let peers = create_peer(events, Codec::H265).await.expect("H.265 peer");
        assert!(peers.offer_sdp.contains("H265/90000"));
        assert!(peers.offer_sdp.contains("profile-id=1"));
        assert!(!peers.offer_sdp.contains("tx-mode=SRS"));
        assert!(!peers.offer_sdp.contains("H264/90000"));
        let _ = peers.peer.close().await;

        let (events, _) = broadcast::channel(1);
        let peers = create_peer(events, Codec::H265Hdr)
            .await
            .expect("H.265 HDR peer");
        assert!(peers.offer_sdp.contains("H265/90000"));
        assert!(peers.offer_sdp.contains("profile-id=2"));
        assert!(!peers.offer_sdp.contains("tx-mode=SRS"));
        let _ = peers.peer.close().await;
    }
}
