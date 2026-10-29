// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! GKCrypt: the Takion media cipher.
//!
//! AES-128-CTR style keystream (ECB encrypting an incrementing IV), plus an
//! AES-128-GCM tag truncated to four bytes. The GCM key is refreshed every
//! 45,000 key positions.

use aes::Aes128;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{AesGcm, Nonce};
use cipher::BlockEncrypt;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::crypto::ecdh::CHIAKI_HANDSHAKE_KEY_SIZE;
use crate::error::{Error, Result};

type HmacSha256 = Hmac<Sha256>;
type Aes128Ecb = aes::Aes128;
type Gcm16 = AesGcm<Aes128, aes_gcm::aead::consts::U16>;

pub const GKCRYPT_BLOCK_SIZE: usize = 0x10;
pub const GKCRYPT_GMAC_SIZE: usize = 4;
pub const GKCRYPT_GMAC_KEY_REFRESH_KEY_POS: u64 = 45_000;
pub const GKCRYPT_GMAC_KEY_REFRESH_IV_OFFSET: u64 = 44_910;
pub const ECDH_SECRET_SIZE: usize = 32;

#[derive(Clone)]
pub struct GKCrypt {
    pub index: u8,
    pub iv: [u8; 16],
    pub key_base: [u8; 16],
    pub key_gmac_base: [u8; 16],
    pub key_gmac_current: [u8; 16],
    pub key_gmac_index_current: u64,
}

impl std::fmt::Debug for GKCrypt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GKCrypt")
            .field("index", &self.index)
            .field("key_gmac_index_current", &self.key_gmac_index_current)
            .finish_non_exhaustive()
    }
}

impl GKCrypt {
    pub fn new(
        index: u8,
        handshake_key: &[u8; CHIAKI_HANDSHAKE_KEY_SIZE],
        ecdh_secret: &[u8; ECDH_SECRET_SIZE],
    ) -> Result<Self> {
        let mut data = [0u8; 3 + CHIAKI_HANDSHAKE_KEY_SIZE + 2];
        data[0] = 1;
        data[1] = index;
        data[2] = 0;
        data[3..3 + CHIAKI_HANDSHAKE_KEY_SIZE].copy_from_slice(handshake_key);
        data[3 + CHIAKI_HANDSHAKE_KEY_SIZE] = 1;
        data[3 + CHIAKI_HANDSHAKE_KEY_SIZE + 1] = 0;

        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(ecdh_secret).expect("hmac accepts any key len");
        mac.update(&data);
        let digest = mac.finalize().into_bytes();
        let mut key_base = [0u8; 16];
        let mut iv = [0u8; 16];
        key_base.copy_from_slice(&digest[..16]);
        iv.copy_from_slice(&digest[16..32]);
        let key_gmac_base = Self::gen_gmac_key(0, &key_base, &iv);
        Ok(Self {
            index,
            iv,
            key_base,
            key_gmac_base,
            key_gmac_current: key_gmac_base,
            key_gmac_index_current: 0,
        })
    }

    /// C-style little-endian counter addition into a 16-byte block.
    pub fn counter_add(out: &mut [u8; 16], base: &[u8; 16], v: u64) {
        out.copy_from_slice(base);
        let mut carry = v;
        for b in out.iter_mut() {
            if carry == 0 {
                break;
            }
            let r = (*b as u64) + (carry & 0xff);
            *b = (r & 0xff) as u8;
            carry = (carry >> 8) + (r >> 8);
        }
    }

    pub fn gen_gmac_key(index: u64, key_base: &[u8; 16], iv: &[u8; 16]) -> [u8; 16] {
        let mut data = [0u8; 0x20];
        data[..16].copy_from_slice(key_base);
        let mut ctr = [0u8; 16];
        Self::counter_add(
            &mut ctr,
            iv,
            index.wrapping_mul(GKCRYPT_GMAC_KEY_REFRESH_IV_OFFSET),
        );
        data[16..].copy_from_slice(&ctr);
        let md = Sha256::digest(data);
        let mut out = [0u8; 16];
        for i in 0..16 {
            out[i] = md[i] ^ md[i + 16];
        }
        out
    }

    pub fn gen_new_gmac_key(&mut self, index: u64) {
        debug_assert!(index > 0);
        self.key_gmac_current = Self::gen_gmac_key(index, &self.key_gmac_base, &self.iv);
        self.key_gmac_index_current = index;
    }

