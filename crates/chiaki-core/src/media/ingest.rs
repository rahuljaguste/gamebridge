// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! AV ingest: turns decrypted Takion AV packets into complete video frames
//! and Opus packets, with FEC recovery and keyframe-gap reporting.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::media::audio::{AudioFormat, AudioFrame, AudioReceiver};
use crate::media::frame_processor::{FlushResult, FrameProcessor};
use crate::media::video::{NalScan, scan_annex_b};
use crate::packet_stats::PacketStats;
use crate::takion::packet::AvPacket;
use crate::takion::{TakionCommand, seq16_gt, seq16_lt};
use crate::target::Codec;

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

/// A frame only carries a usable parameter-set set when every set the decoder
/// needs is present. Incomplete in-band sets are paired with the cached
/// stream-info blob by the WebRTC bridge.
fn has_complete_param_sets(scan: NalScan, codec: Codec) -> bool {
    if codec.is_h265() {
        scan.has_vps && scan.has_sps && scan.has_pps
    } else {
        scan.has_sps && scan.has_pps
    }
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub data: Bytes,
    pub frame_index: u16,
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
    codec: Codec,
}

impl VideoReceiver {
    fn new(codec: Codec) -> Self {
        Self {
            profiles: Vec::new(),
            profile_cur: None,
            frame_index_cur: None,
            frame_index_prev: None,
            frame_index_prev_complete: 0,
            processor: FrameProcessor::new(),
            codec,
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

        let next = self
            .frame_index_cur
            .is_none_or(|cur| seq16_gt(frame_index, cur));
        if next {
            self.processor.report_packet_stats(stats);
            if let Some(cur) = self.frame_index_cur
                && self.frame_index_prev != Some(cur)
            {
                self.flush(video_tx, takion);
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
            if self.processor.alloc_frame(packet, &data).is_err() {
                // Leave `frame_index_cur` unchanged. Advancing it here would
                // make later packets for this frame look like duplicates and
                // could assemble the next frame against the bad geometry.
                tracing::warn!("failed to allocate frame {frame_index}");
                return;
            }
            self.frame_index_cur = Some(frame_index);
        }

        if self.processor.is_flushed() {
            // The frame flushed as soon as its source units arrived; FEC
            // units and retransmissions can legitimately trail behind. They
            // must not alter the frame, but they do count as received for
            // packet stats and the console's congestion feedback.
            self.processor.note_late_unit(packet);
            return;
        }
        if let Err(error) = self.processor.put_unit(packet, &data) {
            tracing::warn!(
                frame = frame_index,
                unit = packet.unit_index,
                total = packet.units_in_frame_total,
                fec = packet.units_in_frame_fec,
                data_len = data.len(),
                %error,
                "failed to put video unit"
            );
            return;
        }
        if self.frame_index_cur != self.frame_index_prev && self.processor.flush_possible() {
            self.flush(video_tx, takion);
        }
    }

    fn flush(&mut self, video_tx: &mpsc::Sender<VideoFrame>, takion: &mpsc::Sender<TakionCommand>) {
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
        let scan = scan_annex_b(&data, self.codec);
        let frame = VideoFrame {
            data: Bytes::from(data),
            frame_index: cur,
            is_idr: scan.is_idr,
            has_param_sets: has_complete_param_sets(scan, self.codec),
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
    codec: Codec,
    cancel: CancellationToken,
) {
    let mut video = VideoReceiver::new(codec);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::av_packet as packet;

    #[test]
    fn h265_requires_vps_sps_and_pps_together() {
        let complete = NalScan {
            is_idr: true,
            has_vps: true,
            has_sps: true,
            has_pps: true,
        };
        assert!(has_complete_param_sets(complete, Codec::H265));
        assert!(has_complete_param_sets(complete, Codec::H265Hdr));

        for incomplete in [
            NalScan {
                has_vps: false,
                ..complete
            },
            NalScan {
                has_sps: false,
                ..complete
            },
            NalScan {
                has_pps: false,
                ..complete
            },
        ] {
            assert!(!has_complete_param_sets(incomplete, Codec::H265));
        }
    }

    #[test]
    fn h264_requires_sps_and_pps_but_not_vps() {
        let complete = NalScan {
            is_idr: true,
            has_vps: false,
            has_sps: true,
            has_pps: true,
        };
        assert!(has_complete_param_sets(complete, Codec::H264));
        assert!(!has_complete_param_sets(
            NalScan {
                has_sps: false,
                ..complete
            },
            Codec::H264
        ));
        assert!(!has_complete_param_sets(
            NalScan {
                has_pps: false,
                ..complete
            },
            Codec::H264
        ));
    }

    #[tokio::test]
    async fn failed_allocation_does_not_advance_the_frame_index() {
        let mut receiver = VideoReceiver::new(Codec::H264);
        receiver.profiles.push(VideoProfileInfo {
            width: 1920,
            height: 1080,
            header: Bytes::new(),
        });
        let (video_tx, _video_rx) = mpsc::channel(4);
        let (events, _event_rx) = mpsc::unbounded_channel();
        let (takion, _takion_rx) = mpsc::channel(4);
        let stats = PacketStats::new();

        let bad = packet(5, 0, 1, 2, 4);
        receiver
            .on_packet(
                &bad,
                Bytes::from_static(&[0, 0, 0, 0]),
                &video_tx,
                &events,
                &takion,
                &stats,
            )
            .await;
        assert_eq!(receiver.frame_index_cur, None);

        let good = packet(5, 0, 3, 1, 5);
        receiver
            .on_packet(
                &good,
                Bytes::from_static(&[0, 3, 0xaa, 0xbb, 0xcc]),
                &video_tx,
                &events,
                &takion,
                &stats,
            )
            .await;
        assert_eq!(receiver.frame_index_cur, Some(5));
    }
}
