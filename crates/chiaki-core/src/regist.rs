// SPDX-License-Identifier: AGPL-3.0-only
//! Console registration (PIN pairing) and the registration search handshake.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio::time::{Instant, timeout};

use crate::crypto::rpcrypt::{RPCrypt, aeropause, aeropause_ps4_pre10};
use crate::error::{Error, Result, rp_application_reason_string};
use crate::http;
use crate::target::Target;
use crate::util::{base64, rng};

pub const PSN_ACCOUNT_ID_SIZE: usize = 8;

/// Parse a PSN Account ID typed by the user into its 8 raw bytes.
///
/// Accepted forms:
/// - the decimal account number (as shown in PSN account info)
/// - a hexadecimal number, either `0x`-prefixed or exactly 16 hex digits
///   containing at least one `a`-`f` (a purely decimal string is decimal)
/// - base64 of the 8 raw bytes, with or without padding
///
/// Sony represents the account number as an 8-byte little-endian integer;
/// decimal and hex input are converted the same way Chiaki's
/// `psn-account-id.py` does (`int(user_id).to_bytes(8, "little")`).
pub fn parse_psn_account_id(input: &str) -> Result<[u8; PSN_ACCOUNT_ID_SIZE]> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::InvalidData);
    }

    // Decimal account number.
    if input.bytes().all(|b| b.is_ascii_digit()) {
        let value: u64 = input.parse().map_err(|_| Error::InvalidData)?;
        return Ok(value.to_le_bytes());
    }

    // Hex number. Unprefixed input must look like hex rather than decimal:
    // exactly 16 characters containing at least one a-f digit. Anything that
    // is all decimal digits was already handled above.
    let (hex, prefixed) = match input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        Some(h) => (h, true),
        None => (input, false),
    };
    let looks_hex =
        !hex.is_empty() && hex.len() <= 16 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    let has_hex_letter = hex
        .bytes()
        .any(|b| b.is_ascii_hexdigit() && !b.is_ascii_digit());
    if looks_hex && (prefixed || (input.len() == 16 && has_hex_letter)) {
        let value = u64::from_str_radix(hex, 16).map_err(|_| Error::InvalidData)?;
        return Ok(value.to_le_bytes());
    }

    // Base64 of the raw 8 bytes.
    if let Ok(bytes) = base64::decode(input)
        && bytes.len() == PSN_ACCOUNT_ID_SIZE
    {
        let mut out = [0u8; PSN_ACCOUNT_ID_SIZE];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }

    Err(Error::InvalidData)
}

const REGIST_PORT: u16 = 9295;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const INNER_HEADER_OFF: usize = 0x1e0;

#[derive(Debug, Clone)]
pub struct RegistInfo {
    pub target: Target,
    pub host: String,
    pub broadcast: bool,
    /// Used instead of the account id for PS4 < 7.0.
    pub psn_online_id: Option<String>,
    /// Raw 8-byte PSN account id, as produced by
    /// [`parse_psn_account_id`].
    pub psn_account_id: Option<[u8; PSN_ACCOUNT_ID_SIZE]>,
    pub pin: u32,
}

#[derive(Debug, Clone, Default)]
pub struct RegisteredHost {
    pub target: Target,
    pub ap_ssid: String,
    pub ap_bssid: String,
    pub ap_key: String,
    pub ap_name: String,
    pub server_mac: [u8; 6],
    pub server_nickname: String,
    /// 16-byte RP regist key (wire form is hex).
    pub rp_regist_key: [u8; 16],
    pub rp_key_type: u32,
    /// 16-byte RP-Key, used as the "morning" value during session auth.
    pub rp_key: [u8; 16],
}

fn request_path(target: Target) -> &'static str {
    match target {
        Target::Ps5Unknown | Target::Ps5_1 => "/sie/ps5/rp/sess/rgst",
        Target::Ps4_8 | Target::Ps4_9 => "/sce/rp/regist",
        _ => "/sie/ps4/rp/sess/rgst",
    }
}

