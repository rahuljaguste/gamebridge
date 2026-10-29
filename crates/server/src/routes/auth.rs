// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Optional token authentication for the JSON API and the event WebSocket.
//!
//! The token is configured on the command line or read from a file. A browser
//! verifies it once at `/api/auth/login` and receives an HttpOnly,
//! SameSite=Strict cookie containing a random, server-side session id; scripts
//! can use `Authorization: Bearer <token>`. When `--trust-proxy` is set and
//! the proxy reports HTTPS, the cookie is also marked `Secure`. Only a SHA-256
//! digest of the configured token and of each session id is kept in memory, so
//! neither a cookie nor a memory dump is directly reusable.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const COOKIE_NAME: &str = "gamebridge_auth";
const COOKIE_MAX_AGE: u64 = 30 * 24 * 60 * 60;
const SESSION_TTL: Duration = Duration::from_secs(COOKIE_MAX_AGE);
/// A failed login is delayed so a weak token cannot be guessed at wire speed.
const FAILURE_DELAY: Duration = Duration::from_millis(300);
const SLOW_FAILURE_DELAY: Duration = Duration::from_secs(1);
const MAX_FAILURES_BEFORE_SLOWDOWN: u32 = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct FailureWindow {
    count: u32,
    since: Instant,
}

impl Default for FailureWindow {
    fn default() -> Self {
        Self {
            count: 0,
            since: Instant::now(),
        }
    }
}

#[derive(Clone)]
pub struct Auth {
    /// SHA-256 of the configured token; `None` disables authentication.
    expected: Option<[u8; 32]>,
    /// SHA-256(session id) -> expiry. Session ids themselves never live here.
    sessions: Arc<Mutex<HashMap<[u8; 32], Instant>>>,
    /// Per-source failed-authentication counts used to slow down guessing.
    failures: Arc<Mutex<HashMap<IpAddr, FailureWindow>>>,
    /// Honor `X-Forwarded-Proto` from a reverse proxy when setting cookies.
    trust_proxy: bool,
}

impl Auth {
    pub fn disabled() -> Self {
        Self {
            expected: None,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            failures: Arc::new(Mutex::new(HashMap::new())),
            trust_proxy: false,
        }
    }

    pub fn from_token(token: &str) -> Self {
        Self {
            expected: Some(hash(token)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            failures: Arc::new(Mutex::new(HashMap::new())),
            trust_proxy: false,
        }
    }

    /// Mark the session cookie `Secure` when a trusted reverse proxy reports
    /// HTTPS. Enable this only when the server is reachable exclusively
    /// through that proxy.
    pub fn with_trusted_proxy(mut self, trust_proxy: bool) -> Self {
        self.trust_proxy = trust_proxy;
        self
    }

    pub fn required(&self) -> bool {
        self.expected.is_some()
    }

    fn matches_token(&self, token: &str) -> bool {
        let Some(expected) = self.expected.as_ref() else {
            return true;
        };
        expected.ct_eq(&hash(token)).into()
    }

    fn matches_session(&self, value: &str) -> bool {
        if self.expected.is_none() {
            return true;
        }
        let digest = hash(value);
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        let now = Instant::now();
        match sessions.get(&digest) {
            Some(expires) if *expires > now => true,
            Some(_) => {
                sessions.remove(&digest);
                false
            }
            None => false,
        }
    }

    fn new_session(&self) -> Option<String> {
        self.expected.as_ref()?;
        let id = hex::encode(rand::random::<[u8; 32]>());
        let digest = hash(&id);
        let now = Instant::now();
        let Ok(mut sessions) = self.sessions.lock() else {
            return None;
        };
        sessions.retain(|_, expires| *expires > now);
        sessions.insert(digest, now + SESSION_TTL);
        Some(id)
    }

    fn revoke_session(&self, value: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&hash(value));
        }
    }

    fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        if let Ok(mut failures) = self.failures.lock() {
            if failures.len() > 4096 {
                failures.retain(|_, window| now.duration_since(window.since) <= FAILURE_WINDOW);
            }
            let window = failures.entry(ip).or_default();
            if now.duration_since(window.since) > FAILURE_WINDOW {
                *window = FailureWindow {
                    count: 0,
                    since: now,
                };
            }
            window.count = window.count.saturating_add(1);
        }
    }

    fn failure_delay(&self, ip: IpAddr) -> Duration {
        let count = self
            .failures
            .lock()
            .ok()
            .and_then(|failures| failures.get(&ip).map(|window| window.count))
            .unwrap_or(0);
        if count >= MAX_FAILURES_BEFORE_SLOWDOWN {
            SLOW_FAILURE_DELAY
        } else {
            FAILURE_DELAY
        }
    }

    fn clear_failures(&self, ip: IpAddr) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.remove(&ip);
        }
    }

    /// True when the request carries no credentials and authentication is off,
    /// or when either credential form matches.
    pub fn authenticated(&self, headers: &HeaderMap) -> bool {
        if !self.required() {
            return true;
        }
        if let Some(token) = bearer_token(headers)
            && self.matches_token(token)
        {
            return true;
        }
        cookie(headers, COOKIE_NAME).is_some_and(|value| self.matches_session(value))
    }
}

fn hash(token: &str) -> [u8; 32] {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token)
    } else {
        None
    }
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then_some(value)
        })
        .next()
}

