// SPDX-License-Identifier: AGPL-3.0-only
//! RPCrypt: the registration/control channel cipher.
//!
//! Bright/ambassador keys are derived from the console nonce and the
//! registration `RP-Key` ("morning"). Payloads are AES-128-CFB128 encrypted
//! with an IV derived by HMAC-SHA256 and a monotonically increasing counter.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::crypto::cfb128;
use crate::crypto::rpcrypt_keys::*;
use crate::error::{Error, Result};
use crate::target::Target;

type HmacSha256 = Hmac<Sha256>;

const ECHO_B: [u8; 16] = [
    0xe1, 0xec, 0x9c, 0x3a, 0xdd, 0xbd, 0x08, 0x85, 0xfc, 0x0e, 0x1d, 0x78, 0x90, 0x32, 0xc0, 0x04,
];

const HMAC_KEY_PS4_PRE10: [u8; 16] = [
    0xac, 0x07, 0x88, 0x83, 0xc8, 0x3a, 0x1f, 0xe8, 0x11, 0x46, 0x3a, 0xf3, 0x9e, 0xe3, 0xe3, 0x77,
];
const HMAC_KEY_PS4: [u8; 16] = [
    0x20, 0xd6, 0x6f, 0x59, 0x04, 0xea, 0x7c, 0x14, 0xe5, 0x57, 0xff, 0xc5, 0x2e, 0x48, 0x8a, 0xc8,
];
const HMAC_KEY_PS5: [u8; 16] = [
    0x46, 0x46, 0x87, 0xb3, 0x49, 0xca, 0x8c, 0xe8, 0x59, 0xc5, 0x27, 0x0f, 0x5d, 0x7a, 0x69, 0xd6,
];

#[derive(Debug, Clone)]
pub struct RPCrypt {
    pub target: Target,
    pub bright: [u8; 16],
    pub ambassador: [u8; 16],
}

impl RPCrypt {
    pub fn init_auth(target: Target, nonce: &[u8; 16], morning: &[u8; 16]) -> Result<Self> {
        let (bright, ambassador) = bright_ambassador(target, nonce, morning)?;
        Ok(Self {
            target,
            bright,
            ambassador,
        })
    }

    pub fn init_regist(
        target: Target,
        ambassador: &[u8; 16],
        key_0_off: usize,
        pin: u32,
    ) -> Result<Self> {
        if target < Target::Ps4_10 {
            return Err(Error::InvalidData);
        }
        if key_0_off >= 0x20 {
            return Err(Error::InvalidData);
        }
        let keys_0 = if target.is_ps5() {
            &PS5_KEYS_0
        } else {
            &PS4_KEYS_0
        };
        let mut bright = [0u8; 16];
        for (i, b) in bright.iter_mut().enumerate() {
            *b = keys_0[i][key_0_off];
        }
        bright[0xc] ^= (pin >> 0x18) as u8;
        bright[0xd] ^= (pin >> 0x10) as u8;
        bright[0xe] ^= (pin >> 0x08) as u8;
        bright[0xf] ^= pin as u8;
        Ok(Self {
            target,
            bright,
            ambassador: *ambassador,
        })
    }

    pub fn init_regist_ps4_pre10(ambassador: &[u8; 16], pin: u32) -> Self {
        const REGIST_AES_KEY: [u8; 16] = [
            0x3f, 0x1c, 0xc4, 0xb6, 0xdc, 0xbb, 0x3e, 0xcc, 0x50, 0xba, 0xed, 0xef, 0x97, 0x34,
            0xc7, 0xc9,
        ];
        let mut bright = REGIST_AES_KEY;
        bright[0] ^= (pin >> 0x18) as u8;
        bright[1] ^= (pin >> 0x10) as u8;
        bright[2] ^= (pin >> 0x08) as u8;
        bright[3] ^= pin as u8;
        Self {
            target: Target::Ps4_9,
            bright,
            ambassador: *ambassador,
        }
    }

