// SPDX-License-Identifier: AGPL-3.0-only
//! AV ingest: turns decrypted Takion AV packets into complete video frames
//! and Opus packets, with FEC recovery and keyframe-gap reporting.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::media::audio::{AudioFormat, AudioFrame, AudioReceiver};
use crate::media::frame_processor::{FlushResult, FrameProcessor};
use crate::media::video::scan_annex_b;
use crate::packet_stats::PacketStats;
use crate::takion::packet::AvPacket;
use crate::takion::{TakionCommand, seq16_gt, seq16_lt};

#[derive(Debug, Clone)]
pub struct VideoProfileInfo {
    pub width: u32,
    pub height: u32,
    pub header: Bytes,
}

#[derive(Debug)]
pub enum IngestCommand {
    StreamInfo {
        profiles: Vec<VideoProfileInfo>,
        audio_header: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub enum MediaEvent {
    /// Sent once stream info is known; the bridge can derive H.264 fmtp.
    StreamInfo {
        profiles: Vec<VideoProfileInfo>,
        audio: AudioFormat,
    },
    /// Sent whenever the adaptive profile changes (including the first).
    VideoFormat {
        profile_index: u8,
        width: u32,
        height: u32,
        header: Bytes,
    },
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub data: Bytes,
    pub frame_index: u16,
    pub profile_index: u8,
    pub width: u32,
    pub height: u32,
    pub is_idr: bool,
    pub has_param_sets: bool,
}

struct VideoReceiver {
    profiles: Vec<VideoProfileInfo>,
    profile_cur: Option<usize>,
    frame_index_cur: Option<u16>,
    frame_index_prev: Option<u16>,
    frame_index_prev_complete: u16,
    processor: FrameProcessor,
}

impl VideoReceiver {
    fn new() -> Self {
        Self {
            profiles: Vec::new(),
            profile_cur: None,
            frame_index_cur: None,
            frame_index_prev: None,
            frame_index_prev_complete: 0,
            processor: FrameProcessor::new(),
        }
    }

    async fn on_packet(
        &mut self,
        packet: &AvPacket,
        data: Bytes,
        video_tx: &mpsc::Sender<VideoFrame>,
        events: &mpsc::UnboundedSender<MediaEvent>,
        takion: &mpsc::Sender<TakionCommand>,
        stats: &PacketStats,
    ) {
        let frame_index = packet.frame_index;
        if let Some(cur) = self.frame_index_cur
            && seq16_lt(frame_index, cur)
        {
            tracing::warn!("video receiver got an old frame packet");
            return;
        }

        let profile = packet.adaptive_stream_index as usize;
        if self.profile_cur != Some(profile) {
            if profile >= self.profiles.len() {
                tracing::error!(
                    "video packet has invalid adaptive stream index {profile} >= {}",
                    self.profiles.len()
                );
                return;
            }
            self.profile_cur = Some(profile);
            let info = &self.profiles[profile];
            let _ = events.send(MediaEvent::VideoFormat {
                profile_index: profile as u8,
                width: info.width,
                height: info.height,
                header: info.header.clone(),
            });
        }

        let next =
            self.frame_index_cur.is_none() || seq16_gt(frame_index, self.frame_index_cur.unwrap());
        if next {
            self.processor.report_packet_stats(stats);
            if let Some(cur) = self.frame_index_cur
                && self.frame_index_prev != Some(cur)
            {
                self.flush(true, video_tx, takion).await;
            }
            let next_expected = self.frame_index_prev_complete.wrapping_add(1);
            if seq16_gt(frame_index, next_expected)
                && !(frame_index == 1 && self.frame_index_cur.is_none())
            {
                tracing::warn!(
                    "detected missing/corrupt frames from {next_expected} to {frame_index}"
                );
                let _ = takion.try_send(TakionCommand::SendCorruptFrame {
                    start: next_expected,
                    end: frame_index.wrapping_sub(1),
                });
            }
            self.frame_index_cur = Some(frame_index);
            if self.processor.alloc_frame(packet, &data).is_err() {
                tracing::warn!("failed to allocate frame {frame_index}");
                return;
            }
        }

        if self.processor.put_unit(packet, &data).is_err() {
            tracing::warn!("failed to put unit for frame {frame_index}");
            return;
        }
        if self.frame_index_cur != self.frame_index_prev && self.processor.flush_possible() {
            self.flush(false, video_tx, takion).await;
        }
    }

    async fn flush(
        &mut self,
        _forced: bool,
        video_tx: &mpsc::Sender<VideoFrame>,
        takion: &mpsc::Sender<TakionCommand>,
    ) {
        let (result, data) = self.processor.flush();
        match result {
            FlushResult::Failed => return,
            FlushResult::FecFailed => {
                if let Some(cur) = self.frame_index_cur {
                    tracing::warn!("dropping frame {cur}: FEC failed");
                    let _ = takion.try_send(TakionCommand::SendCorruptFrame {
                        start: cur,
                        end: cur,
                    });
                }
                self.frame_index_prev = self.frame_index_cur;
                return;
            }
            FlushResult::Success | FlushResult::FecSuccess => {}
        }
        let Some(cur) = self.frame_index_cur else {
            return;
        };
        let profile = self.profile_cur.unwrap_or(0);
        let info = self.profiles.get(profile);
        let scan = scan_annex_b(&data);
        let frame = VideoFrame {
            data: Bytes::from(data),
            frame_index: cur,
            profile_index: profile as u8,
            width: info.map(|p| p.width).unwrap_or(0),
            height: info.map(|p| p.height).unwrap_or(0),
            is_idr: scan.is_idr,
            has_param_sets: scan.has_sps || scan.has_pps,
        };
        if video_tx.try_send(frame).is_err() {
            tracing::warn!("video queue full; dropping frame {cur}");
            let _ = takion.try_send(TakionCommand::SendCorruptFrame {
                start: cur,
                end: cur,
            });
        }
        self.frame_index_prev = Some(cur);
        self.frame_index_prev_complete = cur;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut commands: mpsc::Receiver<IngestCommand>,
    mut av: mpsc::Receiver<(AvPacket, Bytes)>,
    video_tx: mpsc::Sender<VideoFrame>,
    audio_tx: mpsc::Sender<AudioFrame>,
    events: mpsc::UnboundedSender<MediaEvent>,
    takion: mpsc::Sender<TakionCommand>,
    stats: Arc<PacketStats>,
    cancel: CancellationToken,
) {
    let mut video = VideoReceiver::new();
    let mut audio: Option<AudioReceiver> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            command = commands.recv() => {
                match command {
                    Some(IngestCommand::StreamInfo { profiles, audio_header }) => {
                        video.profiles = profiles.clone();
                        match AudioFormat::parse(&audio_header) {
                            Ok(format) => {
                                audio = Some(AudioReceiver::new(format));
                                let _ = events.send(MediaEvent::StreamInfo { profiles, audio: format });
                            }
                            Err(e) => tracing::error!("invalid audio header in stream info: {e}"),
                        }
                    }
                    None => break,
                }
            }
            item = av.recv() => {
                let Some((packet, data)) = item else { break };
                if packet.is_video {
                    video.on_packet(&packet, data, &video_tx, &events, &takion, &stats).await;
                } else if let Some(audio) = audio.as_mut() {
                    for frame in audio.push(&packet, &data, Some(&stats)) {
                        if frame.is_haptics {
                            continue;
                        }
                        if audio_tx.try_send(frame).is_err() {
                            tracing::debug!("audio queue full; dropping packet");
                        }
                    }
                }
            }
        }
    }
}
