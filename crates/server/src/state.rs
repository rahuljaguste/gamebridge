// SPDX-License-Identifier: AGPL-3.0-only
//! Shared server state and browser-facing event types.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use uuid::Uuid;
use webrtc::peer_connection::PeerConnection;

use chiaki_core::controller::ControllerState;
use chiaki_core::session::SessionCommand;

use crate::config::ConfigStore;

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioView {
    pub channels: u8,
    pub bits: u8,
    pub sample_rate: u32,
    pub frame_size: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Phase {
        phase: String,
    },
    Connected,
    PinRequest {
        incorrect: bool,
    },
    StreamInfo {
        profiles: Vec<ProfileView>,
        audio: AudioView,
    },
    VideoFormat {
        profile_index: u8,
        width: u32,
        height: u32,
    },
    Rumble {
        unknown: u8,
        left: u8,
        right: u8,
    },
    TriggerEffects {
        type_left: u8,
        type_right: u8,
        left: [u8; 10],
        right: [u8; 10],
    },
    RemoteDisconnect {
        reason: String,
    },
    Error {
        message: String,
    },
    Quit {
        reason: String,
        reason_string: Option<String>,
    },
    Stats {
        received: u64,
        lost: u64,
    },
    PeerState {
        state: String,
    },
    SessionStarted {
        session_id: String,
    },
}

pub struct ActiveSession {
    pub id: Uuid,
    pub host_id: String,
    pub input: watch::Sender<ControllerState>,
    pub commands: mpsc::Sender<SessionCommand>,
    pub peer: Arc<dyn PeerConnection>,
    pub peer_connected: Arc<AtomicBool>,
    pub stop_tx: Option<oneshot::Sender<()>>,
    pub started: Instant,
}

pub struct AppState {
    pub config: Arc<ConfigStore>,
    pub active: Mutex<Option<ActiveSession>>,
    /// Why the most recent session ended abnormally, kept so the browser can
    /// see the real reason if it answers after the session already died.
    pub last_session_error: Mutex<Option<String>>,
    pub events: broadcast::Sender<ServerEvent>,
    pub demo_file: Option<std::path::PathBuf>,
}

impl AppState {
    pub fn new(config: ConfigStore, demo_file: Option<std::path::PathBuf>) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            config: Arc::new(config),
            active: Mutex::new(None),
            last_session_error: Mutex::new(None),
            events,
            demo_file,
        })
    }

    pub fn broadcast(&self, event: ServerEvent) {
        let _ = self.events.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }
}
