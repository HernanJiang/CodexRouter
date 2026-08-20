//! Bounded JSONL telemetry used by every Router Host entrypoint.

use serde_json::{json, Map, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "1.0";
const SENSITIVE_KEY_MARKERS: &[&str] = &[
    "authorization",
    "cookie",
    "api_key",
    "api-key",
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "refresh",
    "access",
    "code",
    "state",
    "email",
    "prompt",
    "body",
    "response",
    "output",
    "user",
    "authorization_code",
];

#[derive(Debug)]
pub struct StructuredLogger {
    path: PathBuf,
    sink: Mutex<File>,
}

impl StructuredLogger {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let sink = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            sink: Mutex::new(sink),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, mut event: Value) -> Result<(), String> {
        sanitize_event(&mut event);
        let mut line = serde_json::to_vec(&event).map_err(|error| error.to_string())?;
        line.push(b'\n');
        let mut sink = self.sink.lock().unwrap_or_else(|error| error.into_inner());
        sink.write_all(&line).map_err(|error| error.to_string())
    }

    pub fn event(&self, builder: LogEventBuilder) -> Result<(), String> {
        self.write(builder.build())
    }
}

pub fn request_id() -> String {
    Uuid::now_v7().to_string()
}

pub fn accepted_request_id(candidate: Option<&str>) -> Result<String, String> {
    let Some(candidate) = candidate.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(request_id());
    };
    let valid_length = (8..=128).contains(&candidate.len());
    let valid_chars = candidate
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid_length && valid_chars {
        Ok(candidate.to_owned())
    } else {
        Err("invalid external request id".to_owned())
    }
}

#[must_use = "a terminal span must be emitted exactly once"]
pub struct TerminalSpanGuard {
    logger: Option<Arc<StructuredLogger>>,
    request_id: String,
    interface: String,
    terminal_event: Value,
}

impl TerminalSpanGuard {
    pub fn new(
        logger: Arc<StructuredLogger>,
        request_id: impl Into<String>,
        interface: impl Into<String>,
    ) -> Self {
        Self {
            logger: Some(logger),
            request_id: request_id.into(),
            interface: interface.into(),
            terminal_event: Value::Null,
        }
    }

    pub fn complete(
        mut self,
        status: &str,
        http_status: Option<u16>,
        error_code: Option<&str>,
        attempts: u32,
    ) -> Result<(), String> {
        self.terminal_event = json!({
            "schema_version": SCHEMA_VERSION,
            "timestamp": timestamp(),
            "level": if error_code.is_none() { "INFO" } else { "ERROR" },
            "event": "request.completed",
            "request_id": self.request_id,
            "trace_id": self.request_id,
            "span_id": "request-end",
            "parent_span_id": "ingress",
            "interface_name": self.interface,
            "method": Value::Null,
            "path_template": Value::Null,
            "chain_node": "request_end",
            "status": status,
            "http_status": http_status,
            "attempts": attempts,
            "error_code": error_code,
            "error_description": error_code.map(|code| format!("terminal error {code}")),
            "retryable": false,
        });
        let logger = self.logger.take().expect("guard consumed once");
        logger.write(self.terminal_event.clone())
    }
}

impl Drop for TerminalSpanGuard {
    fn drop(&mut self) {
        if let Some(logger) = self.logger.take() {
            let _ = logger.write(json!({
                "schema_version": SCHEMA_VERSION,
                "timestamp": timestamp(),
                "level": "ERROR",
                "event": "request.completed",
                "request_id": self.request_id,
                "trace_id": self.request_id,
                "span_id": "request-end",
                "parent_span_id": "ingress",
                "interface_name": self.interface,
                "chain_node": "request_end",
                "status": "internal_error",
                "http_status": 500,
                "attempts": 1,
                "error_code": "CR-SYS-0001",
                "error_description": "terminal span was not completed explicitly",
                "retryable": false,
            }));
        }
    }
}

pub struct LogEventBuilder {
    request_id: String,
    trace_id: String,
    interface: String,
    event: String,
    chain_node: String,
    fields: Map<String, Value>,
}

impl LogEventBuilder {
    pub fn new(
        request_id: impl Into<String>,
        interface: impl Into<String>,
        event: &str,
        chain_node: &str,
    ) -> Self {
        let request_id = request_id.into();
        Self {
            trace_id: request_id.clone(),
            request_id,
            interface: interface.into(),
            event: event.to_owned(),
            chain_node: chain_node.to_owned(),
            fields: Map::new(),
        }
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.fields.insert(key.to_owned(), value);
        self
    }

    pub fn build(self) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "timestamp": timestamp(),
            "level": self.fields.get("level").cloned().unwrap_or_else(|| "INFO".into()),
            "event": self.event,
            "request_id": self.request_id,
            "trace_id": self.trace_id,
            "span_id": self.chain_node,
            "parent_span_id": "ingress",
            "interface_name": self.interface,
            "method": self.fields.get("method").cloned().unwrap_or(Value::Null),
            "path_template": self.fields.get("path_template").cloned().unwrap_or(Value::Null),
            "chain_node": self.chain_node,
            "input_summary": self.fields.get("input_summary").cloned().unwrap_or(Value::Null),
            "provider": self.fields.get("provider").cloned().unwrap_or(Value::Null),
            "pool_id": self.fields.get("pool_id").cloned().unwrap_or(Value::Null),
            "account_id": self.fields.get("account_id").cloned().unwrap_or(Value::Null),
            "attempt": self.fields.get("attempt").cloned().unwrap_or(1.into()),
            "status": self.fields.get("status").cloned().unwrap_or_else(|| "ok".into()),
            "http_status": self.fields.get("http_status").cloned().unwrap_or(Value::Null),
            "elapsed_ms": self.fields.get("elapsed_ms").cloned().unwrap_or(Value::Null),
            "error_code": self.fields.get("error_code").cloned().unwrap_or(Value::Null),
            "error_description": self.fields.get("error_description").cloned().unwrap_or(Value::Null),
            "retryable": self.fields.get("retryable").cloned().unwrap_or(false.into()),
        })
    }
}

