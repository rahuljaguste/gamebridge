// SPDX-License-Identifier: AGPL-3.0-only
//! Base64 compatible with the reference implementation: standard alphabet,
//! padded output, lenient decoding (whitespace skipped, padding optional).

use crate::error::{Error, Result};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (chunks, remainder) = input.as_chunks::<3>();
    for c in chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        out.push(ALPHABET[(n & 63) as usize] as char);
    }
    match remainder {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn decode(input: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut count = 0u8;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        let v = value(c).ok_or(Error::InvalidData)?;
        buf = (buf << 6) | v as u32;
        count += 1;
        if count == 4 {
            out.push((buf >> 16) as u8);
            out.push((buf >> 8) as u8);
            out.push(buf as u8);
            buf = 0;
            count = 0;
        }
    }
    match count {
        0 => {}
        2 => out.push((buf >> 4) as u8),
        3 => {
            out.push((buf >> 10) as u8);
            out.push((buf >> 2) as u8);
        }
        _ => return Err(Error::InvalidData),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for n in 0..40 {
            let data: Vec<u8> = (0..n).map(|i| (i * 17 + 3) as u8).collect();
            let enc = encode(&data);
            assert_eq!(decode(&enc).unwrap(), data);
        }
    }

    #[test]
    fn unpadded_and_whitespace() {
        assert_eq!(decode("AAEC\n").unwrap(), vec![0, 1, 2]);
        assert_eq!(decode("AAE").unwrap(), vec![0, 1]);
        assert_eq!(decode("AA").unwrap(), vec![0]);
    }

    #[test]
    fn rejects_invalid() {
        assert!(decode("!!!!").is_err());
    }
}
