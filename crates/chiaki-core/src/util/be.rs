// SPDX-License-Identifier: AGPL-3.0-only
//! Constant-time helpers shared by the wire parsers.

/// Compare two 4-byte MACs in constant time.
#[inline]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}
