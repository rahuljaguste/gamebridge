// SPDX-License-Identifier: AGPL-3.0-only
//! Shared server state and browser-facing event types.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{Mutex, Notify, broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use webrtc::peer_connection::PeerConnection;

use chiaki_core::controller::ControllerState;
use chiaki_core::session::SessionCommand;
use chiaki_core::target::Codec;

use crate::config::ConfigStore;
use crate::demo::DemoSource;

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
    /// A non-fatal notice, such as the adaptive bitrate decision for the next
    /// session.
    Notice {
        message: String,
    },
    Quit {
        /// Identifies the session that ended. A browser that has already
        /// moved on to a newer session must ignore this event.
        session_id: String,
        reason: String,
        reason_string: Option<String>,
    },
    Stats {
        received: u64,
        lost: u64,
        /// Recent packet loss percentage after the startup warm-up.
        loss_pct: Option<f64>,
        /// Round-trip time to the console in milliseconds, when measured.
        latency_ms: Option<f64>,
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
    /// Video codec the peer was built for; the answer must accept it.
    pub codec: Codec,
    pub input: watch::Sender<ControllerState>,
    pub commands: mpsc::Sender<SessionCommand>,
    pub peer: Arc<dyn PeerConnection>,
    pub peer_connected: Arc<AtomicBool>,
    pub gathering: Arc<Notify>,
    pub stop_tx: Option<oneshot::Sender<()>>,
    pub started: Instant,
}

pub struct AppState {
    pub config: Arc<ConfigStore>,
    pub active: Mutex<Option<ActiveSession>>,
    /// Set while a start request is reserving the single session slot. This
    /// turns check-then-set into an atomic reservation: two rapid starts can no
    /// longer each establish a console session before one loses the race.
    starting: AtomicBool,
    /// Why the most recent session ended abnormally, kept so the browser can
    /// see the real reason if it answers after the session already died.
    pub last_session_error: Mutex<Option<String>>,
    pub events: broadcast::Sender<ServerEvent>,
    /// Recent events replayed to a WebSocket that connects after the session
    /// started. The console can ask for a PIN before the browser finishes
    /// gathering ICE candidates, so a live-only broadcast would lose it.
    replay: std::sync::Mutex<VecDeque<ServerEvent>>,
    /// Browsers currently attached to `/api/session/ws`. The session manager
    /// uses this to detect a closed tab.
    pub ws_clients: Arc<AtomicUsize>,
    pub demo: Option<DemoSource>,
    /// Cancelled once when the HTTP server begins a graceful shutdown, so
    /// upgraded WebSocket handlers can close their sockets instead of relying
    /// on the runtime being torn down underneath them.
    pub shutdown: CancellationToken,
}

const REPLAY_CAPACITY: usize = 32;

impl AppState {
    pub fn new(config: ConfigStore, demo: Option<DemoSource>) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            config: Arc::new(config),
            active: Mutex::new(None),
            starting: AtomicBool::new(false),
            last_session_error: Mutex::new(None),
            events,
            replay: std::sync::Mutex::new(VecDeque::new()),
            ws_clients: Arc::new(AtomicUsize::new(0)),
            demo,
            shutdown: CancellationToken::new(),
        })
    }

    /// Publish a session event to live WebSockets and to the replay buffer.
    /// The replay lock is held across `send` so a subscriber either sees an
    /// event live or in its snapshot, never both and never neither.
    pub fn publish(&self, event: ServerEvent) {
        // A panic while a previous writer held this lock must not turn every
        // later event into another panic; the buffer is only a replay cache.
        let mut replay = self.replay.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(&event, ServerEvent::Stats { .. }) {
            if replay.len() >= REPLAY_CAPACITY {
                replay.pop_front();
            }
            replay.push_back(event.clone());
        }
        let _ = self.events.send(event);
    }

    /// Subscribe and snapshot the replay buffer atomically.
    pub fn subscribe_with_replay(&self) -> (broadcast::Receiver<ServerEvent>, Vec<ServerEvent>) {
        let replay = self.replay.lock().unwrap_or_else(|e| e.into_inner());
        let receiver = self.events.subscribe();
        (receiver, replay.iter().cloned().collect())
    }

    pub fn clear_replay(&self) {
        self.replay
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn ws_client_connected(&self) {
        self.ws_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn ws_client_disconnected(&self) {
        self.ws_clients.fetch_sub(1, Ordering::Relaxed);
    }

    /// Reserve the session slot for a start request. Returns `None` when
    /// another start is already in flight; the returned guard releases the
    /// reservation on every early return.
    pub fn begin_start(self: &Arc<Self>) -> Option<StartGuard> {
        if self.starting.swap(true, Ordering::Acquire) {
            return None;
        }
        Some(StartGuard {
            state: self.clone(),
            armed: true,
        })
    }

    /// Reserve the single session slot and reset per-session state. The
    /// caller must `disarm` the guard once the session is installed.
    pub async fn reserve_session_start(self: &Arc<Self>) -> Option<StartGuard> {
        let guard = self.begin_start()?;
        if self.active.lock().await.is_some() {
            return None;
        }
        self.clear_replay();
        *self.last_session_error.lock().await = None;
        Some(guard)
    }
}

/// RAII reservation held from the first check through the final insertion of
/// the `ActiveSession`.
pub struct StartGuard {
    state: Arc<AppState>,
    armed: bool,
}

impl StartGuard {
    /// The session is installed in `active`; release the reservation without
    /// dropping the session.
    pub fn disarm(mut self) {
        self.armed = false;
        self.state.starting.store(false, Ordering::Release);
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.state.starting.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigStore;

    fn test_state() -> Arc<AppState> {
        let dir = std::env::temp_dir().join(format!("gamebridge-test-{}", Uuid::new_v4()));
        let store = ConfigStore::load(Some(dir)).expect("load empty config");
        AppState::new(store, None)
    }

    #[test]
    fn start_guard_excludes_concurrent_starts_and_releases() {
        let state = test_state();
        let first = state.begin_start().expect("first reservation");
        assert!(state.begin_start().is_none());
        drop(first);
        assert!(state.begin_start().is_some());
    }

    #[test]
    fn disarm_keeps_slot_available() {
        let state = test_state();
        state.begin_start().expect("reservation").disarm();
        assert!(state.begin_start().is_some());
    }

    #[test]
    fn replay_feeds_a_late_subscriber() {
        let state = test_state();
        state.publish(ServerEvent::Phase {
            phase: "waiting_pin".to_string(),
        });
        let (mut live, replay) = state.subscribe_with_replay();
        assert!(matches!(
            replay.as_slice(),
            [ServerEvent::Phase { phase }] if phase.as_str() == "waiting_pin"
        ));

        state.publish(ServerEvent::Connected);
        assert!(matches!(live.try_recv(), Ok(ServerEvent::Connected)));
    }

    #[test]
    fn stats_are_not_replayed() {
        let state = test_state();
        state.publish(ServerEvent::Stats {
            received: 1,
            lost: 0,
            loss_pct: None,
            latency_ms: None,
        });
        let (_, replay) = state.subscribe_with_replay();
        assert!(replay.is_empty());
    }
}
