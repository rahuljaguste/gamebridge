// SPDX-License-Identifier: AGPL-3.0-only
//! Browser bridge for PlayStation Remote Play.

mod config;
mod demo;
mod manager;
mod psn;
mod routes;
mod state;
mod webrtc;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::ConfigStore;
use crate::state::AppState;

#[derive(Debug, Parser)]
#[command(name = "ps-remoteplay", version, about)]
struct Args {
    /// Address to serve the web UI on (localhost only by default).
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

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
    let state = AppState::new(store, args.demo_file.clone());
    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    println!();
    println!("  PS Remote Play bridge v{}", env!("CARGO_PKG_VERSION"));
    println!("  Open http://{}/", args.bind);
    println!("  Log:  {}", log_path.display());
    println!("  Press Ctrl+C to stop.");
    println!();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
