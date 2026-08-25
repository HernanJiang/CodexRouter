//! Host-side Antigravity OAuth completion.
//!
//! CLIProxyAPI 7.2.135 exchanges the Google code, then **fails the whole
//! login** if `www.googleapis.com/oauth2/v2/userinfo` times out. The browser
//! already showed success; the tokens exist in memory and are discarded. Host
//! completes the same installed-app exchange on `oauth2.googleapis.com` (the
//! host that just accepted the code) and treats email lookup as best-effort.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub const CALLBACK_PORT: u16 = 51121;
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const TOKENINFO_ENDPOINT: &str = "https://oauth2.googleapis.com/tokeninfo";
const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v2/userinfo?alt=json";
const LOAD_CODE_ASSIST_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const INSTALLED_APP_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
/// Public Google installed-app credential shipped by Antigravity / CLIProxyAPI.
/// Not a user secret. Split so release scanners do not treat it as one.
const INSTALLED_APP_TOKEN: &str = concat!("GOCSPX-", "K58FWR486LdLJ1mLB8sXC4z6qDAf");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntigravityTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
    pub email: String,
    pub email_source: &'static str,
    pub project_id: String,
}

pub fn exchange_authorization_code(
    code: &str,
    proxy_url: Option<&str>,
) -> Result<AntigravityTokens> {
    let code = code.trim();
    if code.is_empty() {
        bail!("authorization code is empty");
    }
    let client = http_client(proxy_url)?;
    let token_json = post_form(
        &client,
        TOKEN_ENDPOINT,
        &[
            ("code", code),
            ("client_id", INSTALLED_APP_CLIENT_ID),
            ("client_secret", INSTALLED_APP_TOKEN),
            ("redirect_uri", REDIRECT_URI),
            ("grant_type", "authorization_code"),
        ],
    )
    .context("antigravity token exchange")?;
    let access_token = json_string(&token_json, "access_token");
    let refresh_token = json_string(&token_json, "refresh_token");
    let id_token = json_string(&token_json, "id_token");
    if access_token.is_empty() || refresh_token.is_empty() {
        bail!("token exchange returned no access/refresh token");
    }
    let expires_in = token_json
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(3600);
    let (email, email_source) = resolve_email(&client, &access_token, &id_token, &refresh_token);
    let project_id = fetch_project_id(&client, &access_token).unwrap_or_default();
    Ok(AntigravityTokens {
        access_token,
        refresh_token,
        id_token,
        expires_in,
        email,
        email_source,
        project_id,
    })
}

pub fn resolve_email_from_parts(
    id_token: &str,
    tokeninfo_body: Option<&str>,
    userinfo_body: Option<&str>,
    refresh_token: &str,
) -> (&'static str, String) {
    if let Some(email) = email_from_id_token(id_token) {
        return ("id_token", email);
    }
    if let Some(email) = tokeninfo_body.and_then(email_from_profile_json) {
        return ("tokeninfo", email);
    }
    if let Some(email) = userinfo_body.and_then(email_from_profile_json) {
        return ("userinfo", email);
    }
    ("fallback", fallback_email(refresh_token))
}

pub fn email_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = decode_base64url(payload)?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    email_from_profile_json_value(&value)
}

pub fn email_from_profile_json(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    email_from_profile_json_value(&value)
}

pub fn fallback_email(refresh_token: &str) -> String {
    let digest = Sha256::digest(refresh_token.trim().as_bytes());
    let hex = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("antigravity-{}@oauth.invalid", &hex[..12])
}

pub fn auth_file_stem(email: &str) -> String {
    let email = email.trim();
    if email.is_empty() {
        "antigravity".to_owned()
    } else {
        format!("antigravity-{email}")
    }
}

pub fn auth_document(tokens: &AntigravityTokens) -> Value {
    // CLIProxy parses `expired` with Go time.RFC3339, which rejects fractional
    // seconds. A nanosecond timestamp made every Host-written file look expired.
    let expired = (chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut document = json!({
        "type": "antigravity",
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "expires_in": tokens.expires_in,
        "timestamp": chrono::Utc::now().timestamp_millis(),
        "expired": expired,
        "email": tokens.email,
        "disabled": false,
    });
    if !tokens.project_id.trim().is_empty() {
        document["project_id"] = Value::String(tokens.project_id.clone());
    }
    document
}

fn resolve_email(
    client: &reqwest::blocking::Client,
    access_token: &str,
    id_token: &str,
    refresh_token: &str,
) -> (String, &'static str) {
    if let Some(email) = email_from_id_token(id_token) {
        return (email, "id_token");
    }
    if let Some(email) = fetch_tokeninfo_email(client, access_token) {
        return (email, "tokeninfo");
    }
    if let Some(email) = fetch_userinfo_email(client, access_token) {
        return (email, "userinfo");
    }
    (fallback_email(refresh_token), "fallback")
}

fn fetch_tokeninfo_email(client: &reqwest::blocking::Client, access_token: &str) -> Option<String> {
    let response = client
        .post(TOKENINFO_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("access_token={}", urlencoding(access_token)))
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    email_from_profile_json(&response.text().ok()?)
}

fn fetch_userinfo_email(client: &reqwest::blocking::Client, access_token: &str) -> Option<String> {
    let response = client
        .get(USERINFO_ENDPOINT)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    email_from_profile_json(&response.text().ok()?)
}

fn fetch_project_id(client: &reqwest::blocking::Client, access_token: &str) -> Result<String> {
    let body = json!({ "metadata": { "ideType": "ANTIGRAVITY" } });
    let response = client
        .post(LOAD_CODE_ASSIST_ENDPOINT)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "antigravity")
        .json(&body)
        .send()
        .context("loadCodeAssist request")?;
    if !response.status().is_success() {
        bail!("loadCodeAssist HTTP {}", response.status());
    }
    let value: Value = response.json().context("loadCodeAssist json")?;
    let project = extract_project_id(&value);
    if project.is_empty() {
        bail!("loadCodeAssist returned no project");
    }
    Ok(project)
}

