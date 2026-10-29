// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Known-answer tests ported from Chiaki v2.2.0.

use chiaki_core::crypto::ecdh::{CHIAKI_ECDH_SECRET_SIZE, Ecdh};
use chiaki_core::crypto::gkcrypt::GKCrypt;
use chiaki_core::crypto::rpcrypt::*;
use chiaki_core::target::Target;

fn hex16(s: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let v = hex::decode(s).unwrap();
    out.copy_from_slice(&v);
    out
}

fn hex32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let v = hex::decode(s).unwrap();
    out.copy_from_slice(&v);
    out
}

const NONCE_PRE10: &str = "430967ae364b1c452662377abf3fe939";
const MORNING_PRE10: &str = "d2789f5185a799a24452779c2b83cf07";

#[test]
fn rpcrypt_bright_ambassador_ps4_pre10() {
    let (bright, ambassador) =
        bright_ambassador(Target::Ps4_9, &hex16(NONCE_PRE10), &hex16(MORNING_PRE10)).unwrap();
    assert_eq!(bright, hex16("a44e2a165e20d30faa118bc77ca7dc11"));
    assert_eq!(ambassador, hex16("1da8b91f6e26642ebc088b004f015b52"));
}

#[test]
fn rpcrypt_bright_ambassador_modern() {
    let nonce = hex16("ae92e764882651ef89018cfa696c6938");
    let morning = hex16("74a59c9693c2083ba6a84ba050fa8e5a");
    let (bright, ambassador) = bright_ambassador(Target::Ps4_10, &nonce, &morning).unwrap();
    assert_eq!(ambassador, hex16("92bee219b9d5b1abc6494577a421e9bd"));
    assert_eq!(bright, hex16("67408c5e65665ad291a832ebe2d90abb"));
}

#[test]
fn rpcrypt_iv_ps4_pre10() {
    let crypt =
        RPCrypt::init_auth(Target::Ps4_9, &hex16(NONCE_PRE10), &hex16(MORNING_PRE10)).unwrap();
    assert_eq!(
        crypt.generate_iv(0),
        hex16("0629be04e9911c48b45c026db7b78846")
    );
    assert_eq!(
        crypt.generate_iv(0x0102_0304_0506_0708),
        hex16("3fd0830ac730fc56752dbeb82c68a704")
    );
}

#[test]
fn rpcrypt_iv_regist_ps4() {
    let ambassador = hex16("3e7e7a825973adab2f694346bd44dab5");
    let crypt = RPCrypt::init_regist(Target::Ps4_10, &ambassador, 0, 0).unwrap();
    assert_eq!(
        crypt.generate_iv(0),
        hex16("ac489977f92ac55bb9093c33b6113c46")
    );
}

#[test]
fn rpcrypt_iv_regist_ps5() {
    let ambassador = hex16("3e7e7a825973adab2f694346bd44dab5");
    let crypt = RPCrypt::init_regist(Target::Ps5_1, &ambassador, 0, 0).unwrap();
    assert_eq!(
        crypt.generate_iv(0),
        hex16("9044408273f8044dca767b5a16394d64")
    );
}

#[test]
fn rpcrypt_bright_regist_ps4() {
    let ambassador = hex16("dca1c84dfe50d65722da09654231e7c2");
    let crypt = RPCrypt::init_regist(Target::Ps4_10, &ambassador, 0x1e, 78_703_893).unwrap();
    assert_eq!(crypt.bright, hex16("edfc1dc5a2fe2d7f09198575336c1316"));
}

#[test]
fn rpcrypt_bright_regist_ps5() {
    let ambassador = hex16("dca1c84dfe50d65722da09654231e7c2");
    let crypt = RPCrypt::init_regist(Target::Ps5_1, &ambassador, 0x1e, 78_703_893).unwrap();
    assert_eq!(crypt.bright, hex16("e29d644c141b9d617431a56d34cfc17f"));
}

