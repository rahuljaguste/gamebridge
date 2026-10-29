// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! ECDH over secp256k1 with an HMAC-SHA256 signature over the public key.
//! The shared secret is the raw x-coordinate, matching OpenSSL's
//! `ECDH_compute_key` used by the reference implementation.

use hmac::{Hmac, Mac};
use secp256k1::ecdh::shared_secret_point;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};

type HmacSha256 = Hmac<Sha256>;

pub const CHIAKI_HANDSHAKE_KEY_SIZE: usize = 0x10;
pub const CHIAKI_ECDH_SECRET_SIZE: usize = 32;

pub struct Ecdh {
    secret: SecretKey,
    public: PublicKey,
}

impl Ecdh {
    pub fn new() -> Result<Self> {
        let secp = Secp256k1::new();
        let secret = SecretKey::new(&mut rand::thread_rng());
        let public = PublicKey::from_secret_key(&secp, &secret);
        Ok(Self { secret, public })
    }

    /// Test/restore constructor. The public key is always derived from the
    /// private key, matching the reference behavior.
    pub fn from_private(private_key: &[u8]) -> Result<Self> {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(private_key).map_err(|_| Error::InvalidData)?;
        let public = PublicKey::from_secret_key(&secp, &secret);
        Ok(Self { secret, public })
    }

    pub fn public_key(&self) -> [u8; 65] {
        self.public.serialize_uncompressed()
    }

    /// HMAC-SHA256(handshake_key, public_key)
    pub fn sign_public_key(&self, handshake_key: &[u8; 16]) -> [u8; 32] {
        let pubkey = self.public_key();
        let mut mac = <HmacSha256 as Mac>::new_from_slice(handshake_key).expect("hmac key size");
        mac.update(&pubkey);
        let digest = mac.finalize().into_bytes();
        let mut sig = [0u8; 32];
        sig.copy_from_slice(&digest);
        sig
    }

