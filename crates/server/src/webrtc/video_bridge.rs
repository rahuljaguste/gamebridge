// SPDX-License-Identifier: AGPL-3.0-only
//! Forward complete H.264 access units to the WebRTC video track.
//!
//! The bridge never sends a partial frame. It starts at an IDR, detects frame
//! index gaps, and asks the console for a fresh keyframe when needed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use chiaki_core::media::ingest::VideoFrame;
use chiaki_core::session::SessionCommand;

use crate::webrtc::TrackWriter;
use crate::webrtc::shared::MediaShared;

const CACHED_IDR_MAX_AGE: Duration = Duration::from_millis(150);

pub async fn run(
    mut frames: mpsc::Receiver<VideoFrame>,
    writer: TrackWriter,
    peer_connected: Arc<AtomicBool>,
    shared: Arc<MediaShared>,
    commands: mpsc::Sender<SessionCommand>,
    frame_duration: Duration,
    cancel: CancellationToken,
) {
    let mut awaiting_idr = true;
    let mut last_frame: Option<u16> = None;
    let mut cached_idr: Option<(VideoFrame, Instant)> = None;
    let mut last_keyframe_request: Option<Instant> = None;

    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                frame
            }
        };

        if !peer_connected.load(Ordering::Relaxed) {
            if frame.is_idr {
                cached_idr = Some((frame, Instant::now()));
            }
            continue;
        }

        // A freshly cached IDR can bridge the gap between media start and the
        // peer becoming ready. Only use it while it is still recent.
        if awaiting_idr
            && !frame.is_idr
            && let Some((cached, at)) = cached_idr.take()
            && at.elapsed() < CACHED_IDR_MAX_AGE
        {
            let index = cached.frame_index;
            if send_frame(&writer, &shared, cached, frame_duration).await {
                last_frame = Some(index);
                awaiting_idr = false;
            }
        }

        if awaiting_idr {
            if frame.is_idr {
                awaiting_idr = false;
            } else {
                request_keyframe(&commands, &mut last_keyframe_request);
                continue;
            }
        }

        if !frame.is_idr {
            let Some(last) = last_frame else {
                awaiting_idr = true;
                request_keyframe(&commands, &mut last_keyframe_request);
                continue;
            };
            if frame.frame_index != last.wrapping_add(1) {
                tracing::warn!(
                    "video frame gap {} -> {}; waiting for IDR",
                    last,
                    frame.frame_index
                );
                awaiting_idr = true;
                request_keyframe(&commands, &mut last_keyframe_request);
                continue;
            }
        }

        let index = frame.frame_index;
        if send_frame(&writer, &shared, frame, frame_duration).await {
            last_frame = Some(index);
            if awaiting_idr {
                awaiting_idr = false;
            }
        } else {
            awaiting_idr = true;
            request_keyframe(&commands, &mut last_keyframe_request);
        }
        cached_idr = None;
    }
}

async fn send_frame(
    writer: &TrackWriter,
    shared: &MediaShared,
    frame: VideoFrame,
    frame_duration: Duration,
) -> bool {
    let mut data = frame.data;
    if frame.is_idr && !frame.has_param_sets {
        let param_sets = shared.param_sets.lock().unwrap().clone();
        if !param_sets.is_empty() {
            let mut combined =
                Vec::with_capacity(data.len() + param_sets.iter().map(|b| b.len()).sum::<usize>());
            for params in &param_sets {
                combined.extend_from_slice(params);
            }
            combined.extend_from_slice(&data);
            data = Bytes::from(combined);
        }
    }
    // RTP timestamps must advance at the stream's nominal frame rate, not by
    // however long the frame took to arrive. Arrival-derived durations make
    // the browser's render clock jitter whenever a frame is late, and the
    // jitter accumulates into visible stutter. The console encodes at the
    // requested `maxFps`, so a fixed 1/fps step is the correct timeline.
    if let Err(e) = writer.write_sample(data, frame_duration).await {
        tracing::warn!("video write failed: {e:#}");
        return false;
    }
    if frame.is_idr {
        shared.first_idr_sent.store(true, Ordering::Relaxed);
    }
    true
}

fn request_keyframe(commands: &mpsc::Sender<SessionCommand>, last_request: &mut Option<Instant>) {
    if last_request
        .map(|t| t.elapsed() < Duration::from_secs(1))
        .unwrap_or(false)
    {
        return;
    }
    *last_request = Some(Instant::now());
    let _ = commands.try_send(SessionCommand::RequestKeyframe);
}