    pub fn gen_tmp_gmac_key(&self, index: u64) -> [u8; 16] {
        if index > 0 {
            Self::gen_gmac_key(index, &self.key_gmac_base, &self.iv)
        } else {
            self.key_gmac_base
        }
    }

    /// Generate the keystream starting at an aligned key position.
    pub fn gen_key_stream(&self, key_pos: u64, buf: &mut [u8]) -> Result<()> {
        if !key_pos.is_multiple_of(16) || !buf.len().is_multiple_of(16) {
            return Err(Error::InvalidData);
        }
        let cipher = Aes128Ecb::new_from_slice(&self.key_base).map_err(|_| Error::Unknown)?;
        let mut counter_offset = key_pos / 16;
        for block in buf.as_chunks_mut::<16>().0 {
            let mut ctr = [0u8; 16];
            Self::counter_add(&mut ctr, &self.iv, counter_offset);
            counter_offset = counter_offset.wrapping_add(1);
            block.copy_from_slice(&ctr);
            let mut b = cipher::generic_array::GenericArray::clone_from_slice(block);
            cipher.encrypt_block(&mut b);
            block.copy_from_slice(&b);
        }
        Ok(())
    }

    /// XOR a buffer with the keystream at an arbitrary key position.
    /// This is symmetric: it performs both encryption and decryption.
    ///
    /// The keystream is generated block by block into a stack buffer; media
    /// packets take this path, so it must not allocate per packet.
    pub fn crypt(&self, key_pos: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let cipher = Aes128Ecb::new_from_slice(&self.key_base).map_err(|_| Error::Unknown)?;
        let padding_pre = (key_pos % 16) as usize;
        let mut counter_offset = (key_pos - padding_pre as u64) / 16;
        let mut pos = 0usize;
        let mut skip = padding_pre;
        while pos < buf.len() {
            let mut ctr = [0u8; 16];
            Self::counter_add(&mut ctr, &self.iv, counter_offset);
            counter_offset = counter_offset.wrapping_add(1);
            let mut block = cipher::generic_array::GenericArray::clone_from_slice(&ctr);
            cipher.encrypt_block(&mut block);
            let take = (16 - skip).min(buf.len() - pos);
            for i in 0..take {
                buf[pos + i] ^= block[skip + i];
            }
            pos += take;
            skip = 0;
        }
        Ok(())
    }

    pub fn decrypt(&self, key_pos: u64, buf: &mut [u8]) -> Result<()> {
        self.crypt(key_pos, buf)
    }

    pub fn encrypt(&self, key_pos: u64, buf: &mut [u8]) -> Result<()> {
        self.crypt(key_pos, buf)
    }

    pub fn gmac(&mut self, key_pos: u64, buf: &[u8]) -> Result<[u8; 4]> {
        let mut iv = [0u8; 16];
        Self::counter_add(&mut iv, &self.iv, key_pos / 16);

        let key_index = if key_pos > 0 {
            (key_pos - 1) / GKCRYPT_GMAC_KEY_REFRESH_KEY_POS
        } else {
            0
        };
        let tmp_key;
        let key = if key_index > self.key_gmac_index_current {
            self.gen_new_gmac_key(key_index);
            &self.key_gmac_current
        } else if key_index < self.key_gmac_index_current {
            tmp_key = self.gen_tmp_gmac_key(key_index);
            &tmp_key
        } else {
            &self.key_gmac_current
        };

        let cipher = Gcm16::new_from_slice(key).map_err(|_| Error::Unknown)?;
        let nonce = Nonce::from_slice(&iv);
        let mut empty: [u8; 0] = [];
        let tag = cipher
            .encrypt_in_place_detached(nonce, buf, &mut empty)
            .map_err(|_| Error::Unknown)?;
        let mut out = [0u8; 4];
        out.copy_from_slice(&tag[..4]);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_add_little_endian_carry() {
        let mut base = [0u8; 16];
        base[0] = 0xff;
        let mut out = [0u8; 16];
        GKCrypt::counter_add(&mut out, &base, 1);
        assert_eq!(out[0], 0x00);
        assert_eq!(out[1], 0x01);
        assert_eq!(out[15], 0x00);
        let base = [0u8; 16];
        let mut out = [0u8; 16];
        GKCrypt::counter_add(&mut out, &base, 0x0102_0304_0506_0708);
        assert_eq!(
            out,
            [
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
    }
}
