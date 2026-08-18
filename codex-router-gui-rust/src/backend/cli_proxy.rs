//! Management API client for the private CLIProxyAPI process.

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct CliProxyManagementClient {
    base_url: String,
    secret: String,
    client: Client,
}

impl CliProxyManagementClient {
    pub fn new(base_url: impl Into<String>, secret: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into();
        let parsed = url::Url::parse(&base_url)?;
        if parsed.scheme() != "http"
            || parsed
                .host_str()
                .is_none_or(|host| host != "127.0.0.1" && host != "localhost")
        {
            bail!("CLIProxyAPI management URL must be loopback HTTP");
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            secret: secret.into(),
            client,
        })
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.secret)
            .header("X-Request-ID", uuid::Uuid::now_v7().to_string());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let value = response.json::<Value>().await?;
        if !status.is_success() {
            bail!("CLI management {path} returned {status}: {value}");
        }
        Ok(value)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, Some(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::PUT, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.request(Method::DELETE, path, None).await
    }

    /// Push a full YAML snapshot to `/v0/management/config.yaml`.
    /// The CLI validates the document, writes it in place and triggers its
    /// own watcher-driven client reload. Raw body: the endpoint expects YAML,
    /// not the JSON envelope used by the structured helpers above.
    pub async fn put_config_yaml(&self, yaml_text: &str) -> Result<()> {
        let response = self
            .client
            .put(format!("{}/v0/management/config.yaml", self.base_url))
            .bearer_auth(&self.secret)
            .header("X-Request-ID", uuid::Uuid::now_v7().to_string())
            .header(reqwest::header::CONTENT_TYPE, "application/yaml")
            .body(yaml_text.to_owned())
            .send()
            .await
            .context("CLIProxyAPI config push failed")?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            let sanitized: String = detail.chars().take(200).collect();
            bail!("CR-CFG-0005: CLI config push returned {status}: {sanitized}");
        }
        Ok(())
    }

    pub async fn health(&self) -> Result<()> {
        let response = self
            .client
            .get(format!("{}/healthz", self.base_url))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .context("CLIProxyAPI health request failed")?;
        if response.status() != StatusCode::OK {
            bail!("CLIProxyAPI health returned {}", response.status());
        }
        Ok(())
    }

    pub async fn plugins(&self) -> Result<Value> {
        self.get("/v0/management/plugins").await
    }

    pub async fn auth_files(&self) -> Result<Value> {
        self.get("/v0/management/auth-files").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_client_rejects_non_loopback_urls() {
        assert!(CliProxyManagementClient::new("http://127.0.0.1:18081", "s").is_ok());
        assert!(CliProxyManagementClient::new("http://0.0.0.0:18081", "s").is_err());
        assert!(CliProxyManagementClient::new("https://127.0.0.1:18081", "s").is_err());
    }
}
