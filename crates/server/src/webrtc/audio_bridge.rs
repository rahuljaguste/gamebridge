// SPDX-License-Identifier: AGPL-3.0-only
//! Forward console Opus packets to the WebRTC audio track.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use chiaki_core::media::audio::AudioFrame;

use crate::webrtc::TrackWriter;
use crate::webrtc::shared::MediaShared;

pub async fn run(
    mut frames: mpsc::Receiver<AudioFrame>,
    writer: TrackWriter,
    shared: Arc<MediaShared>,
    cancel: CancellationToken,
) {
    // Wait for stream info, then for the first video IDR so A/V starts together.
    let format = loop {
        if let Some(format) = *shared
            .audio_format
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            break format;
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    };
    if format.sample_rate != 48000 || format.channels == 0 || format.channels > 2 {
        tracing::error!(
            "unsupported audio format: {} Hz, {} channels",
            format.sample_rate,
            format.channels
        );
        return;
    }
    while !shared.first_idr_sent.load(Ordering::Relaxed) {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }

    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                frame
            }
        };
        let result = tokio::select! {
            _ = cancel.cancelled() => break,
            result = writer.write_sample(frame.data, frame.duration) => result,
        };
        if let Err(e) = result {
            tracing::warn!("audio write failed: {e:#}");
        }
    }
}
