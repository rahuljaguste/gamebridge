// SPDX-License-Identifier: AGPL-3.0-only
//! Cryptography used by the Remote Play protocol.
//!
//! The algorithms and constants are ported from Chiaki's `rpcrypt.c`,
//! `gkcrypt.c` and `ecdh.c` (AGPL-3.0-only-OpenSSL).

pub mod cfb128;
pub mod ecdh;
pub mod gkcrypt;
pub mod rpcrypt;
pub mod rpcrypt_keys;