/// Build the registration POST payload. Public for vector tests.
pub fn build_payload(info: &RegistInfo, ambassador: &[u8; 16]) -> Result<(RPCrypt, Vec<u8>)> {
    let mut buf = vec![b'A'; 0x400];
    let crypt;
    let mut psn_online_id = info.psn_online_id.as_deref();

    if info.target < Target::Ps4_10 {
        crypt = RPCrypt::init_regist_ps4_pre10(ambassador, info.pin);
        let aero = aeropause_ps4_pre10(&crypt.ambassador);
        buf[0x11c..0x12c].copy_from_slice(&aero);
    } else {
        let key_0_off = (buf[0x18d] & 0x1f) as usize;
        let key_1_off = (buf[0] >> 3) as usize;
        crypt = RPCrypt::init_regist(info.target, ambassador, key_0_off, info.pin)?;
        let aero = aeropause(info.target, key_1_off, &crypt.ambassador)?;
        buf[0xc7..0xcf].copy_from_slice(&aero[8..16]);
        buf[0x191..0x199].copy_from_slice(&aero[0..8]);
        // Modern targets only use the account id.
        psn_online_id = None;
    }

    let inner = if let Some(online_id) = psn_online_id {
        format!("Client-Type: Windows\r\nNp-Online-Id: {online_id}\r\n")
    } else {
        let account_id = info.psn_account_id.ok_or(Error::InvalidData)?;
        let account_b64 = base64::encode(&account_id);
        let client_type = if info.target < Target::Ps4_10 {
            "Windows"
        } else {
            "dabfa2ec873de5839bee8d3f4c0239c4282c07c25c6077a2931afcf0adc0d34f"
        };
        format!("Client-Type: {client_type}\r\nNp-AccountId: {account_b64}\r\n")
    };

    let end = INNER_HEADER_OFF + inner.len();
    if end > buf.len() {
        return Err(Error::BufferTooSmall);
    }
    buf[INNER_HEADER_OFF..end].copy_from_slice(inner.as_bytes());
    crypt.encrypt(0, &mut buf[INNER_HEADER_OFF..end])?;
    buf.truncate(end);
    Ok((crypt, buf))
}

/// Format the registration request head (including its deliberate quirks).
pub fn format_request_header(info: &RegistInfo, payload_len: usize) -> String {
    let mut s = format!(
        "POST {} HTTP/1.1\r\n HTTP/1.1\r\nHOST: 10.0.2.15\r\nUser-Agent: remoteplay Windows\r\nConnection: close\r\nContent-Length: {payload_len}\r\n",
        request_path(info.target)
    );
    if info.target >= Target::Ps4_9
        && let Some(version) = info.target.rp_version()
    {
        s.push_str(&format!("RP-Version: {version}\r\n"));
    }
    s.push_str("\r\n");
    s
}

async fn resolve_ipv4(host: &str) -> Result<SocketAddr> {
    let mut addrs = lookup_host((host, REGIST_PORT))
        .await
        .map_err(|_| Error::ParseAddr)?;
    addrs.find(|a| a.is_ipv4()).ok_or(Error::ParseAddr)
}

async fn search(info: &RegistInfo, host: SocketAddr) -> Result<SocketAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|_| Error::Network)?;
    let src: &[u8] = if info.target.is_ps5() {
        b"SRC3\0"
    } else {
        b"SRC2\0"
    };
    let res: &[u8] = if info.target.is_ps5() {
        b"RES3"
    } else {
        b"RES2"
    };
    let dest = if info.broadcast {
        sock.set_broadcast(true).map_err(|_| Error::Network)?;
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::BROADCAST), REGIST_PORT)
    } else {
        host
    };
    sock.send_to(src, dest).await.map_err(|_| Error::Network)?;

    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let mut buf = [0u8; 0x100];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::Timeout);
        }
        let received = timeout(deadline - now, sock.recv_from(&mut buf))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Network)?;
        let (n, peer) = received;
        if n >= res.len() && buf[..res.len()] == *res {
            return Ok(peer);
        }
    }
}

