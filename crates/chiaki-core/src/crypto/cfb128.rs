// SPDX-License-Identifier: AGPL-3.0-only
//! AES-128-CFB128 with a 128-bit feedback block, matching OpenSSL's
//! `EVP_aes_128_cfb128()` (no padding).

use aes::Aes128;
use cfb_mode::{Decryptor, Encryptor};
use cipher::{AsyncStreamCipher, KeyIvInit};

use crate::error::{Error, Result};

pub fn encrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) -> Result<()> {
    let cipher = Encryptor::<Aes128>::new_from_slices(key, iv).map_err(|_| Error::InvalidData)?;
    cipher.encrypt(data);
    Ok(())
}

pub fn decrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) -> Result<()> {
    let cipher = Decryptor::<Aes128>::new_from_slices(key, iv).map_err(|_| Error::InvalidData)?;
    cipher.decrypt(data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let mut data: Vec<u8> = (0..80).map(|i| i as u8).collect();
        let mut enc = data.clone();
        encrypt(&key, &iv, &mut enc).unwrap();
        assert_ne!(enc, data);
        decrypt(&key, &iv, &mut enc).unwrap();
        assert_eq!(enc, data);
        data.clear();
    }
}
