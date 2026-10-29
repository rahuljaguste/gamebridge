// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Minimal HTTP/1.1 response parsing. The console sends `HTTP/1.1` responses
//! with either CRLF or bare LF line endings.

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct HttpResponse {
    pub code: i32,
    pub headers: Vec<HttpHeader>,
}

impl HttpResponse {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
    }

    pub fn get_ci(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case(key))
            .map(|h| h.value.as_str())
    }
}

/// Parse a bare header block (no status line).
pub fn parse_headers(buf: &[u8]) -> Result<Vec<HttpHeader>> {
    let mut headers = Vec::new();
    let text = String::from_utf8_lossy(buf);
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            // Tolerate a stray blank/leading line; anything else is invalid.
            if line.trim().is_empty() {
                continue;
            }
            return Err(Error::InvalidData);
        };
        if key.is_empty() {
            return Err(Error::InvalidData);
        }
        let value = value.strip_prefix(' ').unwrap_or(value);
        if value.is_empty() {
            return Err(Error::InvalidData);
        }
        headers.push(HttpHeader {
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    Ok(headers)
}

/// Parse a response status line and the following headers.
pub fn parse_response(buf: &[u8]) -> Result<HttpResponse> {
    let text = String::from_utf8_lossy(buf);
    let first_end = text.find('\n').ok_or(Error::InvalidData)?;
    let status_line = text[..first_end]
        .strip_suffix('\r')
        .unwrap_or(&text[..first_end]);
    let rest = &text[first_end + 1..];
    if !status_line.starts_with("HTTP/1.1 ") {
        return Err(Error::InvalidData);
    }
    let code_str = status_line[9..].split_whitespace().next().unwrap_or("");
    let code: i32 = code_str.parse().map_err(|_| Error::InvalidData)?;
    if code == 0 {
        return Err(Error::InvalidData);
    }
    let headers = parse_headers(rest.as_bytes())?;
    Ok(HttpResponse { code, headers })
}

/// Read until the end of an HTTP header block. Returns the header length;
/// `buf` retains any bytes read past the header (needed by the ctrl channel).
pub async fn read_http_header<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut BytesMut,
    max_size: usize,
) -> Result<usize> {
    let mut search_from = buf.len().saturating_sub(3);
    loop {
        if let Some(pos) = find_header_end(&buf[search_from..]) {
            return Ok(search_from + pos);
        }
        if buf.len() >= max_size {
            return Err(Error::BufferTooSmall);
        }
        let mut chunk = [0u8; 4096];
        let n = reader.read(&mut chunk).await.map_err(|_| Error::Network)?;
        if n == 0 {
            return Err(Error::Disconnected);
        }
        buf.extend_from_slice(&chunk[..n]);
        search_from = buf.len().saturating_sub(n + 3);
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    // Return the index just past the terminating newline.
    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some(pos + 4);
    }
    if let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
        return Some(pos + 2);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_crlf() {
        let r = parse_response(
            b"HTTP/1.1 200 OK\r\nContent-type: text/html, text, plain\r\nUltimate Ability: Gamer\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.code, 200);
        assert_eq!(r.get("Ultimate Ability"), Some("Gamer"));
        assert_eq!(r.get("Content-type"), Some("text/html, text, plain"));
    }

    #[test]
    fn parse_lf() {
        let r = parse_response(
            b"HTTP/1.1 200 Ok\nContent-type: text/html, text, plain\nUltimate Ability:Gamer\n",
        )
        .unwrap();
        assert_eq!(r.code, 200);
        assert_eq!(r.get("Ultimate Ability"), Some("Gamer"));
        assert_eq!(r.get("Content-type"), Some("text/html, text, plain"));
    }

    #[test]
    fn rejects_bad_status() {
        assert!(parse_response(b"HTTP/1.0 200 OK\r\n\r\n").is_err());
        assert!(parse_response(b"garbage\r\n").is_err());
    }

    #[tokio::test]
    async fn read_header_keeps_remainder() {
        let data = b"HTTP/1.1 200 OK\r\nA: b\r\n\r\npayload";
        let mut cursor = std::io::Cursor::new(data);
        let mut buf = BytesMut::new();
        let n = read_http_header(&mut cursor, &mut buf, 4096).await.unwrap();
        assert_eq!(&buf[..n], b"HTTP/1.1 200 OK\r\nA: b\r\n\r\n");
        assert_eq!(&buf[n..], b"payload");
    }
}
