// SPDX-License-Identifier: AGPL-3.0-only
//! Cross-session adaptive bitrate.
//!
//! The launch spec fixes the console encoder's bitrate for the whole session;
//! neither Chiaki's protocol nor the console exposes a mid-session retarget
//! command. GameBridge therefore adapts the *next* session's starting bitrate from
//! the packet loss measured during this one.

use std::time::{Duration, Instant};

use crate::config::{AdaptiveRecord, ConfigStore};

/// Never step below this unless the user's own starting bitrate is lower.
pub const MIN_KBPS: u32 = 3000;
/// Sustained loss at or above this is treated as a bad session.
pub const BAD_LOSS_PCT: f64 = 1.5;
/// A one-second window at or above this is a burst, even if the session
/// average stays low. Bursty Wi-Fi loss is what causes visible artifacts and
/// dropped frames long before the average crosses `BAD_LOSS_PCT`.
pub const BURST_LOSS_PCT: f64 = 5.0;
/// At or below this, a session counts toward stepping back up.
pub const GOOD_LOSS_PCT: f64 = 0.3;
/// A clean session must also avoid bursts.
pub const GOOD_PEAK_LOSS_PCT: f64 = 1.0;
/// Two clean sessions are required before stepping up one rung.
pub const CLEAN_SESSIONS_TO_RECOVER: u32 = 2;
/// Ignore the first seconds after `Connected`; startup always loses packets.
pub const WARMUP: Duration = Duration::from_secs(5);
/// A session must have run this long and carried this many packets before it
/// can change the adaptive state.
pub const MIN_SESSION: Duration = Duration::from_secs(15);
pub const MIN_PACKETS: u64 = 1000;

