// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Real captured AV packet vectors from Chiaki's test suite.

use chiaki_core::crypto::gkcrypt::GKCrypt;
use chiaki_core::takion::KeyState;
use chiaki_core::takion::packet::parse_av;
use chiaki_core::util::base64;

const HANDSHAKE_KEY: &str = "b3148857b04605966237e055b560c406";
const ECDH_SECRET: &str = "aabc10460d6ded4b583388dbdb0286db2dd8915237eb593742acb75d5d5e82b4";

fn key16(s: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&hex::decode(s).unwrap());
    out
}

fn key32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&hex::decode(s).unwrap());
    out
}

#[test]
fn real_video_packet_parse_and_decrypt() {
    let mut packet = base64::decode(
        "AgAAAAEAAAQBAwlCI7oAAACAA5gAhJT79yG83L9WhXCDP/h48VDjBL03cGscKH6DLQ3ZOzh007JcllEEIpTP3DbJwYAF5MGH2BwWJJTcbZATOGrzCznFMInIL7pWssSgxQBcf2q+u+vECFWQ5zvEQsq4RxQs3bre0DchyEMz",
    )
    .unwrap();
    let nalu = base64::decode(
        "AAMAAAABZYiAhn8AL8gD/wh+/miASsv/rw5fm2PYQAAAAwAAAwAFIc42jXIHLchAAAe8A/i2oRmALiO8cB3sEzDosBxyZyFg4KK5rAXoBy0/2EfQAAADAAADAAADAAADAAADAAADAAJq",
    )
    .unwrap();
    let mut key_state = KeyState::new();
    let parsed = parse_av(&packet, false, &mut key_state).unwrap();
    assert!(parsed.is_video);
    assert_eq!(parsed.data_len, nalu.len());
    let gk = GKCrypt::new(3, &key16(HANDSHAKE_KEY), &key32(ECDH_SECRET)).unwrap();
    gk.decrypt(
        parsed.key_pos + 0x10,
        &mut packet[parsed.data_offset..parsed.data_offset + parsed.data_len],
    )
    .unwrap();
    assert_eq!(
        &packet[parsed.data_offset..parsed.data_offset + parsed.data_len],
        nalu.as_slice()
    );
}
