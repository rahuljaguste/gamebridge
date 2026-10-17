// SPDX-License-Identifier: AGPL-3.0-only
//! PSN sign-in helper, mirroring Chiaki's `scripts/psn-account-id.py`.
//!
//! The user signs in on Sony's page, then pastes the redirect URL back into
//! the web UI. Only the OAuth code is exchanged here; passwords never reach
//! this server, and tokens are used once and discarded.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use url::Url;

use chiaki_core::regist::{PSN_ACCOUNT_ID_SIZE, parse_psn_account_id};

// Public client credentials of the Remote Play Windows client, as used by
// Chiaki's helper script.
const CLIENT_ID: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
const CLIENT_SECRET: &str = "mvaiZkRsAsI1IBkY";
const AUTH_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize";
const TOKEN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";
const REDIRECT_URI: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";

#[derive(Debug)]
pub struct PsnAccount {
    pub user_id: String,
    pub account_id: [u8; PSN_ACCOUNT_ID_SIZE],
}

/// URL to open in the browser for PSN sign-in.
pub fn login_url() -> String {
    format!(
        "{AUTH_URL}?service_entity=urn:service-entity:psn&response_type=code&client_id={CLIENT_ID}\
         &redirect_uri={REDIRECT_URI}&scope=psn:clientapp&request_locale=en_US&ui=pr\
         &service_logo=ps&layout_type=popup&smcid=remoteplay&prompt=always\
         &PlatformPrivacyWs1=minimal&"
    )
}

/// Pull the OAuth code out of a pasted redirect URL. A bare code is also
/// accepted, so users can paste either the whole URL or just the value.
pub fn extract_code(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        bail!("paste the redirect URL from the PSN sign-in page");
    }
    if let Ok(url) = Url::parse(input) {
        for (key, value) in url.query_pairs() {
            if key == "code" {
                if value.is_empty() {
                    bail!("the redirect URL contains an empty code");
                }
                return Ok(value.into_owned());
            }
        }
        if input.contains("://") {
            bail!("the pasted URL has no \"code\" parameter");
        }
    }
    if input.len() > 4096 {
        bail!("the pasted code is too long");
    }
    Ok(input.to_string())
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("ps-remoteplay/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .context("could not create the HTTP client")
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
}

fn oauth_error(body: &Value) -> Option<String> {
    let error = body.get("error")?.as_str()?;
    let description = body.get("error_description").and_then(Value::as_str);
    Some(match description {
        Some(description) if !description.is_empty() => {
            format!("{error}: {description}")
        }
        _ => error.to_string(),
    })
}

async fn response_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .context("PSN returned a response that was not JSON")?;
    if !status.is_success() {
        let detail = oauth_error(&body).unwrap_or_else(|| status.to_string());
        bail!("PSN rejected the request: {detail}");
    }
    Ok(body)
}

/// Exchange a redirect URL (or code) for the console-facing account ID.
pub async fn resolve_account_id(input: &str) -> Result<PsnAccount> {
    let code = extract_code(input)?;
    let client = client()?;

    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", &code)
        .append_pair("redirect_uri", REDIRECT_URI)
        .finish();
    let response = client
        .post(TOKEN_URL)
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .context("could not reach the PSN token service")?;
    let token: TokenResponse = serde_json::from_value(response_json(response).await?)
        .context("PSN token response is missing fields")?;
    let access_token = token
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("PSN did not return an access token"))?;

    let response = client
        .get(format!("{TOKEN_URL}/{}", quote_path(&access_token)))
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .send()
        .await
        .context("could not reach the PSN account service")?;
    let info = response_json(response).await?;
    let user_id = match info.get("user_id") {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        _ => bail!("PSN account info did not include a user_id"),
    };
    let account_id = parse_psn_account_id(&user_id)
        .context("PSN returned a user_id that is not a 64-bit account number")?;
    Ok(PsnAccount {
        user_id,
        account_id,
    })
}

/// Percent-encode a token used as a URL path segment. Mirrors Python's
/// `quote(value)` default (`/` stays safe) used by Chiaki's helper.
fn quote_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_code_from_redirect_url() {
        assert_eq!(
            extract_code(
                "https://remoteplay.dl.playstation.net/remoteplay/redirect?code=abc123&state=x"
            )
            .unwrap(),
            "abc123"
        );
        assert_eq!(
            extract_code("https://remoteplay.dl.playstation.net/remoteplay/redirect?code=a%2Fb")
                .unwrap(),
            "a/b"
        );
        assert_eq!(extract_code("  bare-code  ").unwrap(), "bare-code");
    }

    #[test]
    fn rejects_missing_code() {
        assert!(extract_code("").is_err());
        assert!(extract_code("https://remoteplay.dl.playstation.net/remoteplay/redirect").is_err());
        assert!(
            extract_code("https://remoteplay.dl.playstation.net/remoteplay/redirect?code=")
                .is_err()
        );
    }

    #[test]
    fn quotes_path_segment() {
        assert_eq!(quote_path("abc-._~/x"), "abc-._~/x");
        assert_eq!(quote_path("a b+c"), "a%20b%2Bc");
    }
}
