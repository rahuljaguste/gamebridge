// SPDX-License-Identifier: AGPL-3.0-only
//! Shared media state between the event forwarder and the track bridges.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use bytes::Bytes;

use chiaki_core::media::audio::AudioFormat;

#[derive(Default)]
pub struct MediaShared {
    /// SPS/PPS Annex-B blob from the stream info / video format event.
    pub param_sets: Mutex<Vec<Bytes>>,
    pub audio_format: Mutex<Option<AudioFormat>>,
    pub first_idr_sent: AtomicBool,
}
