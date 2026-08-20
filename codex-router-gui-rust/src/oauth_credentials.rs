use anyhow::{bail, Result};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthProvider {
    OpenAi,
    Anthropic,
    Gemini,
    Antigravity,
    Grok,
}

impl OAuthProvider {
    pub fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match normalized.as_str() {
            "openai" | "chatgpt" | "codex" => Ok(Self::OpenAi),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "gemini" | "google" | "gemini_cli" => Ok(Self::Gemini),
            "antigravity" => Ok(Self::Antigravity),
            "grok" | "xai" => Ok(Self::Grok),
            _ => bail!("unsupported OAuth provider"),
        }
    }

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
        }
    }

    pub const fn cli_type(self) -> &'static str {
        match self {
            Self::OpenAi => "codex",
            Self::Anthropic => "claude",
            Self::Gemini => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::Grok => "xai",
        }
    }
}

fn string_field(value: &Value, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn value_field(value: &Value, names: &[&str]) -> Value {
    names
        .iter()
        .find_map(|name| value.get(*name).cloned())
        .unwrap_or(Value::Null)
}

pub fn build_auth_file(
    provider: OAuthProvider,
    credentials: &Value,
    prefix: Option<&str>,
) -> Result<Value> {
    if !credentials.is_object() {
        bail!("OAuth credentials must be an object");
    }
    let access = string_field(credentials, &["access_token", "accessToken"]);
    if access.is_empty() {
        bail!("OAuth credentials require access_token");
    }
    let refresh = string_field(credentials, &["refresh_token", "refreshToken"]);
    let id_token = string_field(credentials, &["id_token", "idToken"]);
    let email = string_field(credentials, &["email"]);
    let expired = string_field(
        credentials,
        &["expired", "expires_at", "expiresAt", "expiry"],
    );
    let last_refresh = string_field(credentials, &["last_refresh", "lastRefresh"]);
    let last_refresh = if last_refresh.is_empty() {
        "1970-01-01T00:00:00Z".to_owned()
    } else {
        last_refresh
    };

    let mut document = match provider {
        OAuthProvider::OpenAi => json!({
            "type": provider.cli_type(),
            "id_token": id_token,
            "access_token": access,
            "refresh_token": refresh,
            "account_id": string_field(credentials, &["account_id", "accountId", "chatgpt_account_id"]),
            "email": email,
            "last_refresh": last_refresh,
            "expired": expired,
        }),
        OAuthProvider::Anthropic => json!({
            "type": provider.cli_type(),
            "id_token": id_token,
            "access_token": access,
            "refresh_token": refresh,
            "email": email,
            "account_uuid": string_field(credentials, &["account_uuid", "accountUuid"]),
            "organization_uuid": string_field(credentials, &["organization_uuid", "organizationUuid"]),
            "organization_name": string_field(credentials, &["organization_name", "organizationName"]),
            "last_refresh": last_refresh,
            "expired": expired,
            "expires_in": value_field(credentials, &["expires_in", "expiresIn"]),
        }),
        OAuthProvider::Gemini | OAuthProvider::Antigravity => json!({
            "type": provider.cli_type(),
            "access_token": access,
            "refresh_token": refresh,
            "token_type": string_field(credentials, &["token_type", "tokenType"]),
            "expires_at": expired,
            "project_id": string_field(credentials, &["project_id", "projectId"]),
            "email": email,
            "last_refresh": last_refresh,
        }),
        OAuthProvider::Grok => json!({
            "type": provider.cli_type(),
            "access_token": access,
            "refresh_token": refresh,
            "id_token": id_token,
            "token_type": string_field(credentials, &["token_type", "tokenType"]),
            "expires_in": value_field(credentials, &["expires_in", "expiresIn"]),
            "expired": expired,
            "last_refresh": last_refresh,
            "email": email,
            "sub": string_field(credentials, &["sub", "subject"]),
            "base_url": string_field(credentials, &["base_url", "baseUrl"]),
            "redirect_uri": string_field(credentials, &["redirect_uri", "redirectUri"]),
            "token_endpoint": string_field(credentials, &["token_endpoint", "tokenEndpoint"]),
            "auth_kind": "oauth",
        }),
    };
    if let Some(prefix) = prefix.map(str::trim).filter(|value| !value.is_empty()) {
        document["prefix"] = Value::String(prefix.to_owned());
    }
    Ok(document)
}

pub fn stable_identity_source(credentials: &Value) -> Option<String> {
    [
        "account_id",
        "accountId",
        "chatgpt_account_id",
        "account_uuid",
        "accountUuid",
        "sub",
        "subject",
        "email",
        "project_id",
        "projectId",
        "refresh_token",
        "refreshToken",
        "access_token",
        "accessToken",
    ]
    .into_iter()
    .find_map(|name| {
        credentials
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_all_legacy_provider_auth_files() {
        let credentials = json!({
            "access_token": "synthetic-access",
            "refresh_token": "synthetic-refresh",
            "id_token": "synthetic-id",
            "account_id": "acct-test",
            "project_id": "project-test",
            "email": "oauth@example.invalid",
            "expires_at": "2030-01-01T00:00:00Z",
        });
        for (name, expected_type) in [
            ("openai", "codex"),
            ("anthropic", "claude"),
            ("gemini", "gemini-cli"),
            ("antigravity", "antigravity"),
            ("grok", "xai"),
        ] {
            let document =
                build_auth_file(OAuthProvider::parse(name).unwrap(), &credentials, None).unwrap();
            assert_eq!(document["type"], expected_type);
            assert_eq!(document["access_token"], "synthetic-access");
        }
    }

    #[test]
    fn rejects_unknown_provider_and_missing_access_token() {
        assert!(OAuthProvider::parse("unknown").is_err());
        assert!(build_auth_file(OAuthProvider::OpenAi, &json!({}), None).is_err());
    }
}