fn request_ip(request: &Request) -> IpAddr {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// True when a trusted reverse proxy reports that the browser used HTTPS.
/// The header is ignored unless `--trust-proxy` is set, so a client that
/// reaches the server directly cannot spoof the proxy's protocol.
fn forwarded_https(headers: &HeaderMap, trust_proxy: bool) -> bool {
    if !trust_proxy {
        return false;
    }
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

fn session_cookie(value: &str, max_age: u64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}")
}

#[derive(Serialize)]
pub(crate) struct AuthStatus {
    required: bool,
    authenticated: bool,
}

/// Open to unauthenticated clients: the login screen needs to know whether to
/// ask for a token.
pub async fn status(Extension(auth): Extension<Auth>, headers: HeaderMap) -> Json<AuthStatus> {
    Json(AuthStatus {
        required: auth.required(),
        authenticated: auth.authenticated(&headers),
    })
}

#[derive(Deserialize)]
pub struct LoginRequest {
    token: String,
}

pub async fn login(
    Extension(auth): Extension<Auth>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response {
    if !auth.required() {
        return Json(serde_json::json!({ "ok": true })).into_response();
    }
    if !auth.matches_token(&request.token) {
        let ip = addr.ip();
        auth.record_failure(ip);
        tokio::time::sleep(auth.failure_delay(ip)).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid token" })),
        )
            .into_response();
    }
    auth.clear_failures(addr.ip());
    let Some(value) = auth.new_session() else {
        return Json(serde_json::json!({ "ok": true })).into_response();
    };
    let secure = forwarded_https(&headers, auth.trust_proxy);
    json_with_cookie(&session_cookie(&value, COOKIE_MAX_AGE, secure))
}

pub async fn logout(Extension(auth): Extension<Auth>, headers: HeaderMap) -> Response {
    if let Some(value) = cookie(&headers, COOKIE_NAME) {
        auth.revoke_session(value);
    }
    let secure = forwarded_https(&headers, auth.trust_proxy);
    json_with_cookie(&session_cookie("", 0, secure))
}

/// A JSON success response that clears or sets the session cookie.
fn json_with_cookie(cookie: &str) -> Response {
    let mut response = Json(serde_json::json!({ "ok": true })).into_response();
    if let Ok(header_value) = HeaderValue::from_str(cookie) {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, header_value);
    }
    response
}

/// Gate every protected API route. For the WebSocket route this runs before
/// the upgrade, so an unauthenticated client never attaches (and never counts
/// as a live browser for the session watchdog).
pub async fn require(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    if auth.authenticated(request.headers()) {
        return next.run(request).await;
    }
    let ip = request_ip(&request);
    auth.record_failure(ip);
    tokio::time::sleep(auth.failure_delay(ip)).await;
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer realm=\"gamebridge\"")],
        Json(serde_json::json!({ "error": "authentication required" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).expect("header value"),
            );
        }
        map
    }

    #[test]
    fn disabled_allows_everything() {
        let auth = Auth::disabled();
        assert!(!auth.required());
        assert!(auth.authenticated(&HeaderMap::new()));
    }

    #[test]
    fn bearer_token_matches() {
        let auth = Auth::from_token("s3cret");
        assert!(auth.required());
        assert!(auth.authenticated(&headers(&[("authorization", "Bearer s3cret".to_string(),)])));
        assert!(auth.authenticated(&headers(&[("authorization", "bearer s3cret".to_string(),)])));
        assert!(!auth.authenticated(&headers(&[("authorization", "Bearer wrong".to_string(),)])));
        assert!(!auth.authenticated(&HeaderMap::new()));
    }

    #[test]
    fn cookie_session_matches_and_can_be_revoked() {
        let auth = Auth::from_token("s3cret");
        let value = auth.new_session().expect("auth is enabled");
        assert!(auth.authenticated(&headers(&[(
            "cookie",
            format!("other=1; {COOKIE_NAME}={value}; trailing=x"),
        )])));
        auth.revoke_session(&value);
        assert!(!auth.authenticated(&headers(&[("cookie", format!("{COOKIE_NAME}={value}"),)])));
        assert!(!auth.authenticated(&headers(&[("cookie", format!("{COOKIE_NAME}=deadbeef"),)])));
        assert!(!auth.authenticated(&headers(&[("cookie", "other=1".to_string())])));
    }

    #[test]
    fn repeated_failures_slow_the_response() {
        let auth = Auth::from_token("s3cret");
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..MAX_FAILURES_BEFORE_SLOWDOWN {
            auth.record_failure(ip);
        }
        assert_eq!(auth.failure_delay(ip), SLOW_FAILURE_DELAY);
        auth.clear_failures(ip);
        assert_eq!(auth.failure_delay(ip), FAILURE_DELAY);
    }

    #[test]
    fn forwarded_proto_is_only_honored_with_trust_proxy() {
        let https = headers(&[("x-forwarded-proto", "https".to_string())]);
        let http = headers(&[("x-forwarded-proto", "http".to_string())]);
        let list = headers(&[("x-forwarded-proto", "https, http".to_string())]);
        assert!(!forwarded_https(&https, false));
        assert!(forwarded_https(&https, true));
        assert!(!forwarded_https(&http, true));
        assert!(forwarded_https(&list, true));
        assert!(!forwarded_https(&HeaderMap::new(), true));
    }

    #[test]
    fn session_cookie_sets_secure_only_when_asked() {
        let insecure = session_cookie("abc", COOKIE_MAX_AGE, false);
        assert!(insecure.contains("HttpOnly"));
        assert!(insecure.contains("SameSite=Strict"));
        assert!(!insecure.contains("; Secure"));

        let secure = session_cookie("abc", COOKIE_MAX_AGE, true);
        assert!(secure.ends_with("; Secure"));
    }
}