const LADDER: [u32; 8] = [3000, 5000, 7000, 10000, 15000, 20000, 25000, 30000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveState {
    pub base_kbps: u32,
    pub current_kbps: u32,
    pub clean_sessions: u32,
}

impl AdaptiveState {
    pub fn new(base_kbps: u32) -> Self {
        Self {
            base_kbps,
            current_kbps: base_kbps,
            clean_sessions: 0,
        }
    }
}

/// The starting point for one session, carried into the manager so the result
/// can be recorded when the session ends.
#[derive(Debug, Clone)]
pub struct AdaptiveContext {
    pub host_id: String,
    pub state: AdaptiveState,
}

/// Resolve the starting bitrate for a host. A stored step is reused only when
/// the user's requested/base bitrate has not changed. The bool reports whether
/// the start was stepped down from the base.
pub fn resolve(config: &ConfigStore, host_id: &str, base_kbps: u32) -> (AdaptiveState, bool) {
    let floor = base_kbps.min(MIN_KBPS);
    if let Some(record) = config.adaptive(host_id)
        && record.base_kbps == base_kbps
        && record.current_kbps >= floor
        && record.current_kbps <= base_kbps
    {
        let state = AdaptiveState {
            base_kbps,
            current_kbps: record.current_kbps,
            clean_sessions: record.clean_sessions,
        };
        return (state, state.current_kbps < base_kbps);
    }
    (AdaptiveState::new(base_kbps), false)
}

/// Update and persist the adaptive state after a session. Returns a
/// user-facing message when the next session will start at a new bitrate.
pub fn record(
    config: &ConfigStore,
    context: &AdaptiveContext,
    loss_pct: f64,
    peak_loss_pct: f64,
    packets: u64,
    elapsed: Duration,
) -> Option<String> {
    if packets < MIN_PACKETS || elapsed < MIN_SESSION {
        return None;
    }
    let mut state = context.state;
    let mut changed = false;
    let mut message = None;
    let burst = peak_loss_pct >= BURST_LOSS_PCT;

    if loss_pct >= BAD_LOSS_PCT || burst {
        state.clean_sessions = 0;
        if let Some(next) = step_down(state.current_kbps, state.base_kbps) {
            state.current_kbps = next;
            changed = true;
            let detail = if loss_pct >= BAD_LOSS_PCT {
                format!("averaged {loss_pct:.1}%")
            } else {
                format!("peaked at {peak_loss_pct:.1}%")
            };
            message = Some(format!(
                "Packet loss {detail}. The next session will start at {} (adaptive bitrate).",
                mbps(next)
            ));
        }
    } else if loss_pct <= GOOD_LOSS_PCT && peak_loss_pct <= GOOD_PEAK_LOSS_PCT {
        if state.current_kbps < state.base_kbps {
            state.clean_sessions = state.clean_sessions.saturating_add(1);
            changed = true;
            if state.clean_sessions >= CLEAN_SESSIONS_TO_RECOVER
                && let Some(next) = step_up(state.current_kbps, state.base_kbps)
            {
                state.current_kbps = next;
                state.clean_sessions = 0;
                message = Some(format!(
                    "The last sessions were clean. The next session will start at {} (adaptive bitrate).",
                    mbps(next)
                ));
            }
        }
    } else if state.clean_sessions != 0 {
        state.clean_sessions = 0;
        changed = true;
    }

    if changed {
        let record = AdaptiveRecord {
            host_id: context.host_id.clone(),
            base_kbps: state.base_kbps,
            current_kbps: state.current_kbps,
            clean_sessions: state.clean_sessions,
        };
        if let Err(error) = config.set_adaptive(record) {
            tracing::warn!("could not persist adaptive bitrate state: {error:#}");
        }
    }
    message
}

fn mbps(kbps: u32) -> String {
    let value = kbps as f64 / 1000.0;
    if (value - value.round()).abs() < f64::EPSILON {
        format!("{value:.0} Mbps")
    } else {
        format!("{value:.1} Mbps")
    }
}

/// Next rung below `current`, never below the floor derived from the user's
/// chosen base bitrate.
fn step_down(current: u32, base: u32) -> Option<u32> {
    let floor = base.min(MIN_KBPS);
    if current <= floor {
        return None;
    }
    LADDER
        .iter()
        .rev()
        .find(|&&value| value < current && value >= floor)
        .copied()
        .or(Some(floor))
        .filter(|value| *value < current)
}

/// Next rung above `current`, never above the user's chosen base bitrate.
fn step_up(current: u32, base: u32) -> Option<u32> {
    if current >= base {
        return None;
    }
    LADDER
        .iter()
        .find(|&&value| value > current && value <= base)
        .copied()
        .or(Some(base))
        .filter(|value| *value > current)
}

/// Rolling packet-loss estimate for one session. Amounts are accumulated only
/// after [`WARMUP`], so the startup burst of lost packets does not trigger an
/// adaptation during an otherwise healthy stream.
#[derive(Debug)]
pub struct SessionQuality {
    /// Set when the console reports the stream connected, so time spent at
    /// the PIN prompt does not consume the warm-up window.
    started: Option<Instant>,
    last_received: u64,
    last_lost: u64,
    received: u64,
    lost: u64,
    ema_loss_pct: Option<f64>,
    peak_loss_pct: Option<f64>,
}

impl SessionQuality {
    pub fn new() -> Self {
        Self {
            started: None,
            last_received: 0,
            last_lost: 0,
            received: 0,
            lost: 0,
            ema_loss_pct: None,
            peak_loss_pct: None,
        }
    }

    /// Mark the first stream connection. Later calls are ignored.
    pub fn mark_started(&mut self, now: Instant) {
        if self.started.is_none() {
            self.started = Some(now);
        }
    }

    /// Feed a cumulative `(received, lost)` snapshot from [`PacketStats`].
    pub fn sample(&mut self, received: u64, lost: u64, now: Instant) {
        let delta_received = received.saturating_sub(self.last_received);
        let delta_lost = lost.saturating_sub(self.last_lost);
        self.last_received = received;
        self.last_lost = lost;

        let Some(started) = self.started else {
            return;
        };
        if now.duration_since(started) < WARMUP {
            return;
        }
        let total = delta_received + delta_lost;
        if total < 20 {
            return;
        }
        self.received = self.received.saturating_add(delta_received);
        self.lost = self.lost.saturating_add(delta_lost);
        let instant = delta_lost as f64 / total as f64 * 100.0;
        self.ema_loss_pct = Some(match self.ema_loss_pct {
            Some(previous) => previous * 0.8 + instant * 0.2,
            None => instant,
        });
        self.peak_loss_pct = Some(match self.peak_loss_pct {
            Some(previous) => previous.max(instant),
            None => instant,
        });
    }

    /// Recent loss for the live stats display.
    pub fn loss_pct(&self) -> Option<f64> {
        self.ema_loss_pct
    }

    /// Worst one-second window after the warm-up, used to catch burst loss.
    pub fn peak_loss_pct(&self) -> Option<f64> {
        self.peak_loss_pct
    }

    /// Whole-session loss used for the adaptation decision.
    pub fn overall_loss_pct(&self) -> Option<f64> {
        let total = self.received + self.lost;
        (total >= MIN_PACKETS).then(|| self.lost as f64 / total as f64 * 100.0)
    }

    pub fn observed_packets(&self) -> u64 {
        self.received + self.lost
    }

    pub fn elapsed(&self) -> Duration {
        self.started
            .map(|started| started.elapsed())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store() -> ConfigStore {
        let dir = tempdir().expect("tempdir");
        ConfigStore::load(Some(dir.path().to_path_buf())).expect("store")
    }

    #[test]
    fn steps_down_and_up_the_ladder() {
        assert_eq!(step_down(15000, 30000), Some(10000));
        assert_eq!(step_down(12000, 30000), Some(10000));
        assert_eq!(step_down(4000, 30000), Some(3000));
        assert_eq!(step_down(3000, 30000), None);
        assert_eq!(step_up(10000, 30000), Some(15000));
        assert_eq!(step_up(10000, 12000), Some(12000));
        assert_eq!(step_up(12000, 12000), None);
    }

    #[test]
    fn a_changed_base_resets_the_stored_step() {
        let store = store();
        let (initial, stepped) = resolve(&store, "host", 15000);
        assert_eq!(initial.current_kbps, 15000);
        assert!(!stepped);

        let context = AdaptiveContext {
            host_id: "host".to_string(),
            state: initial,
        };
        assert!(record(&store, &context, 4.0, 8.0, 5000, Duration::from_secs(60)).is_some());

        let (next, stepped) = resolve(&store, "host", 15000);
        assert_eq!(next.current_kbps, 10000);
        assert!(stepped);

        // A new manual base discards the old step.
        let (reset, stepped) = resolve(&store, "host", 20000);
        assert_eq!(reset.current_kbps, 20000);
        assert!(!stepped);
    }

    #[test]
    fn a_bad_session_steps_down_and_a_good_pair_returns() {
        let store = store();
        let start = AdaptiveState::new(15000);
        let context = AdaptiveContext {
            host_id: "host".to_string(),
            state: start,
        };
        assert!(record(&store, &context, 3.0, 8.0, 5000, Duration::from_secs(60)).is_some());

        let (stepped, appeared) = resolve(&store, "host", 15000);
        assert_eq!(stepped.current_kbps, 10000);
        assert!(appeared);

        let context = AdaptiveContext {
            host_id: "host".to_string(),
            state: stepped,
        };
        assert!(record(&store, &context, 0.1, 0.2, 5000, Duration::from_secs(60)).is_none());
        let (after_one, _) = resolve(&store, "host", 15000);
        assert_eq!(after_one.current_kbps, 10000);
        assert_eq!(after_one.clean_sessions, 1);

        // A later session starts from the state persisted after the first
        // clean one.
        let context = AdaptiveContext {
            host_id: "host".to_string(),
            state: after_one,
        };
        let message = record(&store, &context, 0.1, 0.2, 5000, Duration::from_secs(60));
        assert!(message.is_some());
        let (recovered, appeared) = resolve(&store, "host", 15000);
        assert_eq!(recovered.current_kbps, 15000);
        assert!(!appeared);
    }

    #[test]
    fn a_burst_steps_down_even_with_a_low_average() {
        let store = store();
        let context = AdaptiveContext {
            host_id: "host".to_string(),
            state: AdaptiveState::new(15000),
        };
        let message = record(&store, &context, 0.2, 7.5, 5000, Duration::from_secs(60))
            .expect("a burst should step down");
        assert!(message.contains("peaked at 7.5%"), "message: {message}");
        let (state, stepped) = resolve(&store, "host", 15000);
        assert_eq!(state.current_kbps, 10000);
        assert!(stepped);
    }

    #[test]
    fn short_or_small_sessions_do_not_adapt() {
        let store = store();
        let context = AdaptiveContext {
            host_id: "host".to_string(),
            state: AdaptiveState::new(15000),
        };
        assert!(record(&store, &context, 9.0, 30.0, 5000, Duration::from_secs(5)).is_none());
        assert!(record(&store, &context, 9.0, 30.0, 100, Duration::from_secs(60)).is_none());
        let (state, _) = resolve(&store, "host", 15000);
        assert_eq!(state.current_kbps, 15000);
    }

    #[test]
    fn quality_ignores_startup_loss() {
        let start = Instant::now();
        let mut quality = SessionQuality::new();
        quality.mark_started(start);
        // Startup: heavy loss in the first second is ignored.
        quality.sample(100, 50, start + Duration::from_secs(1));
        assert_eq!(quality.loss_pct(), None);
        // After warm-up, only healthy traffic is counted. The lost counter
        // stays at its warm-up value, so these deltas carry no loss.
        quality.sample(1500, 50, start + Duration::from_secs(6));
        quality.sample(2500, 50, start + Duration::from_secs(7));
        assert_eq!(quality.overall_loss_pct(), Some(0.0));
        assert_eq!(quality.observed_packets(), 2400);
        assert!(quality.loss_pct().unwrap() < 0.01);
    }

    #[test]
    fn quality_tracks_a_burst_without_losing_the_session_average() {
        let start = Instant::now();
        let mut quality = SessionQuality::new();
        quality.mark_started(start);
        quality.sample(100, 0, start + Duration::from_secs(1));
        // One bad second, then clean seconds.
        quality.sample(1100, 100, start + Duration::from_secs(6));
        for second in 7..=16 {
            quality.sample(
                cumulative_received(second),
                100,
                start + Duration::from_secs(second),
            );
        }
        let overall = quality.overall_loss_pct().expect("enough packets");
        assert!(overall < 1.0, "average was {overall}");
        assert!(
            quality.peak_loss_pct().expect("peak") >= 9.0,
            "peak was {:?}",
            quality.peak_loss_pct()
        );
        assert!(quality.loss_pct().expect("recent") < 5.0);
    }

    fn cumulative_received(second: u64) -> u64 {
        100 + (second - 5) * 1000
    }

    #[test]
    fn quality_reports_sustained_loss() {
        let start = Instant::now();
        let mut quality = SessionQuality::new();
        quality.mark_started(start);
        for second in 1..=12 {
            let received = second * 1000;
            let lost = second * 30;
            quality.sample(received, lost, start + Duration::from_secs(second));
        }
        let overall = quality.overall_loss_pct().expect("enough packets");
        assert!((overall - 2.91).abs() < 0.2, "loss was {overall}");
        assert!(quality.loss_pct().expect("recent loss") >= BAD_LOSS_PCT);
    }
}
