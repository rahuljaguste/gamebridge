// SPDX-License-Identifier: AGPL-3.0-only
//! PlayStation targets, codecs and video profile presets.

use crate::error::{Error, Result};

/// Values mirror `ChiakiTarget`; the numeric value is part of the protocol
/// behavior (comparisons are used to select PS4 generations and PS5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Target {
    #[default]
    Ps4Unknown = 0,
    Ps4_8 = 800,
    Ps4_9 = 900,
    Ps4_10 = 1000,
    Ps5Unknown = 1_000_000,
    Ps5_1 = 1_000_100,
}

impl Target {
    pub fn is_ps5(self) -> bool {
        self >= Target::Ps5Unknown
    }

    pub fn is_unknown(self) -> bool {
        matches!(self, Target::Ps4Unknown | Target::Ps5Unknown)
    }

    /// RP-Version string used in HTTP headers and the launch spec.
    pub fn rp_version(self) -> Option<&'static str> {
        match self {
            Target::Ps4_8 => Some("8.0"),
            Target::Ps4_9 => Some("9.0"),
            Target::Ps4_10 => Some("10.0"),
            Target::Ps5_1 => Some("1.0"),
            _ => None,
        }
    }

    pub fn parse_rp_version(s: &str, ps5: bool) -> Target {
        if ps5 {
            return if s == "1.0" {
                Target::Ps5_1
            } else {
                Target::Ps5Unknown
            };
        }
        match s {
            "8.0" => Target::Ps4_8,
            "9.0" => Target::Ps4_9,
            "10.0" => Target::Ps4_10,
            _ => Target::Ps4Unknown,
        }
    }

    /// Translate a discovered `system-version` into a target, as
    /// `chiaki_discovery_host_system_version_target` does.
    pub fn from_system_version(version: &str, ps5: bool) -> Target {
        let v = version.trim().parse::<u64>().unwrap_or(0);
        if ps5 {
            if v >= 8_050_001 {
                Target::Ps5_1
            } else if v >= 8_050_000 {
                Target::Ps5Unknown
            } else {
                Target::Ps4Unknown
            }
        } else if v >= 8_000_000 {
            Target::Ps4_10
        } else if v >= 7_000_000 {
            Target::Ps4_9
        } else if v > 0 {
            Target::Ps4_8
        } else {
            Target::Ps4Unknown
        }
    }

    /// Takion protocol version used for the media connection.
    pub fn takion_protocol_version(self) -> u8 {
        if self.is_ps5() { 12 } else { 9 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    H264 = 0,
    H265 = 1,
    H265Hdr = 2,
}

impl Codec {
    pub fn is_h265(self) -> bool {
        matches!(self, Codec::H265 | Codec::H265Hdr)
    }

    pub fn is_hdr(self) -> bool {
        self == Codec::H265Hdr
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H264",
            Codec::H265 => "H265",
            Codec::H265Hdr => "H265 HDR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
}

impl VideoProfile {
    pub fn preset(resolution: Resolution, fps: Fps) -> Result<Self> {
        let (width, height, bitrate_kbps) = match resolution {
            Resolution::P360 => (640, 360, 2000),
            Resolution::P540 => (960, 540, 6000),
            Resolution::P720 => (1280, 720, 10000),
            Resolution::P1080 => (1920, 1080, 15000),
        };
        let max_fps = match fps {
            Fps::Fps30 => 30,
            Fps::Fps60 => 60,
        };
        Ok(Self {
            width,
            height,
            max_fps,
            bitrate_kbps,
            codec: Codec::H264,
        })
    }

    pub fn downgrade_to_720p(&self) -> Self {
        let mut p = *self;
        p.width = 1280;
        p.height = 720;
        p.bitrate_kbps = 10000;
        p
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    P360 = 1,
    P540 = 2,
    P720 = 3,
    P1080 = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fps {
    Fps30 = 30,
    Fps60 = 60,
}

impl Resolution {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "360p" | "360" => Ok(Resolution::P360),
            "540p" | "540" => Ok(Resolution::P540),
            "720p" | "720" => Ok(Resolution::P720),
            "1080p" | "1080" => Ok(Resolution::P1080),
            _ => Err(Error::InvalidData),
        }
    }
}

impl Fps {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "30" => Ok(Fps::Fps30),
            "60" => Ok(Fps::Fps60),
            _ => Err(Error::InvalidData),
        }
    }
}
