//! xAI Grok Web SSO token to OAuth token converter.
//!
//! Faithful port of the Sub2API `xai.ConvertSSOToBuild` device flow that
//! CLIProxyAPI does not provide: validate the SSO cookie, start a device
//! authorization, approve it with the web session, then poll for tokens.

use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const SSO_ACCOUNTS_URL: &str = "https://accounts.x.ai/";
pub const SSO_DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
pub const SSO_VERIFY_URL: &str = "https://auth.x.ai/oauth2/device/verify";
pub const SSO_APPROVE_URL: &str = "https://auth.x.ai/oauth2/device/approve";
pub const SSO_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const SSO_BUILD_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";
const SSO_DEFAULT_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_POLL: Duration = Duration::from_secs(75);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub scope: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SsoError {
    Unauthorized,
    Denied,
    Timeout,
    Upstream(u16),
    Other(String),
}

impl std::fmt::Display for SsoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(formatter, "Grok Web SSO cookie is invalid or expired"),
            Self::Denied => write!(formatter, "xAI device authorization was denied or expired"),
            Self::Timeout => write!(formatter, "xAI SSO conversion timed out"),
            Self::Upstream(status) => write!(formatter, "xAI SSO upstream returned HTTP {status}"),
            Self::Other(message) => write!(formatter, "xAI SSO conversion failed: {message}"),
        }
    }
}

impl std::error::Error for SsoError {}

pub fn normalize_sso_token(token: &str) -> String {
    let token = token.trim();
    let token = token.strip_prefix("sso=").unwrap_or(token);
    token.trim_matches(';').trim().to_owned()
}

struct Flow {
    client: reqwest::blocking::Client,
    sso_token: String,
}

impl Flow {
    fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        form: Option<&[(&str, &str)]>,
    ) -> std::result::Result<(u16, String, String), SsoError> {
        let mut request = self
            .client
            .request(method, url)
            .header("User-Agent", SSO_DEFAULT_UA)
            .header("Cookie", format!("sso={0}; sso-rw={0}", self.sso_token));
        if let Some(form) = form {
            request = request.form(form);
        }
        let response = request.send().map_err(|error| {
            if error.is_timeout() {
                SsoError::Timeout
            } else {
                SsoError::Other(error.to_string())
            }
        })?;
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let body = response
            .text()
            .map_err(|error| SsoError::Other(error.to_string()))?;
        Ok((status, final_url, body))
    }
}

