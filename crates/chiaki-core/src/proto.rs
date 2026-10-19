// SPDX-License-Identifier: AGPL-3.0-only
//! Tiny protobuf wire-format helpers for the handful of Takion messages we
//! need. No code generation or schema files.

use crate::error::{Error, Result};

#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.buf.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        self.buf.push(value as u8);
    }

    pub fn tag(&mut self, field: u32, wire: u8) {
        self.varint(((field as u64) << 3) | wire as u64);
    }

    pub fn uint32(&mut self, field: u32, value: u32) {
        self.tag(field, 0);
        self.varint(value as u64);
    }

    pub fn bool(&mut self, field: u32, value: bool) {
        self.tag(field, 0);
        self.varint(value as u64);
    }

    pub fn bytes(&mut self, field: u32, value: &[u8]) {
        self.tag(field, 2);
        self.varint(value.len() as u64);
        self.buf.extend_from_slice(value);
    }

    pub fn string(&mut self, field: u32, value: &str) {
        self.bytes(field, value.as_bytes());
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self.buf.get(self.pos).ok_or(Error::BufferTooSmall)?;
            self.pos += 1;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 64 {
                return Err(Error::InvalidData);
            }
        }
    }

    /// Read the next field. Returns `None` at the end of the buffer.
    pub fn next_field(&mut self) -> Result<Option<(u32, Value<'a>)>> {
        if self.done() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let wire = (key & 7) as u8;
        if field == 0 {
            return Err(Error::InvalidData);
        }
        let value = match wire {
            0 => Value::Varint(self.varint()?),
            1 => {
                let end = self.pos.checked_add(8).ok_or(Error::BufferTooSmall)?;
                let s = self.buf.get(self.pos..end).ok_or(Error::BufferTooSmall)?;
                self.pos = end;
                let mut v = [0u8; 8];
                v.copy_from_slice(s);
                Value::Fixed64(v)
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| Error::BufferTooSmall)?;
                let end = self.pos.checked_add(len).ok_or(Error::BufferTooSmall)?;
                let s = self.buf.get(self.pos..end).ok_or(Error::BufferTooSmall)?;
                self.pos = end;
                Value::Bytes(s)
            }
            5 => {
                let end = self.pos.checked_add(4).ok_or(Error::BufferTooSmall)?;
                let s = self.buf.get(self.pos..end).ok_or(Error::BufferTooSmall)?;
                self.pos = end;
                let mut v = [0u8; 4];
                v.copy_from_slice(s);
                Value::Fixed32(v)
            }
            _ => return Err(Error::InvalidData),
        };
        Ok(Some((field, value)))
    }

    /// Skip all remaining fields, validating the wire format.
    pub fn skip_all(&mut self) -> Result<()> {
        while self.next_field()?.is_some() {}
        Ok(())
    }
}

pub fn as_u32(value: Value<'_>) -> Result<u32> {
    match value {
        Value::Varint(v) => u32::try_from(v).map_err(|_| Error::InvalidData),
        _ => Err(Error::InvalidData),
    }
}

pub fn as_bool(value: Value<'_>) -> Result<bool> {
    match value {
        Value::Varint(v) => Ok(v != 0),
        _ => Err(Error::InvalidData),
    }
}

pub fn as_bytes(value: Value<'_>) -> Result<&[u8]> {
    match value {
        Value::Bytes(v) => Ok(v),
        _ => Err(Error::InvalidData),
    }
}

pub fn as_string(value: Value<'_>) -> Result<String> {
    String::from_utf8(as_bytes(value)?.to_vec()).map_err(|_| Error::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut w = Writer::new();
        w.uint32(1, 12);
        w.string(2, "hello");
        w.bool(3, true);
        w.bytes(4, &[1, 2, 3]);
        let data = w.finish();
        let mut r = Reader::new(&data);
        assert_eq!(r.next_field().unwrap(), Some((1, Value::Varint(12))));
        assert_eq!(r.next_field().unwrap(), Some((2, Value::Bytes(b"hello"))));
        assert_eq!(r.next_field().unwrap(), Some((3, Value::Varint(1))));
        assert_eq!(r.next_field().unwrap(), Some((4, Value::Bytes(&[1, 2, 3]))));
        assert!(r.next_field().unwrap().is_none());
    }

    #[test]
    fn long_varint() {
        let mut w = Writer::new();
        w.uint32(7, u32::MAX);
        let data = w.finish();
        let mut r = Reader::new(&data);
        assert_eq!(
            r.next_field().unwrap(),
            Some((7, Value::Varint(u32::MAX as u64)))
        );
    }

    #[test]
    fn oversized_length_is_rejected_without_overflow() {
        // Field 1, wire type 2, followed by a length varint of u64::MAX.
        let data = [
            0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ];
        let mut r = Reader::new(&data);
        assert!(r.next_field().is_err());
    }
}
