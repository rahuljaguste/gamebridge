// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Media assembly: FEC, video frames, and Opus packets.

pub mod audio;
pub mod fec;
pub mod frame_processor;
pub mod gf256;
pub mod ingest;
pub mod video;

pub use audio::{AudioFormat, AudioFrame};
pub use ingest::{IngestCommand, MediaEvent, VideoFrame, VideoProfileInfo};
