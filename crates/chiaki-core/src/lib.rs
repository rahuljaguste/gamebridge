// SPDX-License-Identifier: AGPL-3.0-only
//! Rust port of the Chiaki v2.2.0 PlayStation Remote Play protocol core.
//!
//! The protocol constants and algorithms are derived from Chiaki by Florian
//! Märkl and contributors, licensed AGPL-3.0-only-OpenSSL. This crate is
//! licensed AGPL-3.0-only; see the repository README and COPYING.

pub mod controller;
pub mod crypto;
pub mod ctrl;
pub mod discovery;
pub mod error;
pub mod feedback;
pub mod http;
pub mod media;
pub mod packet_stats;
pub mod proto;
pub mod regist;
pub mod senkusha;
pub mod session;
pub mod stream;
pub mod takion;
pub mod target;
pub mod util;

pub use error::{Error, QuitReason, Result};
