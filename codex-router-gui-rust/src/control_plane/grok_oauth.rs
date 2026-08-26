//! Host-side Grok / xAI OAuth (authorization code + PKCE).
//!
//! CLIProxyAPI's `/v0/management/xai-auth-url` does not pin the registered
//! loopback URI `http://127.0.0.1:56121/callback`. The GUI already owns that
//! port; Host generates PKCE, returns the authorize URL, and exchanges the
//! code on `auth.x.ai` so login no longer depends on CLIProxy consuming the
//! one-time code.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

use super::antigravity_oauth::email_from_id_token;
use super::grok_sso::XAI_CLIENT_ID;

pub const CALLBACK_PORT: u16 = 56121;
pub const REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";
pub const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
pub const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
pub const USERINFO_URL: &str = "https://auth.x.ai/oauth2/userinfo";
pub const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const CLI_CHAT_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkceSession {
    pub state: String,
    pub nonce: String,
    pub code_verifier: String,
    pub code_challenge: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokOAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub scope: String,
    pub email: String,
    pub sub: String,
}

pub fn new_pkce_session() -> PkceSession {
    let code_verifier = base64url(&random_bytes(32));
    let code_challenge = base64url(Sha256::digest(code_verifier.as_bytes()).as_slice());
    PkceSession {
        state: base64url(&random_bytes(16)),
        nonce: base64url(&random_bytes(16)),
        code_verifier,
        code_challenge,
    }
}

pub fn authorization_url(session: &PkceSession) -> String {
    let mut url = format!("{AUTHORIZE_URL}?response_type=code");
    for (key, value) in [
        ("client_id", XAI_CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("state", session.state.as_str()),
        ("nonce", session.nonce.as_str()),
        ("code_challenge", session.code_challenge.as_str()),
        ("code_challenge_method", "S256"),
    ] {
        url.push('&');
        url.push_str(key);
        url.push('=');
        url.push_str(&form_encode(value));
    }
    url
}

pub fn exchange_authorization_code(
    code: &str,
    code_verifier: &str,
    proxy_url: Option<&str>,
) -> Result<GrokOAuthTokens> {
    let code = code.trim();
    let code_verifier = code_verifier.trim();
    if code.is_empty() {
        bail!("authorization code is empty");
    }
    if code_verifier.is_empty() {
        bail!("PKCE code_verifier is missing");
    }
    let client = http_client(proxy_url)?;
    let token_json = post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", XAI_CLIENT_ID),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", code_verifier),
        ],
    )
    .context("grok token exchange")?;
    let access_token = json_string(&token_json, "access_token");
    let refresh_token = json_string(&token_json, "refresh_token");
    if access_token.is_empty() || refresh_token.is_empty() {
        bail!("token exchange returned no access/refresh token");
    }
    let id_token = json_string(&token_json, "id_token");
    let expires_in = token_json
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(21600);
    let (email, sub) = resolve_identity(&client, &access_token, &id_token);
    Ok(GrokOAuthTokens {
        access_token,
        refresh_token,
        id_token,
        token_type: {
            let value = json_string(&token_json, "token_type");
            if value.is_empty() {
                "Bearer".to_owned()
            } else {
                value
            }
        },
        expires_in,
        scope: json_string(&token_json, "scope"),
        email,
        sub,
    })
}

pub fn auth_file_stem(email: &str) -> String {
    let email = email.trim();
    if email.is_empty() {
        "xai".to_owned()
    } else {
        format!("xai-{email}")
    }
}

pub fn auth_document(tokens: &GrokOAuthTokens) -> Value {
    let expired = (chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let last_refresh = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    json!({
        "type": "xai",
        "auth_kind": "oauth",
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "id_token": tokens.id_token,
        "token_type": tokens.token_type,
        "expires_in": tokens.expires_in,
        "expired": expired,
        "last_refresh": last_refresh,
        "email": tokens.email,
        "sub": tokens.sub,
        "disabled": false,
        "base_url": CLI_CHAT_BASE_URL,
        "token_endpoint": TOKEN_URL,
        "redirect_uri": REDIRECT_URI,
        "scope": tokens.scope,
    })
}

fn resolve_identity(
    client: &reqwest::blocking::Client,
    access_token: &str,
    id_token: &str,
) -> (String, String) {
    let sub = sub_from_id_token(id_token).unwrap_or_default();
    if let Some(email) = email_from_id_token(id_token) {
        return (email, sub);
    }
    if let Some((email, userinfo_sub)) = fetch_userinfo(client, access_token) {
        let sub = if sub.is_empty() { userinfo_sub } else { sub };
        return (email, sub);
    }
    (fallback_email(access_token), sub)
}

fn fetch_userinfo(
    client: &reqwest::blocking::Client,
    access_token: &str,
) -> Option<(String, String)> {
    let response = client
        .get(USERINFO_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().ok()?;
    let email = value
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.contains('@'))
        .map(str::to_owned)?;
    let sub = value
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    Some((email, sub))
}

fn sub_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = decode_base64url(payload)?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("sub")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn fallback_email(seed: &str) -> String {
    let digest = Sha256::digest(seed.trim().as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("grok-{}@oauth.invalid", &hex[..12])
}

fn json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn http_client(proxy_url: Option<&str>) -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(4));
    if let Some(proxy) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        builder = builder.proxy(reqwest::Proxy::all(proxy).context("invalid proxy URL")?);
    }
    builder.build().context("build grok OAuth HTTP client")
}