    /// Derive the 32-byte shared secret. If `verify_signature` is set, the
    /// peer's HMAC over its public key must match `remote_sig`.
    pub fn derive_secret(
        &self,
        remote_key: &[u8],
        handshake_key: &[u8; 16],
        remote_sig: &[u8],
        verify_signature: bool,
    ) -> Result<[u8; 32]> {
        let peer = PublicKey::from_slice(remote_key).map_err(|_| Error::InvalidData)?;
        if verify_signature {
            let mut mac =
                <HmacSha256 as Mac>::new_from_slice(handshake_key).expect("hmac key size");
            mac.update(remote_key);
            let expected = mac.finalize().into_bytes();
            if expected.len() != remote_sig.len()
                || !bool::from(expected.as_slice().ct_eq(remote_sig))
            {
                return Err(Error::InvalidData);
            }
        }
        let point = shared_secret_point(&peer, &self.secret);
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&point[..32]);
        Ok(secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDSHAKE_KEY: [u8; 16] = [
        0xfc, 0x5d, 0x4b, 0xa0, 0x3a, 0x35, 0x3a, 0xbb, 0x6a, 0x7f, 0xac, 0x79, 0x1b, 0x17, 0xbb,
        0x34,
    ];
    const LOCAL_PRIVATE_KEY: [u8; 32] = [
        0x16, 0xe7, 0x5d, 0xcb, 0xda, 0x98, 0x55, 0xfb, 0x6b, 0xef, 0xdd, 0x8a, 0xa5, 0xf1, 0x6e,
        0x7f, 0x46, 0xfd, 0xe1, 0xd2, 0x27, 0x97, 0x03, 0x60, 0x18, 0x72, 0xd8, 0x4b, 0x15, 0x38,
        0xd9, 0x00,
    ];
    const LOCAL_PUBLIC_KEY: [u8; 65] = [
        0x04, 0xf4, 0x0a, 0xf1, 0x35, 0xa4, 0x88, 0x94, 0x36, 0xce, 0xe5, 0x2b, 0x5c, 0x73, 0xa3,
        0x3e, 0xc5, 0xad, 0x0b, 0xe0, 0x95, 0x2f, 0x57, 0xf4, 0xf0, 0xed, 0x0c, 0x80, 0xb0, 0xbe,
        0xda, 0x7c, 0xa6, 0x43, 0x78, 0x93, 0x93, 0xa5, 0x94, 0x7e, 0x9f, 0xaa, 0x3f, 0x67, 0x95,
        0xc9, 0xaa, 0x09, 0xa9, 0x63, 0x25, 0xdf, 0xe8, 0x50, 0xbf, 0xc3, 0xf1, 0xdb, 0x62, 0xa5,
        0x0a, 0xbf, 0xb0, 0xff, 0xf7,
    ];
    const LOCAL_PUBLIC_KEY_SIG: [u8; 32] = [
        0x99, 0xb5, 0xcb, 0xb5, 0x37, 0x18, 0x0b, 0xfc, 0x55, 0xda, 0x43, 0x7f, 0x44, 0x76, 0xa8,
        0x17, 0xc9, 0x37, 0xfe, 0x56, 0x1b, 0x8a, 0xbe, 0x0c, 0x41, 0x12, 0xab, 0x71, 0xf5, 0xa6,
        0x8d, 0x29,
    ];
    const REMOTE_PUBLIC_KEY: [u8; 65] = [
        0x04, 0xdf, 0xef, 0x08, 0xbb, 0xa8, 0x56, 0xf2, 0xb4, 0x4b, 0x8a, 0x0e, 0x4f, 0x44, 0x20,
        0x3f, 0x8e, 0x49, 0x3f, 0xee, 0xd4, 0x3c, 0xe9, 0x3a, 0xfe, 0x5c, 0x64, 0x67, 0x77, 0x20,
        0x15, 0x7c, 0x59, 0x10, 0x15, 0x67, 0x94, 0xae, 0x5f, 0x02, 0x4a, 0xad, 0x0c, 0xce, 0xfa,
        0x14, 0x15, 0x0a, 0xab, 0xee, 0x08, 0x0b, 0x14, 0x12, 0x76, 0xea, 0x3e, 0xc0, 0xd5, 0x65,
        0xf4, 0x68, 0x77, 0xa3, 0xca,
    ];
    const REMOTE_PUBLIC_KEY_SIG: [u8; 32] = [
        0x13, 0xc5, 0x89, 0xe2, 0x3b, 0x72, 0x85, 0x24, 0xa9, 0x9f, 0x96, 0x80, 0x03, 0xa1, 0x81,
        0x30, 0x59, 0x68, 0xf1, 0xbb, 0xb6, 0x4d, 0xc4, 0xa7, 0x6c, 0xce, 0xf6, 0x79, 0x4c, 0xeb,
        0x2d, 0x98,
    ];
    const SECRET: [u8; 32] = [
        0xb8, 0x1c, 0x61, 0x46, 0xe7, 0x49, 0x73, 0x8c, 0x96, 0x30, 0xca, 0x13, 0xff, 0x71, 0xe5,
        0x9b, 0x3b, 0xf9, 0x41, 0x98, 0xd4, 0x67, 0xa5, 0xa2, 0xbc, 0x78, 0x04, 0x92, 0x81, 0x43,
        0xec, 0x1d,
    ];

    #[test]
    fn ecdh_vector() {
        let ecdh = Ecdh::from_private(&LOCAL_PRIVATE_KEY).unwrap();
        assert_eq!(ecdh.public_key(), LOCAL_PUBLIC_KEY);
        assert_eq!(ecdh.sign_public_key(&HANDSHAKE_KEY), LOCAL_PUBLIC_KEY_SIG);
        let secret = ecdh
            .derive_secret(
                &REMOTE_PUBLIC_KEY,
                &HANDSHAKE_KEY,
                &REMOTE_PUBLIC_KEY_SIG,
                true,
            )
            .unwrap();
        assert_eq!(secret, SECRET);
    }

    #[test]
    fn rejects_bad_signature() {
        let ecdh = Ecdh::from_private(&LOCAL_PRIVATE_KEY).unwrap();
        let mut sig = REMOTE_PUBLIC_KEY_SIG;
        sig[0] ^= 1;
        assert!(
            ecdh.derive_secret(&REMOTE_PUBLIC_KEY, &HANDSHAKE_KEY, &sig, true)
                .is_err()
        );
        // The bypass must still work.
        assert!(
            ecdh.derive_secret(&REMOTE_PUBLIC_KEY, &HANDSHAKE_KEY, &sig, false)
                .is_ok()
        );
    }
}
