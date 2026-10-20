// SPDX-License-Identifier: AGPL-3.0-only
//! GameBridge — an unofficial browser bridge for PlayStation Remote Play.

mod adaptive;
mod config;
mod demo;
mod manager;
mod psn;
mod routes;
mod state;
mod webrtc;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::ConfigStore;
use crate::routes::auth::Auth;
use crate::state::AppState;

#[derive(Debug, Parser)]
#[command(
    name = "gamebridge",
    version,
    about = "Unofficial browser bridge for PlayStation Remote Play"
)]
struct Args {
    /// Address to serve the web UI on (localhost only by default).
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Allow binding to a non-loopback address. Without one of the auth flags
    /// the bridge is unauthenticated.
    #[arg(long)]
    allow_remote: bool,

    /// Require this token for API and WebSocket access. Enables authentication.
    #[arg(long, value_name = "TOKEN", conflicts_with = "auth_token_file")]
    auth_token: Option<String>,

    /// Read the API/WebSocket access token from a file. Enables authentication.
    #[arg(long, value_name = "PATH", conflicts_with = "auth_token")]
    auth_token_file: Option<PathBuf>,

    /// Override the configuration directory.
    #[arg(long)]
    config_dir: Option<PathBuf>,

    /// Enable demo mode with a local Annex-B H.264 file (no console needed).
    #[arg(long)]
    demo_file: Option<PathBuf>,

    /// Write logs to this file as well as stderr (default: server.log in the
    /// configuration directory).
    #[arg(long)]
    log_file: Option<PathBuf>,
}

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    const MAX_LOG_BYTES: u64 = 20 * 1024 * 1024;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(metadata) = std::fs::metadata(path)
        && metadata.len() > MAX_LOG_BYTES
    {
        let backup = PathBuf::from(format!("{}.1", path.display()));
        let _ = std::fs::rename(path, backup);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn init_tracing(log_path: &Path) {
    let filter = || {
        EnvFilter::try_from_default_env()
            // The WebRTC stack warns about the first STUN checks arriving
            // before the answer is installed ("password is empty") and about
            // harmless DTLS extension/mDNS quirks. They are expected in
            // non-trickle signaling; use RUST_LOG=rtc=warn to see them.
            .unwrap_or_else(|_| {
                EnvFilter::new(
                    "info,webrtc=error,rtc=error,rtc_ice=error,rtc_dtls=error,rtc_mdns=error",
                )
            })
    };
    let registry = tracing_subscriber::registry()
        .with(filter())
        .with(tracing_subscriber::fmt::layer());
    match open_log(log_path) {
        Ok(file) => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(file),
            )
            .init(),
        Err(e) => {
            registry.init();
            tracing::warn!("could not open log file {}: {e}", log_path.display());
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = ConfigStore::load(args.config_dir.clone())?;
    let log_path = args
        .log_file
        .clone()
        .unwrap_or_else(|| store.path().with_file_name("server.log"));
    init_tracing(&log_path);
    tracing::info!("configuration: {}", store.path().display());
    tracing::info!("log file: {}", log_path.display());

    let token = match (&args.auth_token, &args.auth_token_file) {
        (Some(token), _) => Some(token.clone()),
        (None, Some(path)) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read auth token file {}", path.display()))?;
            Some(raw.trim_end_matches(['\r', '\n']).to_string())
        }
        (None, None) => None,
    };
    if let Some(token) = token.as_deref() {
        if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
            anyhow::bail!(
                "the auth token must be non-empty and contain no whitespace or control characters"
            );
        }
        if token.len() < 16 {
            tracing::warn!("the auth token is short; use a random 32-byte token");
        }
    }
    let auth = match token.as_deref() {
        Some(token) => Auth::from_token(token),
        None => Auth::disabled(),
    };
    let auth_required = auth.required();
    if auth_required {
        tracing::info!("authentication is enabled");
    }

    if !args.bind.ip().is_loopback() {
        if !args.allow_remote {
            anyhow::bail!(
                "refusing to bind {}: the web UI is localhost-only; \
                 pass --allow-remote to expose it",
                args.bind
            );
        }
        if auth_required {
            tracing::info!("network clients must sign in with the auth token");
        } else {
            tracing::warn!(
                "binding {} exposes an UNAUTHENTICATED Remote Play bridge to the network; \
                 pass --auth-token or --auth-token-file to require a token",
                args.bind
            );
        }
    }
    let state = AppState::new(store, args.demo_file.clone());
    let shutdown_state = state.clone();
    let app = routes::router(state, args.bind, args.allow_remote, auth);

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    println!();
    println!("  GameBridge v{}", env!("CARGO_PKG_VERSION"));
    println!("  Open http://{}/", args.bind);
    println!("  Log:  {}", log_path.display());
    println!(
        "  Auth: {}",
        if auth_required {
            "token required"
        } else {
            "disabled"
        }
    );
    println!("  Press Ctrl+C to stop.");
    println!();

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_state))
    .await
    .context("server error")?;
    Ok(())
}

async fn shutdown_signal(state: Arc<AppState>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => tracing::info!("received Ctrl+C"),
                    _ = sigterm.recv() => tracing::info!("received SIGTERM"),
                }
            }
            Err(e) => {
                tracing::warn!("could not listen for SIGTERM: {e}");
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("received Ctrl+C");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received Ctrl+C");
    }

    tracing::info!("shutting down");
    state.shutdown.cancel();
    stop_active_session(&state).await;
}

/// Ask the session manager to stop before the runtime is torn down, giving it
/// time to send the console a graceful Remote Play stop.
async fn stop_active_session(state: &AppState) {
    {
        let mut active = state.active.lock().await;
        let Some(session) = active.as_mut() else {
            return;
        };
        tracing::info!("stopping the active session");
        // `/api/session/stop` may already have taken the sender. Waiting for
        // `active` to clear still covers that path; returning early would let
        // the runtime abort the manager mid-cleanup.
        if let Some(stop) = session.stop_tx.take() {
            let _ = stop.send(());
        }
    }

    // The manager may spend 5 s waiting for the supervisor, then close the
    // peer and run `Session::stop` (3 s). Allow for that before giving up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if state.active.lock().await.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tracing::warn!("active session did not stop within 15 s");
}
