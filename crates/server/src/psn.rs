// SPDX-License-Identifier: AGPL-3.0-only
//! PSN account-ID helpers.
//!
//! The redirect-code flow mirrors Chiaki's `scripts/psn-account-id.py`: the
//! user signs in on Sony's page, then pastes the redirect URL back into the web
//! UI. Only the OAuth code is exchanged here; passwords never reach this
//! server, and tokens are used once and discarded.
//!
//! The optional public lookup follows chiaki-ng's "Public Lookup" path and
//! queries `psn.flipscreen.games` by PSN Online ID. It is a third-party
//! service, requires the account's privacy setting to allow public search, and
//! does not use a PSN password or token.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use url::Url;

use chiaki_core::regist::{PSN_ACCOUNT_ID_SIZE, parse_psn_account_id};

// Public client credentials inherited from Chiaki v2.2.0's AGPL source (its
// PSN helper script). They were not extracted from a Sony SDK, firmware, or
// decompiled Sony software by this project.
const CLIENT_ID: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
const CLIENT_SECRET: &str = "mvaiZkRsAsI1IBkY";
const AUTH_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize";
const TOKEN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";
const REDIRECT_URI: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";
const PUBLIC_LOOKUP_URL: &str = "https://psn.flipscreen.games/search.php";

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
        .user_agent(concat!("gamebridge/", env!("CARGO_PKG_VERSION")))
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

/// Look up the encoded Account ID by PSN Online ID using the public
/// flipscreen.games service used by chiaki-ng. This does not authenticate with
/// PSN; the account's privacy settings must allow anyone to find it in search.
pub async fn public_lookup(online_id: &str) -> Result<String> {
    let online_id = online_id.trim();
    if online_id.is_empty() {
        bail!("enter a PSN Online ID");
    }
    if online_id.len() > 64 {
        bail!("PSN Online ID is too long");
    }
    let encoded: String = url::form_urlencoded::byte_serialize(online_id.as_bytes()).collect();
    let response = client()?
        .get(format!("{PUBLIC_LOOKUP_URL}?username={encoded}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .context("could not reach the public lookup service")?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("could not read the public lookup response")?;
    if !status.is_success() && text.trim().is_empty() {
        bail!("public lookup service is currently unavailable");
    }
    let body: Value = serde_json::from_str(&text)
        .context("public lookup returned a response that was not JSON")?;
    parse_public_lookup(status, &body)
}

fn parse_public_lookup(status: reqwest::StatusCode, body: &Value) -> Result<String> {
    if !status.is_success() {
        let detail = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("public lookup service is unavailable");
        bail!("{detail}");
    }
    let encoded_id = body
        .get("encoded_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                body.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("PSN account not found or privacy settings block public lookup")
            )
        })?;
    let bytes = chiaki_core::util::base64::decode(encoded_id)
        .map_err(|_| anyhow::anyhow!("public lookup returned an invalid Account ID"))?;
    if bytes.len() != PSN_ACCOUNT_ID_SIZE {
        bail!("public lookup returned an invalid Account ID");
    }
    Ok(chiaki_core::util::base64::encode(&bytes))
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

    #[test]
    fn parses_public_lookup_account_id() {
        let body = serde_json::json!({
            "online_id": "example",
            "user_id": "0",
            "encoded_id": "AAAAAAAAAAA="
        });
        assert_eq!(
            parse_public_lookup(reqwest::StatusCode::OK, &body).unwrap(),
            "AAAAAAAAAAA="
        );
    }

    #[test]
    fn reports_public_lookup_errors() {
        let body = serde_json::json!({ "error": "account not found" });
        let error = parse_public_lookup(reqwest::StatusCode::NOT_FOUND, &body)
            .unwrap_err()
            .to_string();
        assert!(error.contains("account not found"));
        assert!(
            parse_public_lookup(
                reqwest::StatusCode::OK,
                &serde_json::json!({ "encoded_id": "not-base64" })
            )
            .is_err()
        );
    }
}
