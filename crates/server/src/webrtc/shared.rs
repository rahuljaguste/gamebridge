// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Shared media state between the event forwarder and the track bridges.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use bytes::Bytes;

use chiaki_core::media::audio::AudioFormat;

#[derive(Default)]
pub struct MediaShared {
    /// Parameter-set Annex-B blob from the stream info / video format event:
    /// SPS/PPS for H.264, VPS/SPS/PPS for H.265.
    pub param_sets: Mutex<Vec<Bytes>>,
    pub audio_format: Mutex<Option<AudioFormat>>,
    /// Latest console round-trip time, surfaced in the periodic stats event.
    pub latency_ms: Mutex<Option<f64>>,
    pub first_idr_sent: AtomicBool,
    /// Frames waiting for the WebRTC video writer, sampled by the bridge.
    pub video_queue_depth: AtomicUsize,
    /// Highest depth since the stats task last sampled.
    pub video_queue_peak: AtomicUsize,
}

impl MediaShared {
    /// Prepend the cached parameter sets to an access unit that lacks them.
    pub fn with_param_sets(&self, data: Bytes) -> Bytes {
        let param_sets = self
            .param_sets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if param_sets.is_empty() {
            return data;
        }
        let mut combined =
            Vec::with_capacity(data.len() + param_sets.iter().map(Bytes::len).sum::<usize>());
        for params in param_sets.iter() {
            combined.extend_from_slice(params);
        }
        combined.extend_from_slice(&data);
        Bytes::from(combined)
    }
}