pub fn timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let seconds = (millis / 1000) as i64;
    let fraction = (millis % 1000) * 1_000_000;
    chrono::DateTime::from_timestamp(seconds, fraction as u32)
        .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned())
}

pub fn sanitize_event(event: &mut Value) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    for (key, value) in object.iter_mut() {
        if is_sensitive_key(key) {
            *value = Value::String("[REDACTED]".to_owned());
        } else {
            sanitize_value(value);
        }
    }
}

fn sanitize_value(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(sanitize_value),
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if is_sensitive_key(key) {
                    let marker = if key.to_ascii_lowercase().contains("email") {
                        "[REDACTED_EMAIL]"
                    } else {
                        "[REDACTED]"
                    };
                    *child = Value::String(marker.to_owned());
                } else {
                    sanitize_value(child);
                }
            }
        }
        Value::String(text) => *text = redact_text(text),
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    // Router error catalog codes (CR-*) are public diagnostics by design;
    // the generic "code" marker must not swallow them.
    if normalized == "error_code" {
        return false;
    }
    SENSITIVE_KEY_MARKERS.iter().any(|marker| {
        let marker = marker.replace('-', "_");
        normalized == marker
            || normalized.ends_with(&format!("_{marker}"))
            || normalized.starts_with(&format!("{marker}_"))
    })
}

pub fn redact_text(text: &str) -> String {
    let mut output = text.to_owned();
    for marker in ["Bearer ", "sk-", "xai-", "github_pat_", "GOCSPX-"] {
        if let Some(index) = output.find(marker) {
            let end = output[index..]
                .char_indices()
                .find(|(_, character)| character.is_whitespace())
                .map(|(offset, _)| index + offset)
                .unwrap_or(output.len());
            output.replace_range(index..end, "[REDACTED]");
        }
    }
    let email =
        regex::Regex::new(r"(?i)[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}").expect("valid email regex");
    output = email.replace_all(&output, "[REDACTED_EMAIL]").into_owned();
    let oauth_query =
        regex::Regex::new(r#"(?i)([?&](?:code|state)=)[^&\s"]+"#).expect("valid OAuth query regex");
    output = oauth_query
        .replace_all(&output, "$1[REDACTED]")
        .into_owned();
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_builder_and_redaction_keep_schema_and_hide_secrets() {
        let mut event = LogEventBuilder::new("req-redact", "test", "request.received", "ingress")
            .with(
                "input_summary",
                json!({
                    "model": "test",
                    "authorization": "Bearer abc",
                    "nested": { "refresh_token": "secret", "email": "user@example.com" }
                }),
            )
            .build();
        sanitize_event(&mut event);
        assert_eq!(event["schema_version"], SCHEMA_VERSION);
        assert_eq!(event["input_summary"]["authorization"], "[REDACTED]");
        assert_eq!(
            event["input_summary"]["nested"]["refresh_token"],
            "[REDACTED]"
        );
        assert_eq!(
            event["input_summary"]["nested"]["email"],
            "[REDACTED_EMAIL]"
        );
    }

    #[test]
    fn oauth_callback_query_and_account_email_are_redacted() {
        let redacted =
            redact_text("GET /callback?code=one-time-code&state=session-state user@example.com");
        assert!(!redacted.contains("one-time-code"));
        assert!(!redacted.contains("session-state"));
        assert!(!redacted.contains("user@example.com"));
        assert!(redacted.contains("code=[REDACTED]"));
    }

    #[test]
    fn router_error_codes_survive_sanitization() {
        let mut event =
            LogEventBuilder::new("req-code", "test", "request.completed", "request-end")
                .with("error_code", json!("CR-RTE-0001"))
                .with(
                    "input_summary",
                    json!({"authorization_code": "oauth-secret", "code": "raw"}),
                )
                .build();
        sanitize_event(&mut event);
        assert_eq!(event["error_code"], "CR-RTE-0001");
        assert_eq!(event["input_summary"]["authorization_code"], "[REDACTED]");
        assert_eq!(event["input_summary"]["code"], "[REDACTED]");
    }

    #[test]
    fn terminal_guard_emits_exactly_one_terminal_event() {
        let root = std::env::temp_dir().join(format!(
            "router-log-{}-{uuid}",
            std::process::id(),
            uuid = Uuid::now_v7()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("events.jsonl");
        let logger = Arc::new(StructuredLogger::open(&path).unwrap());
        let guard = TerminalSpanGuard::new(logger.clone(), "req-once", "test");
        guard.complete("ok", Some(200), None, 1).unwrap();
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 1);
        assert!(lines.contains("\"event\":\"request.completed\""));
    }
}