    pub fn hmac_key(&self) -> &'static [u8; 16] {
        match self.target {
            Target::Ps5_1 | Target::Ps5Unknown => &HMAC_KEY_PS5,
            Target::Ps4_8 | Target::Ps4_9 => &HMAC_KEY_PS4_PRE10,
            _ => &HMAC_KEY_PS4,
        }
    }

    pub fn generate_iv(&self, counter: u64) -> [u8; 16] {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(self.hmac_key()).expect("fixed hmac key");
        mac.update(&self.ambassador);
        mac.update(&counter.to_be_bytes());
        let digest = mac.finalize().into_bytes();
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&digest[..16]);
        iv
    }

    pub fn encrypt(&self, counter: u64, data: &mut [u8]) -> Result<()> {
        let iv = self.generate_iv(counter);
        cfb128::encrypt(&self.bright, &iv, data)
    }

    pub fn decrypt(&self, counter: u64, data: &mut [u8]) -> Result<()> {
        let iv = self.generate_iv(counter);
        cfb128::decrypt(&self.bright, &iv, data)
    }
}

fn bright_ambassador_ps4_pre10(nonce: &[u8; 16], morning: &[u8; 16]) -> ([u8; 16], [u8; 16]) {
    const ECHO_A: [u8; 16] = [
        0x01, 0x49, 0x87, 0x9b, 0x65, 0x39, 0x8b, 0x39, 0x4b, 0x3a, 0x8d, 0x48, 0xc3, 0x0a, 0xef,
        0x51,
    ];
    let mut bright = [0u8; 16];
    let mut ambassador = [0u8; 16];
    for i in 0..16 {
        let mut v = nonce[i];
        v = v.wrapping_sub(i as u8);
        v = v.wrapping_sub(0x27);
        v ^= ECHO_A[i];
        ambassador[i] = v;
    }
    for i in 0..16 {
        let mut v = morning[i];
        v = v.wrapping_sub(i as u8);
        v = v.wrapping_add(0x34);
        v ^= ECHO_B[i];
        v ^= nonce[i];
        bright[i] = v;
    }
    (bright, ambassador)
}

pub fn bright_ambassador(
    target: Target,
    nonce: &[u8; 16],
    morning: &[u8; 16],
) -> Result<([u8; 16], [u8; 16])> {
    match target {
        Target::Ps4_8 | Target::Ps4_9 => Ok(bright_ambassador_ps4_pre10(nonce, morning)),
        _ => {
            if target < Target::Ps4_10 {
                return Err(Error::InvalidData);
            }
            let (keys_a, keys_b) = if target.is_ps5() {
                (&KEYS_A_PS5, &KEYS_B_PS5)
            } else {
                (&KEYS_A_PS4, &KEYS_B_PS4)
            };
            let key_a = &keys_a[(nonce[0] >> 3) as usize];
            let mut ambassador = [0u8; 16];
            for i in 0..16 {
                let mut v = nonce[i];
                if target.is_ps5() {
                    v = v.wrapping_sub(0x2d);
                    v = v.wrapping_sub(i as u8);
                } else {
                    v = v.wrapping_add(0x36);
                    v = v.wrapping_add(i as u8);
                }
                v ^= key_a[i];
                ambassador[i] = v;
            }
            let key_b = &keys_b[(nonce[7] >> 3) as usize];
            let mut bright = [0u8; 16];
            if target.is_ps5() {
                for i in 0..16 {
                    let mut v = morning[i];
                    v = v.wrapping_add(0x18);
                    v = v.wrapping_add(i as u8);
                    v ^= nonce[i];
                    v ^= key_b[i];
                    bright[i] = v;
                }
            } else {
                for i in 0..16 {
                    let mut v = key_b[i] ^ morning[i];
                    v = v.wrapping_add(0x21);
                    v = v.wrapping_add(i as u8);
                    v ^= nonce[i];
                    bright[i] = v;
                }
            }
            Ok((bright, ambassador))
        }
    }
}

pub fn aeropause_ps4_pre10(ambassador: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        let mut v = ambassador[i];
        v = v.wrapping_sub(i as u8);
        v = v.wrapping_sub(0x29);
        v ^= ECHO_B[i];
        out[i] = v;
    }
    out
}

pub fn aeropause(target: Target, key_1_off: usize, ambassador: &[u8; 16]) -> Result<[u8; 16]> {
    if target < Target::Ps4_10 {
        return Err(Error::InvalidData);
    }
    let keys_1 = if target.is_ps5() {
        &PS5_KEYS_1
    } else {
        &PS4_KEYS_1
    };
    let wurzelbert: u8 = if target.is_ps5() { 0xd3 } else { 0x29 }; // -0x2d as u8
    let mut out = [0u8; 16];
    for i in 0..16 {
        let k = keys_1[i][key_1_off];
        out[i] = (ambassador[i] ^ k)
            .wrapping_add(wurzelbert)
            .wrapping_add(i as u8);
    }
    Ok(out)
}
