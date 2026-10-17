// SPDX-License-Identifier: AGPL-3.0-only
//! Bounds-checked big/little endian helpers used by the wire parsers.

use crate::error::{Error, Result};

#[inline]
pub fn u16be(buf: &[u8], off: usize) -> Result<u16> {
    let s = buf.get(off..off + 2).ok_or(Error::BufferTooSmall)?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

#[inline]
pub fn u32be(buf: &[u8], off: usize) -> Result<u32> {
    let s = buf.get(off..off + 4).ok_or(Error::BufferTooSmall)?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

#[inline]
pub fn u64be(buf: &[u8], off: usize) -> Result<u64> {
    let s = buf.get(off..off + 8).ok_or(Error::BufferTooSmall)?;
    Ok(u64::from_be_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

#[inline]
pub fn u16le(buf: &[u8], off: usize) -> Result<u16> {
    let s = buf.get(off..off + 2).ok_or(Error::BufferTooSmall)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

#[inline]
pub fn u32le(buf: &[u8], off: usize) -> Result<u32> {
    let s = buf.get(off..off + 4).ok_or(Error::BufferTooSmall)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[inline]
pub fn put_u16be(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_be_bytes());
}

#[inline]
pub fn put_u32be(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

#[inline]
pub fn put_u16le(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

#[inline]
pub fn put_u32le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// Compare two 4-byte MACs in constant time.
#[inline]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}