fn post_form(
    client: &reqwest::blocking::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<Value> {
    let mut last_error = None;
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500));
        }
        let response = match client.post(url).form(form).send() {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            last_error = Some(format!("HTTP {status}: {}", truncate_error(&body)));
            continue;
        }
        return serde_json::from_str(&body).context("decode token JSON");
    }
    bail!(
        "{}",
        last_error.unwrap_or_else(|| "token request failed".to_owned())
    )
}

fn truncate_error(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 240 {
        trimmed.to_owned()
    } else {
        format!("{}…", &trimmed[..240])
    }
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn random_bytes(count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        out.extend_from_slice(uuid::Uuid::now_v7().as_bytes());
    }
    out.truncate(count);
    out
}

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let remaining = bytes.len() - index;
        let b0 = bytes[index];
        let b1 = if remaining > 1 { bytes[index + 1] } else { 0 };
        let b2 = if remaining > 2 { bytes[index + 2] } else { 0 };
        encoded.push(TABLE[(b0 >> 2) as usize] as char);
        encoded.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if remaining > 1 {
            encoded.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        }
        if remaining > 2 {
            encoded.push(TABLE[(b2 & 0x3f) as usize] as char);
        }
        index += 3;
    }
    encoded
}

fn decode_base64url(input: &str) -> Option<Vec<u8>> {
    let mut padded = input.replace('-', "+").replace('_', "/");
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let bytes = padded.as_bytes();
    let mut index = 0;
    while index + 4 <= bytes.len() {
        let mut values = [0u8; 4];
        for slot in 0..4 {
            let byte = bytes[index + slot];
            if byte == b'=' {
                values[slot] = 0;
                continue;
            }
            values[slot] = table.iter().position(|candidate| *candidate == byte)? as u8;
        }
        output.push((values[0] << 2) | (values[1] >> 4));
        if bytes[index + 2] != b'=' {
            output.push((values[1] << 4) | (values[2] >> 2));
        }
        if bytes[index + 3] != b'=' {
            output.push((values[2] << 6) | values[3]);
        }
        index += 4;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_the_verifier() {
        let session = new_pkce_session();
        assert!(session.code_verifier.len() >= 43);
        assert_eq!(
            session.code_challenge,
            base64url(Sha256::digest(session.code_verifier.as_bytes()).as_slice())
        );
        assert_ne!(session.state, session.nonce);
        assert_ne!(session.state, session.code_verifier);
    }

    #[test]
    fn authorization_url_pins_the_registered_loopback_callback() {
        let session = PkceSession {
            state: "state-1".into(),
            nonce: "nonce-1".into(),
            code_verifier: "verifier".into(),
            code_challenge: "challenge".into(),
        };
        let url = authorization_url(&session);
        assert!(url.starts_with("https://auth.x.ai/oauth2/authorize?"));
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-1"));
        assert!(!url.contains("localhost"));
        assert!(!url.contains("/auth/callback"));
    }

    #[test]
    fn auth_file_matches_cliproxy_xai_shape() {
        let tokens = GrokOAuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            id_token: "id".into(),
            token_type: "Bearer".into(),
            expires_in: 21600,
            scope: SCOPE.into(),
            email: "user@example.com".into(),
            sub: "sub-1".into(),
        };
        let document = auth_document(&tokens);
        assert_eq!(document["type"], "xai");
        assert_eq!(document["auth_kind"], "oauth");
        assert_eq!(document["disabled"], false);
        assert_eq!(document["base_url"], CLI_CHAT_BASE_URL);
        assert_eq!(document["token_endpoint"], TOKEN_URL);
        assert_eq!(document["redirect_uri"], REDIRECT_URI);
        assert_eq!(document["email"], "user@example.com");
        assert_eq!(auth_file_stem("user@example.com"), "xai-user@example.com");
        let expired = document["expired"].as_str().unwrap();
        assert!(expired.ends_with('Z'));
        assert!(!expired.contains('.'));
    }
}
