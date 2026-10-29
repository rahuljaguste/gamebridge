// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Exact launch-spec JSON from Chiaki's `launchspec.c`.

use crate::target::{Codec, Target};
use crate::util::base64;

#[allow(clippy::too_many_arguments)]
pub fn format(
    target: Target,
    mtu: u32,
    rtt_ms: u32,
    handshake_key: &[u8; 16],
    width: u32,
    height: u32,
    max_fps: u32,
    codec: Codec,
    bw_kbps_sent: u32,
) -> String {
    let (extra0, extra1, extra2) = if target.is_ps5() {
        (
            ",\"adaptiveStreamMode\": \"resize\"",
            if codec.is_h265() {
                "\"videoCodec\":\"hevc\","
            } else {
                "\"videoCodec\":\"avc\","
            },
            if codec.is_hdr() {
                "\"dynamicRange\":\"HDR\","
            } else {
                "\"dynamicRange\":\"SDR\","
            },
        )
    } else {
        ("", "", "")
    };
    let key = base64::encode(handshake_key);
    let bw = bw_kbps_sent;
    let rtt = rtt_ms;
    format!(
        concat!(
            "{{",
            "\"sessionId\":\"sessionId4321\",",
            "\"streamResolutions\":[",
            "{{",
            "\"resolution\":",
            "{{",
            "\"width\":{width},",
            "\"height\":{height}",
            "}},",
            "\"maxFps\":{max_fps},",
            "\"score\":10",
            "}}",
            "],",
            "\"network\":{{",
            "\"bwKbpsSent\":{bw},",
            "\"bwLoss\":0.001000,",
            "\"mtu\":{mtu},",
            "\"rtt\":{rtt},",
            "\"ports\":[53,2053]",
            "}},",
            "\"slotId\":1,",
            "\"appSpecification\":{{",
            "\"minFps\":30,",
            "\"minBandwidth\":0,",
            "\"extTitleId\":\"ps3\",",
            "\"version\":1,",
            "\"timeLimit\":1,",
            "\"startTimeout\":100,",
            "\"afkTimeout\":100,",
            "\"afkTimeoutDisconnect\":100",
            "}},",
            "\"konan\":{{",
            "\"ps3AccessToken\":\"accessToken\",",
            "\"ps3RefreshToken\":\"refreshToken\"",
            "}},\"requestGameSpecification\":{{",
            "\"model\":\"bravia_tv\",",
            "\"platform\":\"android\",",
            "\"audioChannels\":\"5.1\",",
            "\"language\":\"sp\",",
            "\"acceptButton\":\"X\",",
            "\"connectedControllers\":[\"xinput\",\"ds3\",\"ds4\"],",
            "\"yuvCoefficient\":\"bt601\",",
            "\"videoEncoderProfile\":\"hw4.1\",",
            "\"audioEncoderProfile\":\"audio1\"{extra0}",
            "}},",
            "\"userProfile\":{{",
            "\"onlineId\":\"psnId\",",
            "\"npId\":\"npId\",",
            "\"region\":\"US\",",
            "\"languagesUsed\":[\"en\",\"jp\"]",
            "}},",
            "{extra1}{extra2}\"handshakeKey\":\"{key}\"",
            "}}"
        ),
        width = width,
        height = height,
        max_fps = max_fps,
        bw = bw,
        mtu = mtu,
        rtt = rtt,
        extra0 = extra0,
        extra1 = extra1,
        extra2 = extra2,
        key = key,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps4_has_no_extras() {
        let s = format(
            Target::Ps4_10,
            1454,
            1,
            &[0u8; 16],
            1920,
            1080,
            60,
            Codec::H264,
            15000,
        );
        assert!(s.contains("\"sessionId\":\"sessionId4321\""));
        assert!(s.contains("\"width\":1920,\"height\":1080"));
        assert!(s.contains("\"maxFps\":60"));
        assert!(s.contains("\"mtu\":1454,\"rtt\":1"));
        assert!(s.contains("\"audioEncoderProfile\":\"audio1\"}"));
        assert!(!s.contains("adaptiveStreamMode"));
        assert!(s.ends_with("\"handshakeKey\":\"AAAAAAAAAAAAAAAAAAAAAA==\"}"));
    }

    #[test]
    fn ps5_extras() {
        let s = format(
            Target::Ps5_1,
            1454,
            2,
            &[1u8; 16],
            1920,
            1080,
            60,
            Codec::H264,
            15000,
        );
        assert!(s.contains(",\"adaptiveStreamMode\": \"resize\"}"));
        assert!(s.contains("\"videoCodec\":\"avc\","));
        assert!(s.contains("\"dynamicRange\":\"SDR\","));
        assert!(s.contains("\"rtt\":2"));
    }
}