fn parse_hex_padded(value: &str, out: &mut [u8]) -> Result<usize> {
    if !value.len().is_multiple_of(2) {
        return Err(Error::InvalidData);
    }
    let bytes = hex::decode(value).map_err(|_| Error::InvalidData)?;
    if bytes.len() > out.len() {
        return Err(Error::BufferTooSmall);
    }
    out.fill(0);
    out[..bytes.len()].copy_from_slice(&bytes);
    Ok(bytes.len())
}

fn parse_hex_exact<const N: usize>(value: &str) -> Result<[u8; N]> {
    let bytes = hex::decode(value).map_err(|_| Error::InvalidData)?;
    if bytes.len() != N {
        return Err(Error::InvalidData);
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub(crate) fn parse_response_payload(target: Target, payload: &[u8]) -> Result<RegisteredHost> {
    let headers = http::parse_headers(payload)?;
    let ps5 = target.is_ps5();
    let mut host = RegisteredHost {
        target,
        ..Default::default()
    };
    let mut regist_key_found = false;
    let mut key_found = false;
    let mut mac_found = false;

    for h in &headers {
        match h.key.as_str() {
            "AP-Ssid" => host.ap_ssid = h.value.clone(),
            "AP-Bssid" => host.ap_bssid = h.value.clone(),
            "AP-Key" => host.ap_key = h.value.clone(),
            "AP-Name" => host.ap_name = h.value.clone(),
            "PS5-Nickname" if ps5 => host.server_nickname = h.value.clone(),
            "PS4-Nickname" if !ps5 => host.server_nickname = h.value.clone(),
            "PS5-RegistKey" if ps5 => {
                if parse_hex_padded(&h.value, &mut host.rp_regist_key).is_ok() {
                    regist_key_found = true;
                } else {
                    host.rp_regist_key = [0; 16];
                }
            }
            "PS4-RegistKey" if !ps5 => {
                if parse_hex_padded(&h.value, &mut host.rp_regist_key).is_ok() {
                    regist_key_found = true;
                } else {
                    host.rp_regist_key = [0; 16];
                }
            }
            "RP-KeyType" => {
                host.rp_key_type = h.value.parse().map_err(|_| Error::InvalidData)?;
            }
            "RP-Key" => {
                if let Ok(key) = parse_hex_exact::<16>(&h.value) {
                    host.rp_key = key;
                    key_found = true;
                }
            }
            "PS5-Mac" if ps5 => {
                if let Ok(mac) = parse_hex_exact::<6>(&h.value) {
                    host.server_mac = mac;
                    mac_found = true;
                }
            }
            "PS4-Mac" if !ps5 => {
                if let Ok(mac) = parse_hex_exact::<6>(&h.value) {
                    host.server_mac = mac;
                    mac_found = true;
                }
            }
            _ => {}
        }
    }

    if !regist_key_found || !key_found || !mac_found {
        let family = if ps5 { "PS5" } else { "PS4" };
        let mut missing = Vec::new();
        if !regist_key_found {
            missing.push(format!("{family}-RegistKey"));
        }
        if !key_found {
            missing.push("RP-Key".to_string());
        }
        if !mac_found {
            missing.push(format!("{family}-Mac"));
        }
        let other = if ps5 { "PS4" } else { "PS5" };
        let hint = if headers.iter().any(|h| h.key.starts_with(other)) {
            format!("; the console returned {other} headers, so pick {other} as the console type")
        } else {
            String::new()
        };
        tracing::warn!(
            "registration response headers: {:?}",
            headers.iter().map(|h| h.key.as_str()).collect::<Vec<_>>()
        );
        return Err(Error::Protocol(format!(
            "registration response is missing {}{hint}",
            missing.join(", ")
        )));
    }
    Ok(host)
}

/// Perform the complete registration flow.
pub async fn register(info: &RegistInfo) -> Result<RegisteredHost> {
    tracing::info!(
        "registering {:?} console at {} (broadcast: {})",
        info.target,
        info.host,
        info.broadcast
    );
    let mut ambassador = [0u8; 16];
    rng::fill(&mut ambassador);
    let (crypt, payload) = build_payload(info, &ambassador)?;
    let header = format_request_header(info, payload.len());
    let host = resolve_ipv4(&info.host).await?;

    let peer = search(info, host).await?;
    tracing::info!("registration search reached console at {peer}");
    // The console does not accept the TCP request immediately.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut tcp = timeout(RESPONSE_TIMEOUT, TcpStream::connect(peer))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::ConnectionRefused)?;
    tcp.set_nodelay(true).ok();
    tcp.write_all(header.as_bytes())
        .await
        .map_err(|_| Error::Network)?;
    tcp.write_all(&payload).await.map_err(|_| Error::Network)?;

    let mut buf = BytesMut::with_capacity(0x200);
    let header_size = timeout(
        RESPONSE_TIMEOUT,
        http::read_http_header(&mut tcp, &mut buf, 0x200),
    )
    .await
    .map_err(|_| Error::Timeout)??;
    let response = http::parse_response(&buf[..header_size])?;
    if response.code != 200 {
        let reason = response.get("RP-Application-Reason").and_then(|reason| {
            let hex = reason
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            u32::from_str_radix(hex, 16).ok()
        });
        let detail = match reason {
            Some(code) => format!(
                "console rejected registration (HTTP {}): {} ({code:#x})",
                response.code,
                rp_application_reason_string(code)
            ),
            None => format!("console rejected registration with HTTP {}", response.code),
        };
        tracing::error!("{detail}");
        return Err(Error::Protocol(detail));
    }
    let content_size: usize = response
        .get_ci("Content-Length")
        .ok_or_else(|| Error::Protocol("console response is missing Content-Length".into()))?
        .trim()
        .parse()
        .map_err(|_| Error::Protocol("console response has an invalid Content-Length".into()))?;
    if content_size == 0 {
        return Err(Error::Protocol(
            "console response has an empty body (Content-Length: 0)".into(),
        ));
    }
    if header_size + content_size > 0x200 {
        return Err(Error::Protocol(format!(
            "console response is too large ({} body bytes)",
            content_size
        )));
    }
    while buf.len() < header_size + content_size {
        let want = header_size + content_size - buf.len();
        let mut chunk = vec![0u8; want];
        let n = timeout(
            RESPONSE_TIMEOUT,
            tokio::io::AsyncReadExt::read(&mut tcp, &mut chunk),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Network)?;
        if n == 0 {
            return Err(Error::Disconnected);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let mut body = buf[header_size..header_size + content_size].to_vec();
    crypt.decrypt(0, &mut body).map_err(|e| {
        Error::Protocol(format!(
            "could not decrypt the console registration response: {e}"
        ))
    })?;
    let registered = parse_response_payload(info.target, &body)?;
    tracing::info!(
        "registration succeeded: {:?} \"{}\"",
        registered.target,
        registered.server_nickname
    );
    Ok(registered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMBASSADOR: [u8; 16] = [
        0x13, 0x37, 0xde, 0xad, 0xbe, 0xef, 0xc0, 0xff, 0xee, 0x42, 0x63, 0x68, 0x69, 0x61, 0x6b,
        0x69,
    ];

    #[test]
    fn account_id_from_decimal() {
        // Matches Chiaki's psn-account-id.py: int(user_id).to_bytes(8, "little").
        assert_eq!(
            parse_psn_account_id("1234567890123456789").unwrap(),
            1234567890123456789u64.to_le_bytes()
        );
        assert_eq!(
            parse_psn_account_id(" 42 ").unwrap(),
            [42, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn account_id_from_hex() {
        assert_eq!(
            parse_psn_account_id("0x0102030405060708").unwrap(),
            0x0102_0304_0506_0708u64.to_le_bytes()
        );
        assert_eq!(
            parse_psn_account_id("01020304050607af").unwrap(),
            0x0102_0304_0506_07afu64.to_le_bytes()
        );
        assert_eq!(
            parse_psn_account_id("0x1").unwrap(),
            [1, 0, 0, 0, 0, 0, 0, 0]
        );
        // A purely decimal string is always decimal, even at hex length.
        assert_eq!(
            parse_psn_account_id("0102030405060708").unwrap(),
            102_030_405_060_708u64.to_le_bytes()
        );
    }

    #[test]
    fn account_id_from_base64() {
        let raw = [0x12, 0xab, 0x34, 0xcd, 0x56, 0xef, 0x78, 0x90];
        let padded = base64::encode(&raw);
        assert_eq!(parse_psn_account_id(&padded).unwrap(), raw);
        assert_eq!(
            parse_psn_account_id(padded.trim_end_matches('=')).unwrap(),
            raw
        );
    }

    #[test]
    fn account_id_rejects_invalid() {
        assert!(parse_psn_account_id("").is_err());
        assert!(parse_psn_account_id("hello").is_err());
        assert!(parse_psn_account_id("not a base64 id!").is_err());
        // u64 overflow.
        assert!(parse_psn_account_id("99999999999999999999").is_err());
        // Correct base64 but the wrong length.
        assert!(parse_psn_account_id(&base64::encode(&[0u8; 7])).is_err());
    }

    #[test]
    fn aeropause_vec() {
        let aero = aeropause_ps4_pre10(&AMBASSADOR);
        assert_eq!(
            aero,
            [
                0x0b, 0xe1, 0x2f, 0xbb, 0x4c, 0x7c, 0x99, 0x4a, 0x41, 0x1e, 0x2d, 0x4c, 0xa4, 0x19,
                0xf4, 0x35
            ]
        );
    }

    #[test]
    fn pin_bright_vec() {
        let crypt = RPCrypt::init_regist_ps4_pre10(&AMBASSADOR, 13_374_201);
        assert_eq!(
            crypt.bright,
            [
                0x3f, 0xd0, 0xd6, 0x4f, 0xdc, 0xbb, 0x3e, 0xcc, 0x50, 0xba, 0xed, 0xef, 0x97, 0x34,
                0xc7, 0xc9
            ]
        );
    }

    mod payload_vector {
        include!("../tests/vectors/regist_payload.rs");
    }

    #[test]
    fn request_payload_ps4_pre10_vector() {
        let info = RegistInfo {
            target: Target::Ps4_9,
            host: "127.0.0.1".into(),
            broadcast: false,
            psn_online_id: Some("ChiakiNanami1337".into()),
            psn_account_id: None,
            pin: 13_374_201,
        };
        let (_, payload) = build_payload(&info, &AMBASSADOR).unwrap();
        assert_eq!(payload.len(), payload_vector::EXPECTED.len());
        assert_eq!(payload, payload_vector::EXPECTED);
    }

    #[test]
    fn request_header_quirk() {
        let info = RegistInfo {
            target: Target::Ps5_1,
            host: "127.0.0.1".into(),
            broadcast: false,
            psn_online_id: None,
            psn_account_id: Some([1, 2, 3, 4, 5, 6, 7, 8]),
            pin: 12345678,
        };
        let header = format_request_header(&info, 534);
        assert!(header.starts_with("POST /sie/ps5/rp/sess/rgst HTTP/1.1\r\n HTTP/1.1\r\n"));
        assert!(header.contains("HOST: 10.0.2.15\r\n"));
        assert!(header.contains("Content-Length: 534\r\n"));
        assert!(header.contains("RP-Version: 1.0\r\n"));
        assert!(header.ends_with("\r\n\r\n"));
    }

    #[test]
    fn response_payload_parse() {
        let payload = b"PS5-RegistKey: 00112233445566778899aabbccddeeff\r\nRP-KeyType: 0\r\nRP-Key: 0123456789abcdef0123456789abcdef\r\nPS5-Mac: 001122334455\r\nPS5-Nickname: Test\r\n";
        let host = parse_response_payload(Target::Ps5_1, payload).unwrap();
        assert_eq!(
            host.rp_regist_key,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert_eq!(host.server_mac, [0, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert_eq!(host.server_nickname, "Test");
    }
}