/// Convert one normalized Grok Web SSO token into OAuth tokens.
pub fn convert_sso_to_oauth(
    sso_token: &str,
    proxy_url: Option<&str>,
) -> std::result::Result<GrokTokens, SsoError> {
    let sso_token = normalize_sso_token(sso_token);
    if sso_token.is_empty() {
        return Err(SsoError::Unauthorized);
    }
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15));
    if let Some(proxy) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        let proxy =
            reqwest::Proxy::all(proxy).map_err(|error| SsoError::Other(error.to_string()))?;
        builder = builder.proxy(proxy);
    }
    let client = builder
        .build()
        .map_err(|error| SsoError::Other(error.to_string()))?;
    let flow = Flow { client, sso_token };

    // 1. Validate the SSO cookie against the account portal.
    let (status, final_url, _) = flow.send(reqwest::Method::GET, SSO_ACCOUNTS_URL, None)?;
    if status == 401 || final_url.contains("sign-in") || final_url.contains("sign-up") {
        return Err(SsoError::Unauthorized);
    }
    if !(200..400).contains(&status) {
        return Err(SsoError::Upstream(status));
    }

    // 2. Start the device flow.
    let (status, _, body) = flow.send(
        reqwest::Method::POST,
        SSO_DEVICE_URL,
        Some(&[("client_id", XAI_CLIENT_ID), ("scope", SSO_BUILD_SCOPE)]),
    )?;
    if !(200..300).contains(&status) {
        return Err(SsoError::Upstream(status));
    }
    let device: Value =
        serde_json::from_str(&body).map_err(|error| SsoError::Other(error.to_string()))?;
    let device_code = device
        .get("device_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let user_code = device
        .get("user_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let verification = device
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if device_code.is_empty()
        || user_code.is_empty()
        || !verification.starts_with("https://auth.x.ai")
    {
        return Err(SsoError::Other(
            "xAI device flow response is incomplete".to_owned(),
        ));
    }
    let interval = device
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .max(1);
    let expires_in = device
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(1800);

    // 3. Open the verification page with the SSO session.
    let (status, _, _) = flow.send(reqwest::Method::GET, &verification, None)?;
    if !(200..400).contains(&status) {
        return Err(SsoError::Upstream(status));
    }

    // 4. Verify the user code.
    let (status, final_url, _) = flow.send(
        reqwest::Method::POST,
        SSO_VERIFY_URL,
        Some(&[("user_code", user_code.as_str())]),
    )?;
    if !(200..400).contains(&status) {
        return Err(SsoError::Upstream(status));
    }
    if !final_url.contains("consent") {
        return Err(SsoError::Denied);
    }

    // 5. Approve the device.
    let (status, final_url, _) = flow.send(
        reqwest::Method::POST,
        SSO_APPROVE_URL,
        Some(&[
            ("user_code", user_code.as_str()),
            ("action", "allow"),
            ("principal_type", "User"),
            ("principal_id", ""),
        ]),
    )?;
    if !(200..400).contains(&status) {
        return Err(SsoError::Upstream(status));
    }
    if !final_url.contains("done") {
        return Err(SsoError::Denied);
    }

    // 6. Poll for the token.
    let deadline = Instant::now() + MAX_POLL.min(Duration::from_secs(expires_in));
    loop {
        if Instant::now() >= deadline {
            return Err(SsoError::Timeout);
        }
        std::thread::sleep(Duration::from_secs(interval));
        let (status, _, body) = flow.send(
            reqwest::Method::POST,
            SSO_TOKEN_URL,
            Some(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", XAI_CLIENT_ID),
                ("device_code", device_code.as_str()),
            ]),
        )?;
        let payload: Value =
            serde_json::from_str(&body).map_err(|error| SsoError::Other(error.to_string()))?;
        if (200..300).contains(&status) {
            if let Some(access_token) = payload
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                return Ok(GrokTokens {
                    access_token: access_token.to_owned(),
                    refresh_token: payload
                        .get("refresh_token")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    id_token: payload
                        .get("id_token")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    token_type: payload
                        .get("token_type")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("Bearer")
                        .to_owned(),
                    expires_in: payload
                        .get("expires_in")
                        .and_then(Value::as_i64)
                        .unwrap_or(21600),
                    scope: payload
                        .get("scope")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
        }
        match payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "authorization_pending" | "slow_down" => continue,
            "access_denied" | "expired_token" => return Err(SsoError::Denied),
            _other if !(200..300).contains(&status) => return Err(SsoError::Upstream(status)),
            _ => continue,
        }
    }
}

/// Converted tokens as the legacy Sub2API account credential JSON shape.
pub fn tokens_to_credentials(tokens: &GrokTokens) -> Value {
    json!({
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "id_token": tokens.id_token,
        "token_type": tokens.token_type,
        "expires_at": (chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in)).to_rfc3339(),
        "client_id": XAI_CLIENT_ID,
        "scope": tokens.scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sso_token_normalization_accepts_raw_and_cookie_forms() {
        assert_eq!(normalize_sso_token("  abc123  "), "abc123");
        assert_eq!(normalize_sso_token("sso=abc123;"), "abc123");
        assert!(normalize_sso_token("   ").is_empty());
    }

    #[test]
    fn credentials_shape_matches_legacy_contract() {
        let tokens = GrokTokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            token_type: "Bearer".into(),
            expires_in: 21600,
            scope: "scope".into(),
        };
        let credentials = tokens_to_credentials(&tokens);
        assert_eq!(credentials["client_id"], XAI_CLIENT_ID);
        assert!(credentials.get("expires_at").is_some());
    }
}