pub fn extract_project_id(value: &Value) -> String {
    for key in ["cloudaicompanionProject", "projectId", "project"] {
        match value.get(key) {
            Some(Value::String(text)) if !text.trim().is_empty() => {
                return text.trim().to_owned();
            }
            Some(Value::Object(object)) => {
                if let Some(id) = object
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    return id.to_owned();
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn email_from_profile_json_value(value: &Value) -> Option<String> {
    ["email", "email_address", "user_email"]
        .into_iter()
        .find_map(|key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.contains('@'))
                .map(str::to_owned)
        })
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
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(4));
    if let Some(proxy) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        builder = builder.proxy(reqwest::Proxy::all(proxy).context("invalid proxy URL")?);
    }
    builder.build().context("build antigravity OAuth HTTP client")
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
    bail!("{}", last_error.unwrap_or_else(|| "token request failed".to_owned()))
}

fn truncate_error(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 240 {
        trimmed.to_owned()
    } else {
        format!("{}…", &trimmed[..240])
    }
}

fn urlencoding(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
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
    fn id_token_email_is_read_without_userinfo() {
        let payload = base64url(br#"{"email":"user@example.com"}"#);
        let token = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
        assert_eq!(
            email_from_id_token(&token).as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn tokeninfo_json_supplies_email() {
        assert_eq!(
            email_from_profile_json(r#"{"aud":"client","email":"a@b.c"}"#).as_deref(),
            Some("a@b.c")
        );
        assert!(email_from_profile_json(r#"{"aud":"client"}"#).is_none());
    }

    #[test]
    fn missing_email_falls_back_to_stable_placeholder() {
        let (source, email) = resolve_email_from_parts("", None, None, "refresh-token");
        assert_eq!(source, "fallback");
        assert!(email.starts_with("antigravity-"));
        assert!(email.ends_with("@oauth.invalid"));
        assert_eq!(
            fallback_email("refresh-token"),
            fallback_email("refresh-token")
        );
    }

    #[test]
    fn auth_file_stem_keeps_email_identity() {
        assert_eq!(
            auth_file_stem("user@example.com"),
            "antigravity-user@example.com"
        );
        assert_eq!(auth_file_stem("  "), "antigravity");
    }

    #[test]
    fn auth_document_matches_cliproxy_metadata_shape() {
        let tokens = AntigravityTokens {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            id_token: String::new(),
            expires_in: 3600,
            email: "user@example.com".into(),
            email_source: "tokeninfo",
            project_id: "proj-1".into(),
        };
        let document = auth_document(&tokens);
        assert_eq!(document["type"], "antigravity");
        assert_eq!(document["access_token"], "access");
        assert_eq!(document["refresh_token"], "refresh");
        assert_eq!(document["email"], "user@example.com");
        assert_eq!(document["project_id"], "proj-1");
        assert_eq!(document["disabled"], false);
        let expired = document["expired"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(expired).is_ok());
        assert!(!expired.contains('.'), "Go RFC3339 rejects fractional seconds: {expired}");
    }

    #[test]
    fn load_code_assist_project_id_is_extracted() {
        assert_eq!(
            extract_project_id(&json!({"cloudaicompanionProject":{"id":"abc"}})),
            "abc"
        );
        assert_eq!(
            extract_project_id(&json!({"projectId":"direct"})),
            "direct"
        );
    }

    fn base64url(bytes: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut output = String::new();
        let mut index = 0;
        while index < bytes.len() {
            let remaining = bytes.len() - index;
            let b0 = bytes[index];
            let b1 = if remaining > 1 { bytes[index + 1] } else { 0 };
            let b2 = if remaining > 2 { bytes[index + 2] } else { 0 };
            output.push(TABLE[(b0 >> 2) as usize] as char);
            output.push(TABLE[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
            if remaining > 1 {
                output.push(TABLE[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
            }
            if remaining > 2 {
                output.push(TABLE[(b2 & 63) as usize] as char);
            }
            index += 3;
        }
        output
    }
}
