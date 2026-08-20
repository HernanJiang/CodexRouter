//! Management API client for the private CLIProxyAPI process.

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use std::collections::HashSet;
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
        let method_name = method.as_str().to_owned();
        let safe_path = path.split('?').next().unwrap_or(path);
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
            bail!("CLI management {method_name} {safe_path} returned {status}");
        }
        Ok(value)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, Some(body)).await
    }

    /// Execute a management POST while preserving only the HTTP status on
    /// failures. This is used by account probes so an upstream response body
    /// can never enter Router errors or logs.
    pub async fn post_status(
        &self,
        path: &str,
        body: Value,
    ) -> Result<(StatusCode, Option<Value>)> {
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.secret)
            .header("X-Request-ID", uuid::Uuid::now_v7().to_string())
            .json(&body)
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Ok((status, None));
        }
        Ok((status, Some(response.json::<Value>().await?)))
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
            bail!("CR-CFG-0005: CLI config push returned {status}");
        }
        Ok(())
    }

    /// Push a config snapshot and return only after its expected models are
    /// visible in the live CLI registry. CLIProxyAPI's Windows watcher can
    /// coalesce rapid writes, so a successful management PUT alone is not an
    /// application acknowledgement.
    pub async fn put_config_yaml_and_wait_for_models(
        &self,
        yaml_text: &str,
        downstream_key: &str,
        expected_models: &[String],
    ) -> Result<()> {
        const MAX_PUSH_ATTEMPTS: usize = 3;
        const POLLS_PER_PUSH: usize = 12;
        const POLL_DELAY: Duration = Duration::from_millis(250);

        let expected = expected_models.iter().collect::<HashSet<_>>();
        for push_attempt in 0..MAX_PUSH_ATTEMPTS {
            self.put_config_yaml(yaml_text).await?;
            if expected.is_empty() {
                return Ok(());
            }

            for poll in 0..POLLS_PER_PUSH {
                if poll > 0 {
                    tokio::time::sleep(POLL_DELAY).await;
                }
                let response = self
                    .client
                    .get(format!("{}/v1/models", self.base_url))
                    .bearer_auth(downstream_key)
                    .header("X-Request-ID", uuid::Uuid::now_v7().to_string())
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                let Ok(response) = response else {
                    continue;
                };
                if !response.status().is_success() {
                    continue;
                }
                let Ok(value) = response.json::<Value>().await else {
                    continue;
                };
                let visible = value
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("id").and_then(Value::as_str))
                    .collect::<HashSet<_>>();
                if expected
                    .iter()
                    .all(|model| visible.contains(model.as_str()))
                {
                    return Ok(());
                }
            }

            if push_attempt + 1 < MAX_PUSH_ATTEMPTS {
                tokio::time::sleep(POLL_DELAY).await;
            }
        }

        bail!(
            "CR-CFG-0005: CLI runtime did not apply {} expected models",
            expected.len()
        )
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

    pub async fn model_registry(&self, downstream_key: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .bearer_auth(downstream_key)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .context("CLI model registry request failed")?;
        if !response.status().is_success() {
            bail!("CLI model registry returned {}", response.status());
        }
        let value = response
            .json::<Value>()
            .await
            .context("CLI model registry response was not valid JSON")?;
        if !value.get("data").is_some_and(Value::is_array) {
            bail!("CLI model registry response did not contain a model array");
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::StatusCode as AxumStatusCode,
        response::Json,
        routing::{get, put},
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn management_client_rejects_non_loopback_urls() {
        assert!(CliProxyManagementClient::new("http://127.0.0.1:18081", "s").is_ok());
        assert!(CliProxyManagementClient::new("http://0.0.0.0:18081", "s").is_err());
        assert!(CliProxyManagementClient::new("https://127.0.0.1:18081", "s").is_err());
    }

    #[tokio::test]
    async fn management_errors_never_include_authenticated_response_bodies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v0/management/auth-files",
                    get(|| async {
                        (
                            AxumStatusCode::UNAUTHORIZED,
                            r#"{"token":"secret-response-token","email":"private@example.com"}"#,
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let client =
            CliProxyManagementClient::new(format!("http://{address}"), "management-secret")
                .unwrap();

        let error = client.auth_files().await.unwrap_err().to_string();

        assert!(error.contains("GET /v0/management/auth-files"));
        assert!(error.contains("401 Unauthorized"));
        assert!(!error.contains("secret-response-token"));
        assert!(!error.contains("private@example.com"));
        server.abort();
    }

    #[tokio::test]
    async fn config_push_errors_never_include_authenticated_response_bodies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v0/management/config.yaml",
                    put(|| async {
                        (
                            AxumStatusCode::BAD_REQUEST,
                            "configuration rejected near api-key: secret-config-key",
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let client =
            CliProxyManagementClient::new(format!("http://{address}"), "management-secret")
                .unwrap();

        let error = client
            .put_config_yaml("api-key: request-secret")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("CR-CFG-0005"));
        assert!(error.contains("400 Bad Request"));
        assert!(!error.contains("secret-config-key"));
        server.abort();
    }

    #[tokio::test]
    async fn config_apply_repushes_until_expected_models_are_live() {
        let pushes = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let put_pushes = pushes.clone();
        let get_pushes = pushes.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/v0/management/config.yaml",
                        put(move || {
                            let pushes = put_pushes.clone();
                            async move {
                                pushes.fetch_add(1, Ordering::SeqCst);
                                AxumStatusCode::OK
                            }
                        }),
                    )
                    .route(
                        "/v1/models",
                        get(move || {
                            let pushes = get_pushes.clone();
                            async move {
                                let model = if pushes.load(Ordering::SeqCst) >= 2 {
                                    "cr_r4_antigravity/smoke-ag"
                                } else {
                                    "cr_r3_openai/smoke-image"
                                };
                                Json(serde_json::json!({"data":[{"id":model}]}))
                            }
                        }),
                    ),
            )
            .await
            .unwrap();
        });
        let client =
            CliProxyManagementClient::new(format!("http://{address}"), "management-secret")
                .unwrap();

        client
            .put_config_yaml_and_wait_for_models(
                "openai-compatibility: []",
                "downstream-secret",
                &["cr_r4_antigravity/smoke-ag".to_owned()],
            )
            .await
            .unwrap();

        assert_eq!(pushes.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
