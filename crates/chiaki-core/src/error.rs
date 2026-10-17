// SPDX-License-Identifier: AGPL-3.0-only
//! Error types and protocol reason codes.

use std::fmt;

/// Core protocol errors. Mirrors `ChiakiErrorCode` where it matters, with
/// additional context for the browser bridge.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("unknown error")]
    Unknown,
    #[error("failed to parse address")]
    ParseAddr,
    #[error("thread error")]
    Thread,
    #[error("out of memory")]
    Memory,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("network error")]
    Network,
    #[error("connection refused")]
    ConnectionRefused,
    #[error("host is down")]
    HostDown,
    #[error("host unreachable")]
    HostUnreach,
    #[error("disconnected")]
    Disconnected,
    #[error("invalid data")]
    InvalidData,
    #[error("buffer too small")]
    BufferTooSmall,
    #[error("canceled")]
    Canceled,
    #[error("timeout")]
    Timeout,
    #[error("invalid response")]
    InvalidResponse,
    #[error("invalid MAC")]
    InvalidMac,
    #[error("not initialized")]
    Uninitialized,
    #[error("FEC decode failed")]
    FecFailed,
    #[error("version mismatch")]
    VersionMismatch,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Why a session ended. Mirrors `ChiakiQuitReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitReason {
    None,
    Stopped,
    SessionRequestUnknown,
    SessionRequestConnectionRefused,
    SessionRequestRpInUse,
    SessionRequestRpCrash,
    SessionRequestRpVersionMismatch,
    CtrlUnknown,
    CtrlConnectFailed,
    CtrlConnectionRefused,
    StreamConnectionUnknown,
    StreamConnectionRemoteDisconnected,
    StreamConnectionRemoteShutdown,
    CryptoFailed,
}

impl QuitReason {
    pub fn is_error(self) -> bool {
        !matches!(
            self,
            QuitReason::Stopped | QuitReason::StreamConnectionRemoteShutdown | QuitReason::None
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QuitReason::None => "Unknown",
            QuitReason::Stopped => "Stopped",
            QuitReason::SessionRequestUnknown => "Unknown Session Request Error",
            QuitReason::SessionRequestConnectionRefused => "Connection Refused in Session Request",
            QuitReason::SessionRequestRpInUse => "Remote Play on Console is already in use",
            QuitReason::SessionRequestRpCrash => "Remote Play on Console has crashed",
            QuitReason::SessionRequestRpVersionMismatch => "RP-Version mismatch",
            QuitReason::CtrlUnknown => "Unknown Ctrl Error",
            QuitReason::CtrlConnectFailed => "Ctrl failed to connect",
            QuitReason::CtrlConnectionRefused => "Connection Refused in Ctrl",
            QuitReason::StreamConnectionUnknown => "Unknown Error in Stream Connection",
            QuitReason::StreamConnectionRemoteDisconnected => {
                "Remote has disconnected from Stream Connection"
            }
            QuitReason::StreamConnectionRemoteShutdown => {
                "Remote has disconnected because the Server shut down"
            }
            QuitReason::CryptoFailed => "Cryptography initialization failed",
        }
    }
}

impl fmt::Display for QuitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// RP application reason codes returned by the console in HTTP headers.
pub const RP_APPLICATION_REASON_REGIST_FAILED: u32 = 0x8010_8b09;
pub const RP_APPLICATION_REASON_INVALID_PSN_ID: u32 = 0x8010_8b02;
pub const RP_APPLICATION_REASON_IN_USE: u32 = 0x8010_8b10;
pub const RP_APPLICATION_REASON_CRASH: u32 = 0x8010_8b15;
pub const RP_APPLICATION_REASON_RP_VERSION: u32 = 0x8010_8b11;
pub const RP_APPLICATION_REASON_UNKNOWN: u32 = 0x8010_8bff;

pub fn rp_application_reason_string(reason: u32) -> &'static str {
    match reason {
        RP_APPLICATION_REASON_REGIST_FAILED => "Regist failed, probably invalid PIN",
        RP_APPLICATION_REASON_INVALID_PSN_ID => "Invalid PSN ID",
        RP_APPLICATION_REASON_IN_USE => "Remote is already in use",
        RP_APPLICATION_REASON_CRASH => "Remote Play on Console crashed",
        RP_APPLICATION_REASON_RP_VERSION => "RP-Version mismatch",
        _ => "unknown",
    }
}
