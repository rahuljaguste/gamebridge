// SPDX-License-Identifier: AGPL-3.0-only
//! Reject requests that did not originate from this machine's browser UI.
//!
//! Listening on loopback is not an origin boundary. Any web page can open a
//! WebSocket to `127.0.0.1` (WebSockets are exempt from CORS), and DNS
//! rebinding lets a remote page reach the JSON API with a Host that makes the
//! request same-origin. Requiring a loopback `Host` and, when present, a
//! loopback `Origin` whose port matches closes both paths.
//!
//! This is a browser-origin filter, not authentication. `--allow-remote`
//! deliberately relaxes the Host check for a non-loopback bind, but still
//! requires a browser Origin to match the requested host.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Clone, Copy)]
pub struct LocalOnly {
    bind: SocketAddr,
    allow_remote: bool,
}

impl LocalOnly {
    pub fn new(bind: SocketAddr, allow_remote: bool) -> Self {
        Self { bind, allow_remote }
    }
}

pub async fn local_only(State(local): State<LocalOnly>, request: Request, next: Next) -> Response {
    let host = header_str(&request, header::HOST);
    // HTTP/2 carries the authority in the URI instead of a Host header.
    let host = host.or_else(|| request.uri().authority().map(|a| a.as_str()));
    let origin = header_str(&request, header::ORIGIN);
    if allowed(local.bind, local.allow_remote, host, origin) {
        next.run(request).await
    } else {
        tracing::warn!(?host, ?origin, "rejected request with invalid Host/Origin");
        (StatusCode::FORBIDDEN, "forbidden\n").into_response()
    }
}

fn header_str(request: &Request, name: header::HeaderName) -> Option<&str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn allowed(bind: SocketAddr, allow_remote: bool, host: Option<&str>, origin: Option<&str>) -> bool {
    let Some((host_name, host_port)) = host.and_then(parse_authority) else {
        return false;
    };
    // `--allow-remote` only widens the policy for a non-loopback bind. A
    // loopback bind stays strict even if the flag is passed.
    let open_hosts = allow_remote && !bind.ip().is_loopback();
    if !open_hosts && !host_allowed(bind, host_name) {
        return false;
    }
    let Some(origin) = origin else {
        return true;
    };
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    let default_port = match uri.scheme_str() {
        Some("http") => 80,
        Some("https") => 443,
        _ => return false,
    };
    let Some(origin_name) = uri.host() else {
        return false;
    };
    if open_hosts {
        // A cross-site page can open a WebSocket to a LAN address; requiring
        // the Origin host to match the Host blocks it. DNS rebinding remains
        // possible in remote mode, which is why the mode is opt-in.
        if !same_host(host_name, origin_name) {
            return false;
        }
    } else if !host_allowed(bind, origin_name) {
        return false;
    }
    let origin_port = uri.port_u16().unwrap_or(default_port);
    // This server speaks plain HTTP, so a Host without a port means port 80.
    origin_port == host_port.unwrap_or(80)
}

fn same_host(a: &str, b: &str) -> bool {
    let normalize = |raw: &str| {
        raw.strip_prefix('[')
            .and_then(|name| name.strip_suffix(']'))
            .unwrap_or(raw)
            .to_ascii_lowercase()
    };
    normalize(a) == normalize(b)
}

fn host_allowed(bind: SocketAddr, raw: &str) -> bool {
    let name = raw
        .strip_prefix('[')
        .and_then(|name| name.strip_suffix(']'))
        .unwrap_or(raw);
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match name.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback() || (!bind.ip().is_unspecified() && ip == bind.ip()),
        Err(_) => false,
    }
}

/// Split an HTTP authority into host and optional port, handling bracketed
/// IPv6 literals.
fn parse_authority(authority: &str) -> Option<(&str, Option<u16>)> {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        let (name, tail) = rest.split_once(']')?;
        let port = if tail.is_empty() {
            None
        } else {
            Some(tail.strip_prefix(':')?.parse().ok()?)
        };
        return Some((name, port));
    }
    if authority.matches(':').count() > 1 {
        // Unbracketed IPv6 literal; it carries no port.
        return Some((authority, None));
    }
    match authority.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && !port.is_empty() => {
            Some((name, Some(port.parse().ok()?)))
        }
        _ => Some((authority, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind() -> SocketAddr {
        "127.0.0.1:8080".parse().unwrap()
    }

    fn local(host: Option<&str>, origin: Option<&str>) -> bool {
        allowed(bind(), false, host, origin)
    }

    #[test]
    fn accepts_the_local_ui() {
        assert!(local(Some("127.0.0.1:8080"), Some("http://127.0.0.1:8080")));
        assert!(local(Some("localhost:8080"), Some("http://localhost:8080")));
        assert!(local(Some("localhost:8080"), None));
        assert!(local(Some("[::1]:8080"), Some("http://[::1]:8080")));
    }

    #[test]
    fn rejects_dns_rebinding_and_foreign_origins() {
        assert!(!local(
            Some("evil.example:8080"),
            Some("http://evil.example:8080")
        ));
        assert!(!local(Some("evil.example:8080"), None));
        assert!(!local(
            Some("127.0.0.1:8080"),
            Some("http://evil.example:8080")
        ));
        assert!(!local(Some("127.0.0.1:8080"), Some("null")));
        assert!(!local(None, None));
    }

    #[test]
    fn rejects_origins_from_another_local_port() {
        // A different local dev server must not be able to drive this daemon.
        assert!(!local(
            Some("127.0.0.1:8080"),
            Some("http://127.0.0.1:3000")
        ));
    }

    #[test]
    fn remote_mode_requires_the_flag_and_a_matching_origin() {
        let any: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        assert!(allowed(
            any,
            true,
            Some("192.168.1.20:8080"),
            Some("http://192.168.1.20:8080")
        ));
        assert!(allowed(any, true, Some("ps-bridge.local:8080"), None));
        assert!(!allowed(
            any,
            true,
            Some("192.168.1.20:8080"),
            Some("http://evil.example:8080")
        ));
        assert!(!allowed(any, false, Some("192.168.1.20:8080"), None));
        // A loopback bind stays strict even when the flag is present.
        assert!(!allowed(bind(), true, Some("evil.example:8080"), None));
    }

    #[test]
    fn parses_authorities() {
        assert_eq!(
            parse_authority("localhost:8080"),
            Some(("localhost", Some(8080)))
        );
        assert_eq!(parse_authority("localhost"), Some(("localhost", None)));
        assert_eq!(parse_authority("[::1]:8080"), Some(("::1", Some(8080))));
        assert_eq!(parse_authority("[::1]"), Some(("::1", None)));
        assert_eq!(parse_authority("::1"), Some(("::1", None)));
    }
}