#[test]
fn rpcrypt_encrypt_decrypt_ps4_pre10() {
    let crypt =
        RPCrypt::init_auth(Target::Ps4_9, &hex16(NONCE_PRE10), &hex16(MORNING_PRE10)).unwrap();
    let counter = 0x0102_0304_0506_0708;

    let mut a = hex::decode("1337c0ffee").unwrap();
    crypt.encrypt(counter, &mut a).unwrap();
    assert_eq!(a, hex::decode("404863ebb4").unwrap());

    let mut b = hex::decode("dfa8a3d26ad7f34c4f874ceb8c57fb3f").unwrap();
    crypt.encrypt(counter, &mut b).unwrap();
    assert_eq!(b, hex::decode("8cd700c630253a181520eb26f7edab15").unwrap());

    let mut c = hex::decode("080d80c7b22b9ff3e2d7c8a5b092d5008de6d774").unwrap();
    crypt.encrypt(counter, &mut c).unwrap();
    assert_eq!(
        c,
        hex::decode("5b7223d3e8d956a7b8706f68cb28852a06a5d2e0").unwrap()
    );

    let mut d =
        hex::decode("a106911b4974e07303e84742084d834ef326147bde2df67d47968c3b66957e5d").unwrap();
    crypt.encrypt(counter, &mut d).unwrap();
    assert_eq!(
        d,
        hex::decode("f279320f13862927594fe08f73f7d3649d807e90d1f5d818e7be1629fb482bf8").unwrap()
    );

    // Decrypt vectors.
    let mut a = hex::decode("8dd21dfb").unwrap();
    crypt.decrypt(counter, &mut a).unwrap();
    assert_eq!(a, hex::decode("deadbeef").unwrap());

    let mut b = hex::decode("eb224eb773946a316edde58729dcd56b").unwrap();
    crypt.decrypt(counter, &mut b).unwrap();
    assert_eq!(b, hex::decode("b85deda32966a365347a424a52668541").unwrap());

    let mut c = hex::decode("2bd81d86390e2dd7de2db5bcba5ee978eec93b18").unwrap();
    crypt.decrypt(counter, &mut c).unwrap();
    assert_eq!(
        c,
        hex::decode("78a7be9263fce483848a1271c1e4b952f2bbcd39").unwrap()
    );

    let mut d =
        hex::decode("2b80daa05e8d2e1de19516b1edd2c38ab8cc6c42578dc57df15c044297253a91").unwrap();
    crypt.decrypt(counter, &mut d).unwrap();
    assert_eq!(
        d,
        hex::decode("78ff79b4047fe749bb32b17c966893a086a65638792aa803679bf2822d1a0829").unwrap()
    );
}

const HS_KEY_STREAM: &str = "83cf931a6aa769a6c4485d19c15ccc52";
const SECRET_KEY_STREAM: &str = "73c8d549c4d9db502ec044ea33648c6ac9f36c41b6a0504fe093defb619b0973";

#[test]
fn gkcrypt_key_stream_vector() {
    let gk = GKCrypt::new(42, &hex16(HS_KEY_STREAM), &hex32(SECRET_KEY_STREAM)).unwrap();
    assert_eq!(gk.key_base, hex16("08816fa2e5558961d5a286d90eec5b8c"));
    assert_eq!(gk.iv, hex16("2ae1bb3d84dc9aa9c352a4cf3ffb8b72"));

    let mut ks = [0u8; 32];
    gk.gen_key_stream(0x30, &mut ks).unwrap();
    let expected = [
        hex16("0f6d89855ba786745ba1fe5c81196cd5"),
        hex16("54c41ccaf6e934a4892698b06212b31a"),
    ]
    .concat();
    assert_eq!(&ks[..], expected.as_slice());

    let high = (1u64 << 32) + 0x420;
    let mut ks = [0u8; 32];
    gk.gen_key_stream(high, &mut ks).unwrap();
    let expected = [
        hex16("14319ff8be0817a4fc28490b1e5f7b7e"),
        hex16("fae914dec6dfe75dd69c753f9462d52c"),
    ]
    .concat();
    assert_eq!(&ks[..], expected.as_slice());
}

const HS_ENDEC: &str = "14f1e6946c5dcea8b7aa4850f64d21ac";
const SECRET_ENDEC: &str = "0ceb7709834d7afc50b8468cc63c1e7c4e4a88934280c128e61ee9d41b8c6936";
const CLEAR_ENDEC: &str =
    "4e619f945d4b8ebd2a154d036acd49569cc75ce3e700179a38d9695345f90cb58c05650f70";
const ENC_ENDEC: &str =
    "23f48dd8aaf9589bb1944fad2b8daa8d2588faf8b6d417f45f78ecf54e3720b0768107679a";

