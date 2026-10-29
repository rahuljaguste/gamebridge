// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! WebRTC peer setup and media track writers.

pub mod audio_bridge;
pub mod peer;
pub mod shared;
pub mod video_bridge;

pub use peer::{PeerHandles, TrackWriter, create_peer};
pub use shared::MediaShared;