#[test]
fn gkcrypt_encrypt_decrypt_vector() {
    let gk = GKCrypt::new(42, &hex16(HS_ENDEC), &hex32(SECRET_ENDEC)).unwrap();
    assert_eq!(gk.key_base, hex16("277aa51dacd15f0e5412face0dc4636a"));
    assert_eq!(gk.iv, hex16("ef2040c2153c0266321f42bbf450344d"));

    let mut buf = hex::decode(CLEAR_ENDEC).unwrap();
    gk.encrypt(0x11, &mut buf).unwrap();
    assert_eq!(buf, hex::decode(ENC_ENDEC).unwrap());

    let mut buf = hex::decode(CLEAR_ENDEC).unwrap();
    gk.decrypt(0x11, &mut buf).unwrap();
    assert_eq!(buf, hex::decode(ENC_ENDEC).unwrap());

    let high = (1u64 << 32) + 0x420;
    let expected = "2b9ee983274408b117f537a0d9c2c6059578b278fd178c52f4179dee3f62e63001614cf4a1";
    let mut buf = hex::decode(CLEAR_ENDEC).unwrap();
    gk.encrypt(high, &mut buf).unwrap();
    assert_eq!(buf, hex::decode(expected).unwrap());
}

mod gmac_vectors {
    include!("vectors/gmac.rs");
}
mod gmac_split_vectors {
    include!("vectors/gmac_split.rs");
}
mod gmac_refresh_vectors {
    include!("vectors/gmac_refresh.rs");
}

#[test]
fn gkcrypt_gmac_vector() {
    let mut gk = GKCrypt {
        index: 42,
        iv: gmac_vectors::GKCRYPT_IV,
        key_base: [0; 16],
        key_gmac_base: [0; 16],
        key_gmac_current: gmac_vectors::GKCRYPT_KEY,
        key_gmac_index_current: 0,
    };
    let tag = gk.gmac(gmac_vectors::KEY_POS, &gmac_vectors::BUF).unwrap();
    assert_eq!(tag, gmac_vectors::GMAC_EXPECTED);

    let mut gk = GKCrypt {
        index: 42,
        iv: gmac_vectors::GKCRYPT_IV,
        key_base: [0; 16],
        key_gmac_base: [0; 16],
        key_gmac_current: gmac_vectors::GKCRYPT_KEY,
        key_gmac_index_current: 0,
    };
    let tag = gk
        .gmac(gmac_vectors::KEY_POS_HIGH, &gmac_vectors::BUF)
        .unwrap();
    assert_eq!(tag, gmac_vectors::GMAC_EXPECTED_HIGH);
}

#[test]
fn gkcrypt_gmac_split_vector() {
    let mut gk = GKCrypt {
        index: 42,
        iv: gmac_split_vectors::GKCRYPT_IV,
        key_base: [0; 16],
        key_gmac_base: [0; 16],
        key_gmac_current: gmac_split_vectors::GKCRYPT_KEY,
        key_gmac_index_current: 0,
    };
    let tag = gk
        .gmac(gmac_split_vectors::KEY_POS, &gmac_split_vectors::BUF)
        .unwrap();
    assert_eq!(tag, gmac_split_vectors::GMAC_EXPECTED);

    let mut gk = GKCrypt {
        index: 42,
        iv: gmac_split_vectors::GKCRYPT_IV,
        key_base: [0; 16],
        key_gmac_base: [0; 16],
        key_gmac_current: gmac_split_vectors::GKCRYPT_KEY,
        key_gmac_index_current: 0,
    };
    let tag = gk
        .gmac(gmac_split_vectors::KEY_POS_HIGH, &gmac_split_vectors::BUF)
        .unwrap();
    assert_eq!(tag, gmac_split_vectors::GMAC_EXPECTED_HIGH);
}

#[test]
fn gkcrypt_gmac_key_refresh_vector() {
    let mut gk = GKCrypt::new(
        3,
        &gmac_refresh_vectors::HANDSHAKE_KEY,
        &gmac_refresh_vectors::ECDH_SECRET,
    )
    .unwrap();
    let tag = gk
        .gmac(gmac_refresh_vectors::KEY_POS, &gmac_refresh_vectors::DATA)
        .unwrap();
    assert_eq!(tag, gmac_refresh_vectors::GMAC_EXPECTED);
}

#[test]
fn gkcrypt_gen_gmac_key_vector() {
    let key_initial = hex16("beeba0f03d05707d3ac73cd732b94801");
    let iv = hex16("e87187e763e0df463dc2024a2cd29c45");
    let expected = hex16("e3db92d9ddd36899aefd9b15e1a6878b");
    assert_eq!(GKCrypt::gen_gmac_key(1, &key_initial, &iv), expected);
}

#[test]
fn ecdh_module_vector_smoke() {
    // The full vector lives in the ecdh unit test; this ensures the public
    // secret size stays in sync.
    assert_eq!(CHIAKI_ECDH_SECRET_SIZE, 32);
    let _ = Ecdh::new().unwrap();
}
