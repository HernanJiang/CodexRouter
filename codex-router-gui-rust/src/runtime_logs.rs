use eframe::egui;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const INITIAL_TAIL_BYTES: u64 = 128 * 1024;
const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_PENDING_BYTES: usize = 64 * 1024;
const INITIAL_LINES_PER_SOURCE: usize = 30;
const MAX_RECORD_BYTES: usize = 4 * 1024;
const MAX_BATCH_BYTES: usize = 32 * 1024;
const MAX_BATCH_RECORDS: usize = 64;
const DROPPED_SUMMARY_RESERVE_BYTES: usize = 256;
const RUNTIME_LOG_CHANNEL_CAPACITY: usize = 8;

#[derive(Debug)]
pub(crate) struct RuntimeLogBatch {
    records: Vec<String>,
}

impl RuntimeLogBatch {
    pub(crate) fn into_records(self) -> Vec<String> {
        self.records
    }

    #[cfg(test)]
    fn byte_len(&self) -> usize {
        self.records.iter().map(|record| record.len() + 1).sum()
    }
}

pub(crate) fn bounded_channel() -> (SyncSender<RuntimeLogBatch>, Receiver<RuntimeLogBatch>) {
    sync_channel(RUNTIME_LOG_CHANNEL_CAPACITY)
}

struct BatchEmitter {
    tx: SyncSender<RuntimeLogBatch>,
    ctx: egui::Context,
    records: Vec<String>,
    bytes: usize,
    dropped_records: u64,
    dropped_batches: u64,
    dropped_bytes: u64,
    disconnected: bool,
}

impl BatchEmitter {
    fn new(tx: SyncSender<RuntimeLogBatch>, ctx: egui::Context) -> Self {
        Self {
            tx,
            ctx,
            records: Vec::with_capacity(MAX_BATCH_RECORDS),
            bytes: 0,
            dropped_records: 0,
            dropped_batches: 0,
            dropped_bytes: 0,
            disconnected: false,
        }
    }

    fn push(&mut self, record: String) {
        if self.disconnected {
            return;
        }

        let record_bytes = record.len().saturating_add(1);
        let payload_limit = MAX_BATCH_BYTES - DROPPED_SUMMARY_RESERVE_BYTES;
        let payload_records = MAX_BATCH_RECORDS - 1;
        if record_bytes > payload_limit {
            self.note_dropped(1, 1, record_bytes as u64);
            return;
        }
        if !self.records.is_empty()
            && (self.records.len() >= payload_records
                || self.bytes.saturating_add(record_bytes) > payload_limit)
        {
            self.flush();
        }
        self.bytes = self.bytes.saturating_add(record_bytes);
        self.records.push(record);
        if self.records.len() >= payload_records || self.bytes >= payload_limit {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.disconnected || (self.records.is_empty() && self.dropped_records == 0) {
            return;
        }

        let actual_records = self.records.len() as u64;
        let actual_bytes = self.bytes as u64;
        let mut records = std::mem::take(&mut self.records);
        self.bytes = 0;
        if self.dropped_records > 0 {
            let summary = dropped_summary(
                self.dropped_records,
                self.dropped_batches,
                self.dropped_bytes,
            );
            debug_assert!(summary.len() < DROPPED_SUMMARY_RESERVE_BYTES);
            records.insert(0, summary);
        }

        match self.tx.try_send(RuntimeLogBatch { records }) {
            Ok(()) => {
                self.dropped_records = 0;
                self.dropped_batches = 0;
                self.dropped_bytes = 0;
                self.ctx.request_repaint();
            }
            Err(TrySendError::Full(_)) => {
                if actual_records > 0 {
                    self.note_dropped(actual_records, 1, actual_bytes);
                }
                self.ctx.request_repaint();
            }
            Err(TrySendError::Disconnected(_)) => {
                self.disconnected = true;
            }
        }
    }

    fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    fn note_dropped(&mut self, records: u64, batches: u64, bytes: u64) {
        self.dropped_records = self.dropped_records.saturating_add(records);
        self.dropped_batches = self.dropped_batches.saturating_add(batches);
        self.dropped_bytes = self.dropped_bytes.saturating_add(bytes);
    }
}

fn dropped_summary(records: u64, batches: u64, bytes: u64) -> String {
    format!(
        "[Runtime log reader] class=queue_overflow | dropped_records={records} | dropped_batches={batches} | dropped_bytes={bytes}"
    )
}

#[derive(Clone, Copy)]
enum LogKind {
    Sub2Api,
    PostgreSql,
    Redis,
    Stderr,
}

struct LogSource {
    label: &'static str,
    path: PathBuf,
    kind: LogKind,
    offset: u64,
    pending: Vec<u8>,
    discarding_oversized_line: bool,
    initialized: bool,
    last_read_error: Option<String>,
}

impl LogSource {
    fn new(root: &Path, label: &'static str, relative: &'static str, kind: LogKind) -> Self {
        Self {
            label,
            path: root.join("logs").join(relative),
            kind,
            offset: 0,
            pending: Vec::new(),
            discarding_oversized_line: false,
            initialized: false,
            last_read_error: None,
        }
    }

    fn read_new_lines(&mut self) -> std::io::Result<(Vec<String>, bool)> {
        if !self.path.is_file() {
            self.initialized = false;
            self.offset = 0;
            self.pending.clear();
            self.discarding_oversized_line = false;
            return Ok((Vec::new(), false));
        }

        let mut file = File::open(&self.path)?;
        let length = file.metadata()?.len();
        let initial_read = !self.initialized;
        let mut discard_partial_prefix = false;

        if initial_read {
            self.initialized = true;
            self.offset = length.saturating_sub(INITIAL_TAIL_BYTES);
            discard_partial_prefix = self.offset > 0;
        } else if length < self.offset {
            self.offset = 0;
            self.pending.clear();
            self.discarding_oversized_line = false;
        }

        if length <= self.offset {
            return Ok((Vec::new(), initial_read));
        }

        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::with_capacity((length - self.offset).min(MAX_READ_BYTES) as usize);
        let read = file.take(MAX_READ_BYTES).read_to_end(&mut bytes)?;
        self.offset += read as u64;

        if discard_partial_prefix {
            if let Some(index) = bytes.iter().position(|byte| *byte == b'\n') {
                bytes.drain(..=index);
            } else {
                self.discarding_oversized_line = true;
                return Ok((Vec::new(), initial_read));
            }
        }

        if self.discarding_oversized_line {
            if let Some(index) = bytes.iter().position(|byte| *byte == b'\n') {
                bytes.drain(..=index);
                self.discarding_oversized_line = false;
            } else {
                return Ok((Vec::new(), initial_read));
            }
        }

        self.pending.extend_from_slice(&bytes);
        let complete_end = self.pending.iter().rposition(|byte| *byte == b'\n');
        let Some(complete_end) = complete_end else {
            if self.pending.len() > MAX_PENDING_BYTES {
                self.pending.clear();
                self.discarding_oversized_line = true;
            }
            return Ok((Vec::new(), initial_read));
        };

        let complete = self.pending.drain(..=complete_end).collect::<Vec<_>>();
        if self.pending.len() > MAX_PENDING_BYTES {
            self.pending.clear();
            self.discarding_oversized_line = true;
        }
        let text = String::from_utf8_lossy(&complete);
        let lines = text
            .lines()
            .filter(|line| line.len() <= MAX_PENDING_BYTES)
            .map(|line| line.trim_end_matches('\r').to_owned())
            .collect();
        Ok((lines, initial_read))
    }
}

pub(crate) fn spawn(
    router_root: PathBuf,
    tx: SyncSender<RuntimeLogBatch>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut sources = vec![
            LogSource::new(&router_root, "Sub2API", "sub2api.log", LogKind::Sub2Api),
            LogSource::new(
                &router_root,
                "Sub2API stderr",
                "sub2api-stderr.log",
                LogKind::Stderr,
            ),
            LogSource::new(
                &router_root,
                "PostgreSQL",
                "postgres.log",
                LogKind::PostgreSql,
            ),
            LogSource::new(&router_root, "Redis", "redis-stdout.log", LogKind::Redis),
            LogSource::new(
                &router_root,
                "Redis stderr",
                "redis-stderr.log",
                LogKind::Stderr,
            ),
            LogSource::new(
                &router_root,
                "OAuth stderr",
                "oauth-stderr.log",
                LogKind::Stderr,
            ),
            LogSource::new(
                &router_root,
                "Auth adapter",
                "codex-auth-adapter-stderr.log",
                LogKind::Stderr,
            ),
        ];
        let mut emitter = BatchEmitter::new(tx, ctx);

        while !stop.load(Ordering::Relaxed) {
            if paused.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(750));
                continue;
            }
            for source in &mut sources {
                match source.read_new_lines() {
                    Ok((lines, initial_read)) => {
                        source.last_read_error = None;
                        if initial_read {
                            let mut recent = VecDeque::with_capacity(INITIAL_LINES_PER_SOURCE);
                            for line in lines {
                                if let Some(record) =
                                    format_diagnostic_line(source.label, source.kind, &line)
                                {
                                    if recent.len() == INITIAL_LINES_PER_SOURCE {
                                        recent.pop_front();
                                    }
                                    recent.push_back(record);
                                }
                            }
                            for record in recent {
                                emitter.push(record);
                            }
                        } else {
                            for line in lines {
                                if let Some(record) =
                                    format_diagnostic_line(source.label, source.kind, &line)
                                {
                                    emitter.push(record);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if source.last_read_error.as_deref() != Some(message.as_str()) {
                            emitter.push(format!(
                                "[Log reader/{}] {}",
                                source.label,
                                summarize_error_for_display(&message)
                            ));
                            source.last_read_error = Some(message);
                        }
                    }
                }
                if emitter.is_disconnected() {
                    break;
                }
            }

            emitter.flush();
            if emitter.is_disconnected() {
                break;
            }

            let mut waited = Duration::ZERO;
            while waited < Duration::from_millis(750) && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
                waited += Duration::from_millis(50);
            }
        }
    });
}

pub(crate) fn signals_router_health_failure(record: &str) -> bool {
    let normalized = record.to_ascii_lowercase();
    if normalized.contains("class=upstream")
        && !normalized.contains("database")
        && !normalized.contains("postgres")
    {
        return false;
    }
    let service_failure = normalized.contains("database")
        || normalized.contains("postgres")
        || normalized.contains("connection refused")
        || normalized.contains("connection reset")
        || normalized.contains("context deadline exceeded")
        || normalized.contains("i/o timeout")
        || normalized.contains("timed out");
    let relevant_source = normalized.contains("sub2api")
        || normalized.contains("postgresql")
        || normalized.contains("class=database")
        || normalized.contains("class=timeout")
        || normalized.contains("class=connection");
    service_failure && relevant_source
}

fn format_diagnostic_line(label: &str, kind: LogKind, line: &str) -> Option<String> {
    if line.trim().is_empty() {
        return None;
    }
    match kind {
        LogKind::Sub2Api => format_sub2api_line(label, line),
        LogKind::PostgreSql => {
            is_postgres_diagnostic(line).then(|| format_plain_diagnostic(label, line))
        }
        LogKind::Redis => is_redis_diagnostic(line).then(|| format_plain_diagnostic(label, line)),
        LogKind::Stderr => Some(format_plain_diagnostic(label, line)),
    }
}

fn format_sub2api_line(label: &str, line: &str) -> Option<String> {
    let mut fields = line.splitn(6, '\t');
    let timestamp = fields.next().unwrap_or_default().trim();
    let level = fields.next().unwrap_or_default().trim();
    let _logger = fields.next();
    let _location = fields.next();
    let event = fields.next().unwrap_or_default().trim();
    let payload = fields.next().unwrap_or_default().trim();

    if event.is_empty() {
        return contains_error_signal(line).then(|| format_plain_diagnostic(label, line));
    }

    let json = serde_json::from_str::<Value>(payload).ok();
    let status_error = json.as_ref().is_some_and(has_error_status);
    let diagnostic = is_warning_level(level)
        || status_error
        || contains_error_signal(event)
        || json
            .as_ref()
            .and_then(|value| value.get("error"))
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
    if !diagnostic {
        return None;
    }

    let mut output = String::new();
    if let Some(timestamp) = safe_timestamp(timestamp) {
        output.push_str(&timestamp);
        output.push(' ');
    }
    output.push('[');
    output.push_str(label);
    if is_known_level(level) {
        output.push('/');
        output.push_str(&level.to_ascii_uppercase());
    }
    output.push_str("] ");
    output.push_str(&safe_event_name(event));

    if let Some(Value::Object(object)) = json.as_ref() {
        if let Some(value) = object
            .get("status_code")
            .or_else(|| object.get("status"))
            .and_then(safe_http_status_value)
        {
            append_field(&mut output, "status_code", &value);
        }
        if let Some(value) = object
            .get("upstream_status")
            .and_then(safe_http_status_value)
        {
            append_field(&mut output, "upstream_status", &value);
        }
        if let Some(value) = object.get("latency_ms").and_then(safe_latency_value) {
            append_field(&mut output, "latency_ms", &value);
        }
        for key in ["request_id", "client_request_id"] {
            if let Some(value) = object
                .get(key)
                .and_then(Value::as_str)
                .and_then(safe_request_id)
            {
                append_field(&mut output, key, &value);
            }
        }
        if let Some(value) = object
            .get("stage")
            .and_then(Value::as_str)
            .and_then(safe_stage)
        {
            append_field(&mut output, "stage", &value);
        }
        if let Some(value) = object
            .get("platform")
            .and_then(Value::as_str)
            .and_then(safe_platform)
        {
            append_field(&mut output, "platform", &value);
        }
        if let Some(value) = object
            .get("model")
            .and_then(Value::as_str)
            .and_then(safe_model_name)
        {
            append_field(&mut output, "model", &value);
        }
        if let Some(value) = object
            .get("method")
            .and_then(Value::as_str)
            .and_then(safe_http_method)
        {
            append_field(&mut output, "method", &value);
        }
        if let Some(path) = object
            .get("path")
            .and_then(Value::as_str)
            .and_then(safe_http_path)
        {
            append_field(&mut output, "path", path);
        }
    }

    let mut classes = error_classes(event);
    if let Some(Value::Object(object)) = json.as_ref() {
        for key in [
            "error_code",
            "error",
            "reason",
            "detail",
            "message",
            "error_message",
            "upstream_error",
            "upstream_error_message",
        ] {
            if let Some(value) = object.get(key) {
                extend_error_classes(&mut classes, &value.to_string());
            }
        }
        for value in [
            object.get("status_code").or_else(|| object.get("status")),
            object.get("upstream_status"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(status) =
                safe_http_status_value(value).and_then(|value| value.parse::<u16>().ok())
            {
                extend_classes_from_status(&mut classes, status);
            }
        }
    }
    if classes.len() > 1 {
        classes.retain(|class| !matches!(*class, "request_failure" | "warning"));
    }
    if classes.is_empty() {
        classes.push(if is_warning_level(level) {
            "warning"
        } else {
            "request_failure"
        });
    }
    append_field(&mut output, "class", &classes.join("+"));

    Some(limit_utf8_bytes(
        &redact_for_display(&output),
        MAX_RECORD_BYTES,
    ))
}

fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value)
            if !value.is_empty()
                && value.len() <= 20
                && value.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            value.parse().ok()
        }
        _ => None,
    }
}

fn safe_http_status_value(value: &Value) -> Option<String> {
    value_as_u64(value)
        .filter(|status| (100..=599).contains(status))
        .map(|status| status.to_string())
}

fn safe_latency_value(value: &Value) -> Option<String> {
    value_as_u64(value)
        .filter(|latency| *latency <= 86_400_000)
        .map(|latency| latency.to_string())
}

fn append_field(output: &mut String, key: &str, value: &str) {
    output.push_str(" | ");
    output.push_str(key);
    output.push('=');
    output.push_str(value);
}

fn safe_request_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= 20 && trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(trimmed.to_owned());
    }
    if is_canonical_uuid(trimmed) {
        return Some(trimmed.to_ascii_lowercase());
    }

    let digest = Sha256::digest(trimmed.as_bytes());
    let mut summary = String::with_capacity(23);
    summary.push_str("sha256:");
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        write!(&mut summary, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Some(summary)
}

fn is_canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn safe_stage(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "request"
            | "routing"
            | "validation"
            | "authentication"
            | "authorization"
            | "upstream_connect"
            | "upstream_request"
            | "upstream_response"
            | "stream"
            | "streaming"
            | "response"
            | "decode"
            | "encode"
            | "retry"
            | "fallback"
            | "health_check"
            | "startup"
            | "shutdown"
            | "database"
            | "cache"
    )
    .then_some(normalized)
}

fn safe_platform(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "openai"
            | "anthropic"
            | "gemini"
            | "google"
            | "antigravity"
            | "grok"
            | "xai"
            | "azure_openai"
            | "openrouter"
            | "bedrock"
            | "vertex"
    )
    .then_some(normalized)
}

fn safe_model_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 96
        || !trimmed.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
        || redact_for_display(trimmed) != trimmed
    {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn safe_http_method(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "CONNECT"
    )
    .then_some(normalized)
}

fn safe_http_path(value: &str) -> Option<&'static str> {
    match value.trim().trim_end_matches('/') {
        "/v1/responses" => Some("/v1/responses"),
        "/v1/chat/completions" => Some("/v1/chat/completions"),
        "/v1/completions" => Some("/v1/completions"),
        "/v1/messages" => Some("/v1/messages"),
        "/v1/models" => Some("/v1/models"),
        "/v1/embeddings" => Some("/v1/embeddings"),
        "/v1/images/generations" => Some("/v1/images/generations"),
        "/v1/audio/transcriptions" => Some("/v1/audio/transcriptions"),
        "/v1/audio/speech" => Some("/v1/audio/speech"),
        "/v1/realtime" => Some("/v1/realtime"),
        "/backend-api/codex/responses" => Some("/backend-api/codex/responses"),
        "/backend-api/codex/models" => Some("/backend-api/codex/models"),
        "/api/v1/auth/login" => Some("/api/v1/auth/login"),
        "/api/v1/admin/compliance" => Some("/api/v1/admin/compliance"),
        "/api/v1/admin/compliance/accept" => Some("/api/v1/admin/compliance/accept"),
        "/health" => Some("/health"),
        "/healthz" => Some("/healthz"),
        "/ready" => Some("/ready"),
        "/readyz" => Some("/readyz"),
        _ => None,
    }
}

fn safe_event_name(value: &str) -> String {
    let candidate = value
        .split([':', '\t', '{', '}'])
        .next()
        .unwrap_or_default()
        .trim();
    if candidate.is_empty()
        || candidate.len() > 96
        || !candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !candidate
            .bytes()
            .any(|byte| matches!(byte, b'.' | b'_' | b'-'))
        || redact_for_display(candidate) != candidate
    {
        "service_diagnostic".to_owned()
    } else {
        candidate.to_owned()
    }
}

fn has_error_status(value: &Value) -> bool {
    ["status_code", "upstream_status", "status"]
        .iter()
        .filter_map(|key| value.get(key))
        .filter_map(safe_http_status_value)
        .filter_map(|status| status.parse::<u16>().ok())
        .any(|status| status >= 400)
}

fn is_warning_level(level: &str) -> bool {
    matches!(
        level.trim().to_ascii_uppercase().as_str(),
        "WARN" | "WARNING" | "ERROR" | "FATAL" | "PANIC" | "CRITICAL"
    )
}

fn is_known_level(level: &str) -> bool {
    matches!(
        level.trim().to_ascii_uppercase().as_str(),
        "TRACE" | "DEBUG" | "INFO" | "WARN" | "WARNING" | "ERROR" | "FATAL" | "PANIC" | "CRITICAL"
    )
}

fn contains_error_signal(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        " error",
        "error:",
        "_error",
        "failed",
        "failure",
        "timeout",
        "timed out",
        "dial tcp",
        "connection refused",
        "connection reset",
        "connection aborted",
        "proxyconnect",
        "no such host",
        "tls handshake",
        "certificate",
        "unavailable",
        "错误",
        "失败",
        "超时",
        "拒绝",
        "断开",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_postgres_diagnostic(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    [" ERROR:", " FATAL:", " PANIC:", " WARNING:"]
        .iter()
        .any(|needle| upper.contains(needle))
}

fn is_redis_diagnostic(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.contains(" # ")
        || trimmed.ends_with(" #")
        || contains_error_signal(trimmed)
        || trimmed.to_ascii_lowercase().contains("warning")
}

fn format_plain_diagnostic(label: &str, line: &str) -> String {
    let mut output = String::new();
    if let Some(timestamp) = safe_timestamp(line) {
        output.push_str(&timestamp);
        output.push(' ');
    }
    output.push('[');
    output.push_str(label);
    output.push_str("] ");
    output.push_str(&summarize_error_for_display(&format!("{label} {line}")));
    limit_utf8_bytes(&redact_for_display(&output), MAX_RECORD_BYTES)
}

fn safe_timestamp(text: &str) -> Option<String> {
    static TIMESTAMP: OnceLock<Regex> = OnceLock::new();
    let regex = TIMESTAMP.get_or_init(|| {
        Regex::new(r"^(\d{4}-\d{2}-\d{2}[T ][0-9:.+-]+(?:Z| [A-Z]{2,5})?)")
            .expect("timestamp regex is valid")
    });
    regex
        .captures(text.trim_start())
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_owned())
}

pub(crate) fn summarize_error_for_display(text: &str) -> String {
    let mut classes = error_classes(text);
    if classes.is_empty() {
        classes.push("unclassified_error");
    }
    let mut output = format!("class={}", classes.join("+"));
    for status in safe_http_statuses(text) {
        append_field(&mut output, "status", &status);
    }
    output
}

fn error_classes(text: &str) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    let mut classes = Vec::new();

    if lower.contains("router_install_root_conflict") || lower.contains("install_root_conflict") {
        push_unique_class(&mut classes, "install_root_conflict");
    }
    if lower.contains("router_port_conflict") || lower.contains("port_conflict") {
        push_unique_class(&mut classes, "port_conflict");
    }

    if lower.contains("router_lifecycle_deferred") {
        push_unique_class(&mut classes, "lifecycle_deferred");
    }
    if lower.contains("router_lifecycle_busy") {
        push_unique_class(&mut classes, "lifecycle_busy");
    }
    if lower.contains("router_lifecycle_safety_check_failed") {
        push_unique_class(&mut classes, "lifecycle_safety_check_failed");
    }

    if contains_any(
        &lower,
        &["content_policy", "content policy", "内容审计", "风险规则"],
    ) {
        push_unique_class(&mut classes, "content_policy");
    }
    if contains_any(
        &lower,
        &["rate limit", "too many requests", "quota", "限流", "额度"],
    ) || lower.contains("429")
    {
        push_unique_class(&mut classes, "rate_limit");
    }
    if contains_any(
        &lower,
        &[
            "unauthorized",
            "authentication",
            "auth failed",
            "未授权",
            "认证失败",
        ],
    ) || lower.contains("401")
    {
        push_unique_class(&mut classes, "authentication");
    }
    if contains_any(
        &lower,
        &[
            "forbidden",
            "permission denied",
            "access denied",
            "无权限",
            "访问被拒",
        ],
    ) || lower.contains("403")
    {
        push_unique_class(&mut classes, "permission");
    }
    if contains_any(
        &lower,
        &["timeout", "timed out", "deadline exceeded", "超时"],
    ) {
        push_unique_class(&mut classes, "timeout");
    }
    if contains_any(&lower, &["context canceled", "context cancelled", "已取消"]) {
        push_unique_class(&mut classes, "cancelled");
    }
    if contains_any(
        &lower,
        &[
            "no such host",
            "name resolution",
            "dns",
            "lookup ",
            "域名解析",
        ],
    ) {
        push_unique_class(&mut classes, "dns");
    }
    if contains_any(
        &lower,
        &["proxyconnect", "proxy connect", "proxy error", "代理"],
    ) {
        push_unique_class(&mut classes, "proxy");
    }
    if contains_any(&lower, &["tls", "certificate", "x509", "证书"]) {
        push_unique_class(&mut classes, "tls");
    }
    if contains_any(
        &lower,
        &[
            "connection refused",
            "actively refused",
            "connectex",
            "连接被拒",
        ],
    ) {
        push_unique_class(&mut classes, "connection_refused");
    }
    if contains_any(
        &lower,
        &[
            "connection reset",
            "connection aborted",
            "broken pipe",
            "unexpected eof",
            "wsarecv",
            "连接中断",
            "断开",
        ],
    ) {
        push_unique_class(&mut classes, "connection_closed");
    }
    if contains_any(&lower, &["websocket", "web socket", ".ws_", " ws "]) {
        push_unique_class(&mut classes, "websocket");
    }
    if contains_any(&lower, &["upstream", "上游"]) {
        push_unique_class(&mut classes, "upstream");
    }
    if contains_any(&lower, &["postgres", "database", "sqlstate", "数据库"]) {
        push_unique_class(&mut classes, "database");
    }
    if lower.contains("redis") {
        push_unique_class(&mut classes, "redis");
    }
    if contains_any(
        &lower,
        &[
            "configuration",
            "config ",
            "invalid",
            "parse error",
            "deserialize",
            "missing required",
            "配置",
            "缺少",
            "模型",
        ],
    ) {
        push_unique_class(&mut classes, "configuration");
    }
    if contains_any(
        &lower,
        &["no space", "disk full", "read-only file", "storage", "磁盘"],
    ) {
        push_unique_class(&mut classes, "storage");
    }
    if contains_any(&lower, &["dial tcp", "network", "transport", "网络"]) {
        push_unique_class(&mut classes, "network");
    }
    if classes.is_empty()
        && contains_any(
            &lower,
            &[
                "error", "failed", "failure", "fatal", "错误", "失败", "无法",
            ],
        )
    {
        push_unique_class(&mut classes, "request_failure");
    }
    classes
}

fn push_unique_class(classes: &mut Vec<&'static str>, class: &'static str) {
    if !classes.contains(&class) {
        classes.push(class);
    }
}

fn extend_error_classes(classes: &mut Vec<&'static str>, text: &str) {
    for class in error_classes(text) {
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
}

fn extend_classes_from_status(classes: &mut Vec<&'static str>, status: u16) {
    let class = match status {
        401 => Some("authentication"),
        403 => Some("permission"),
        429 => Some("rate_limit"),
        500..=599 => Some("upstream"),
        _ => None,
    };
    if let Some(class) = class {
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn safe_http_statuses(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    if !contains_any(&lower, &["http", "status", "upstream", "响应", "状态码"]) {
        return Vec::new();
    }
    static STATUS: OnceLock<Regex> = OnceLock::new();
    let regex =
        STATUS.get_or_init(|| Regex::new(r"\b([45][0-9]{2})\b").expect("status regex is valid"));
    let mut values = Vec::new();
    for capture in regex.captures_iter(text).take(4) {
        let value = capture[1].to_owned();
        if !values.contains(&value) {
            values.push(value);
        }
    }
    values
}

fn limit_utf8_bytes(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    if maximum < 3 {
        return String::new();
    }

    let mut end = maximum - 3;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut shortened = String::with_capacity(end + 3);
    shortened.push_str(&value[..end]);
    shortened.push_str("...");
    shortened
}

fn redaction_rules() -> &'static Vec<(Regex, &'static str)> {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (
                r"(?i)\b((?:https?|wss?|socks5h?|postgres(?:ql)?|redis)://)[^@/\s]+@",
                "$1[REDACTED]@",
            ),
            (
                r"(?i)\b((?:https?|wss?|socks5h?|postgres(?:ql)?|redis)://[^\s?#]+)[?#][^\s]*",
                "$1?[REDACTED]",
            ),
            (
                r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]+",
                "$1 [REDACTED]",
            ),
            (
                r"\b(?:sk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{8,}|xai-[A-Za-z0-9_-]{8,}|AIza[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{12,}|hf_[A-Za-z0-9]{20,}|ya29\.[A-Za-z0-9_-]{20,}|(?:AKIA|ASIA)[A-Z0-9]{16})\b",
                "[REDACTED]",
            ),
            (
                r"\b[A-Za-z0-9_-]{5,}(?:\.[A-Za-z0-9_-]{1,}){4}\b",
                "[REDACTED]",
            ),
            (
                r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b",
                "[REDACTED]",
            ),
            (
                r"\b[A-Za-z0-9_]{32,}\b",
                "[REDACTED]",
            ),
            (
                r#"(?i)([\"']?(?:api[_-]?key|local[_-]?api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|auth[_-]?token|authorization[_-]?code|client[_-]?secret|jwt[_-]?secret|totp[_-]?(?:encryption[_-]?)?key|aws[_-]?secret[_-]?access[_-]?key|private[_-]?key|credential|secret|password|passwd|proxy[_-]?password|redis[_-]?password|database[_-]?password|admin[_-]?password|authorization|cookie|set-cookie|session[_-]?token|webhook)[\"']?\s*[:=]\s*)(\"[^\"]*\"|'[^']*'|[^\s,;&}]+)"#,
                "$1\"[REDACTED]\"",
            ),
            (
                r"(?i)([?&](?:api[_-]?key|access[_-]?token|refresh[_-]?token|code|client[_-]?secret)=)[^&#\s]+",
                "$1[REDACTED]",
            ),
            (
                r"(?is)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                "[REDACTED PRIVATE KEY]",
            ),
            (
                r"(?i)(--(?:proxy-user|user)\s+)[^\s]+",
                "$1[REDACTED]",
            ),
            (
                r#"(?i)\b[A-Z]:[\\/]+Users[\\/]+[^\\/\s"']+"#,
                "[USER_PROFILE]",
            ),
        ]
        .into_iter()
        .map(|(pattern, replacement)| {
            (
                Regex::new(pattern).expect("built-in redaction regex is valid"),
                replacement,
            )
        })
        .collect()
    })
}

pub(crate) fn redact_for_display(text: &str) -> String {
    redaction_rules()
        .iter()
        .fold(text.to_owned(), |value, (regex, replacement)| {
            regex.replace_all(&value, *replacement).into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_credentials_and_user_paths() {
        let raw = concat!(
            "Authorization: Bearer secret-bearer-value ",
            "api_key=plain-key password=plain-password ",
            "sk-proj-abcdefghijklmnopqrstuvwxyz ",
            "eyJheader12.eyJpayload34.signatur56 ",
            "onepart1.twopart2.threepart3.fourpart4.fivepart5 ",
            "ghp_abcdefghijklmnopqrstuvwxyz123456 ",
            "xoxb-1234567890-abcdefghijklmnop ",
            "sk_live_abcdefghijklmnopqrstuv ",
            "AKIAABCDEFGHIJKLMNOP ",
            "abcdefghijklmnopqrstuvwx12345678 ",
            "http://alice:proxy-password@127.0.0.1:8080/path ",
            "https://example.test/callback?code=oauth-code&access_token=query-token ",
            "https://example.test/callback#fragment-token ",
            "C:\\Users\\Alice\\router"
        );
        let safe = redact_for_display(raw);
        for secret in [
            "secret-bearer-value",
            "plain-key",
            "plain-password",
            "abcdefghijklmnopqrstuvwxyz",
            "eyJpayload34",
            "fivepart5",
            "ghp_",
            "xoxb-",
            "sk_live_",
            "AKIAABCDEFGHIJKLMNOP",
            "abcdefghijklmnopqrstuvwx12345678",
            "proxy-password",
            "oauth-code",
            "query-token",
            "fragment-token",
            "Alice",
        ] {
            assert!(!safe.contains(secret), "secret remained in: {safe}");
        }
        assert!(safe.contains("[REDACTED]"));
        assert!(safe.contains("[USER_PROFILE]"));
    }

    #[test]
    fn sub2api_errors_keep_diagnostics_but_drop_payloads() {
        let line = concat!(
            "2026-08-02T07:45:59.285Z\tWARN\thandler\tfile.go:1\topenai.forward_failed\t",
            r#"{"request_id":"request-123","status_code":502,"upstream_status":403,"stage":"stream","error":"upstream rejected request","request_body":"private prompt","api_key":"secret-key"}"#
        );
        let safe = format_sub2api_line("Sub2API", line).expect("diagnostic line");
        assert!(safe.contains("status_code=502"));
        assert!(safe.contains("upstream_status=403"));
        assert!(safe.contains("request_id=sha256:"));
        assert!(safe.contains("class="));
        assert!(safe.contains("upstream"));
        assert!(safe.contains("permission"));
        assert!(!safe.contains("request-123"));
        assert!(!safe.contains("upstream rejected request"));
        assert!(!safe.contains("private prompt"));
        assert!(!safe.contains("secret-key"));
        assert!(!safe.contains("request_body"));
    }

    #[test]
    fn successful_access_lines_are_not_forwarded() {
        let line = concat!(
            "2026-08-02T08:00:00Z\tINFO\tmiddleware\tlogger.go:82\thttp request completed\t",
            r#"{"status_code":200,"request_id":"ok"}"#
        );
        assert!(format_sub2api_line("Sub2API", line).is_none());
    }

    #[test]
    fn failed_info_lines_are_forwarded() {
        let line = concat!(
            "2026-08-02T08:00:00Z\tINFO\tservice\tworker.go:1\tbackground refresh failed\t",
            r#"{"error":"dial tcp: connection refused"}"#
        );
        let safe = format_sub2api_line("Sub2API", line).expect("failure line");
        assert!(safe.contains("class=connection_refused+network"));
        assert!(!safe.contains("dial tcp"));
    }

    #[test]
    fn only_local_service_failures_trigger_an_immediate_health_probe() {
        assert!(signals_router_health_failure(
            "[Sub2API] class=database+timeout | context deadline exceeded"
        ));
        assert!(signals_router_health_failure(
            "[PostgreSQL] connection reset by peer"
        ));
        assert!(!signals_router_health_failure(
            "[Sub2API] class=upstream | upstream provider timed out"
        ));
        assert!(!signals_router_health_failure(
            "[OAuth stderr] connection refused"
        ));
    }

    #[test]
    fn upstream_status_and_content_policy_codes_remain_actionable() {
        let line = concat!(
            "2026-08-02T08:00:00Z\tERROR\thandler\tgateway.go:1\topenai.forward_failed\t",
            r#"{"status_code":502,"upstream_status":403,"error_code":"content_policy_violation","request_id":"123e4567-e89b-12d3-a456-426614174000","error":"upstream response body: private user input"}"#
        );
        let safe = format_sub2api_line("Sub2API", line).expect("upstream error");
        assert!(safe.contains("status_code=502"));
        assert!(safe.contains("upstream_status=403"));
        assert!(!safe.contains("error_code="));
        assert!(!safe.contains("content_policy_violation"));
        assert!(safe.contains("request_id=123e4567-e89b-12d3-a456-426614174000"));
        assert!(safe.contains("content_policy"));
        assert!(safe.contains("upstream"));
        assert!(safe.contains("permission"));
        assert!(!safe.contains("private user input"));
        assert!(!safe.contains("response body"));
    }

    #[test]
    fn dynamic_fields_are_semantically_allowlisted() {
        let line = concat!(
            "2026-08-02T08:00:00Z\tERROR\thandler\tgateway.go:1\t",
            "customer Alice payment failed\t",
            r#"{"status":403,"upstream_status":999,"latency_ms":-1,"request_id":"short-secret-value","client_request_id":"12345","error_code":"content_policy_violation","stage":"private-stage","platform":"private-provider","model":"gpt-5.6-sol","method":"secret-token","path":"/v1/users/private-customer"}"#
        );
        let safe = format_sub2api_line("Sub2API", line).expect("diagnostic line");
        assert!(safe.contains("service_diagnostic"));
        assert!(safe.contains("status_code=403"));
        assert!(safe.contains("request_id=sha256:"));
        assert!(safe.contains("client_request_id=12345"));
        assert!(safe.contains("model=gpt-5.6-sol"));
        assert!(safe.contains("class=content_policy+permission"));
        for private in [
            "Alice",
            "payment",
            "short-secret-value",
            "content_policy_violation",
            "private-stage",
            "private-provider",
            "secret-token",
            "/v1/users/private-customer",
            "upstream_status=999",
            "latency_ms=",
        ] {
            assert!(!safe.contains(private), "private field remained in: {safe}");
        }
    }

    #[test]
    fn known_structured_fields_remain_actionable() {
        let line = concat!(
            "2026-08-02T08:00:00Z\tERROR\thandler\tgateway.go:1\topenai.forward_failed\t",
            r#"{"status_code":502,"latency_ms":1250,"request_id":"123E4567-E89B-12D3-A456-426614174000","stage":"upstream-connect","platform":"openai","model":"gpt-5.6-sol","method":"post","path":"/v1/responses"}"#
        );
        let safe = format_sub2api_line("Sub2API", line).expect("diagnostic line");
        for expected in [
            "openai.forward_failed",
            "status_code=502",
            "latency_ms=1250",
            "request_id=123e4567-e89b-12d3-a456-426614174000",
            "stage=upstream_connect",
            "platform=openai",
            "model=gpt-5.6-sol",
            "method=POST",
            "path=/v1/responses",
            "class=upstream",
        ] {
            assert!(safe.contains(expected), "missing {expected} in: {safe}");
        }
    }

    #[test]
    fn runtime_batches_preserve_utf8_records_and_stay_bounded() {
        let (tx, rx) = sync_channel(128);
        let mut emitter = BatchEmitter::new(tx, egui::Context::default());
        let expected = (0..200)
            .map(|index| format!("record-{index:03}-{}", "界🙂".repeat(200)))
            .collect::<Vec<_>>();
        for record in &expected {
            emitter.push(record.clone());
        }
        emitter.flush();

        let batches = rx.try_iter().collect::<Vec<_>>();
        assert!(batches.len() > 1);
        for batch in &batches {
            assert!(batch.byte_len() <= MAX_BATCH_BYTES);
            assert!(batch.records.len() <= MAX_BATCH_RECORDS);
        }
        let actual = batches
            .into_iter()
            .flat_map(RuntimeLogBatch::into_records)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn full_runtime_queue_drops_without_blocking_and_reports_safe_totals_once() {
        use crate::AppEvent;

        let (tx, rx) = bounded_channel();
        let mut emitter = BatchEmitter::new(tx, egui::Context::default());
        for index in 0..RUNTIME_LOG_CHANNEL_CAPACITY {
            emitter.push(format!("seed-{index}"));
            emitter.flush();
        }
        for index in 0..5 {
            emitter.push(format!("private-secret-record-{index}"));
            emitter.flush();
        }

        let (control_tx, control_rx) = std::sync::mpsc::channel();
        control_tx.send(AppEvent::Complete).expect("control send");
        assert!(matches!(control_rx.try_recv(), Ok(AppEvent::Complete)));

        rx.try_recv().expect("free one runtime slot");
        emitter.flush();
        let records = rx
            .try_iter()
            .flat_map(RuntimeLogBatch::into_records)
            .collect::<Vec<_>>();
        let summaries = records
            .iter()
            .filter(|record| record.contains("class=queue_overflow"))
            .collect::<Vec<_>>();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].contains("dropped_records=5"));
        assert!(summaries[0].contains("dropped_batches=5"));
        assert!(!summaries[0].contains("private-secret"));

        emitter.flush();
        assert!(rx.try_recv().is_err(), "drop summary must not repeat");
    }

    #[test]
    fn oversized_runtime_record_is_dropped_whole() {
        let (tx, rx) = sync_channel(2);
        let mut emitter = BatchEmitter::new(tx, egui::Context::default());
        emitter.push(format!("private-secret-{}", "界".repeat(MAX_BATCH_BYTES)));
        emitter.flush();
        let records = rx.recv().expect("drop summary").into_records();
        assert_eq!(records.len(), 1);
        assert!(records[0].contains("dropped_records=1"));
        assert!(!records[0].contains("private-secret"));
    }

    #[test]
    fn database_authentication_errors_are_classified_without_raw_context() {
        let raw =
            "2026-08-02 12:00:00 CST FATAL: password authentication failed for user secret-user";
        let safe = format_plain_diagnostic("PostgreSQL", raw);
        assert!(safe.contains("class=authentication+database"));
        assert!(!safe.contains("secret-user"));
        assert!(!safe.contains("password"));
    }

    #[test]
    fn lifecycle_deferrals_remain_explicit_after_error_sanitization() {
        let safe = summarize_error_for_display(
            "ROUTER_LIFECYCLE_DEFERRED: Stop Router was deferred for an active request",
        );
        assert!(safe.contains("class=lifecycle_deferred"));
        assert!(!safe.contains("active request"));
    }

    #[test]
    fn unstructured_stderr_never_reaches_the_display_verbatim() {
        let raw = "fatal: private customer text short-secret-value";
        let safe = format_plain_diagnostic("stderr", raw);
        assert_eq!(safe, "[stderr] class=request_failure");
        assert!(!safe.contains("private customer text"));
        assert!(!safe.contains("short-secret-value"));
    }

    #[test]
    fn oversized_partial_lines_are_discarded_in_full() {
        use std::io::Write;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-router-log-tail-{}-{nonce}",
            std::process::id()
        ));
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).expect("create logs");
        let path = logs.join("stderr.log");
        std::fs::write(
            &path,
            format!(
                "request_body=private-payload{}",
                "x".repeat(MAX_PENDING_BYTES)
            ),
        )
        .expect("write oversized line");

        let mut source = LogSource::new(&root, "test", "stderr.log", LogKind::Stderr);
        assert!(source.read_new_lines().expect("initial read").0.is_empty());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append");
        writeln!(file).expect("finish oversized line");
        writeln!(file, "ERROR connection refused").expect("write diagnostic");
        let lines = source.read_new_lines().expect("follow-up read").0;
        assert_eq!(lines, vec!["ERROR connection refused"]);
        std::fs::remove_dir_all(root).expect("remove temp root");
    }

    #[test]
    fn oversized_tail_after_a_complete_line_is_discarded_in_full() {
        use std::io::Write;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-router-log-complete-tail-{}-{nonce}",
            std::process::id()
        ));
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).expect("create logs");
        let path = logs.join("stderr.log");
        std::fs::write(
            &path,
            format!(
                "ERROR first\nrequest_body=private{}",
                "界".repeat(MAX_PENDING_BYTES / 3 + 100)
            ),
        )
        .expect("write complete line and oversized tail");

        let mut source = LogSource::new(&root, "test", "stderr.log", LogKind::Stderr);
        assert_eq!(
            source.read_new_lines().expect("initial read").0,
            vec!["ERROR first"]
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append");
        writeln!(file).expect("finish oversized line");
        writeln!(file, "ERROR second").expect("write next record");
        let lines = source.read_new_lines().expect("follow-up read").0;
        assert_eq!(lines, vec!["ERROR second"]);
        std::fs::remove_dir_all(root).expect("remove temp root");
    }

    #[test]
    fn utf8_codepoint_split_across_reads_is_preserved() {
        use std::io::Write;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-router-log-utf8-split-{}-{nonce}",
            std::process::id()
        ));
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).expect("create logs");
        let path = logs.join("stderr.log");
        let mut prefix = b"ERROR prefix ".to_vec();
        prefix.extend_from_slice(&[0xE7, 0x95]);
        std::fs::write(&path, prefix).expect("write partial codepoint");

        let mut source = LogSource::new(&root, "test", "stderr.log", LogKind::Stderr);
        assert!(source.read_new_lines().expect("partial read").0.is_empty());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append");
        file.write_all(&[0x8C, b'\n'])
            .expect("finish UTF-8 codepoint");
        assert_eq!(
            source.read_new_lines().expect("complete read").0,
            vec!["ERROR prefix 界"]
        );
        std::fs::remove_dir_all(root).expect("remove temp root");
    }

    #[test]
    fn a_log_file_created_after_start_is_still_initially_tailed() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-router-log-late-file-{}-{nonce}",
            std::process::id()
        ));
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).expect("create logs");
        let mut source = LogSource::new(&root, "test", "stderr.log", LogKind::Stderr);
        assert!(!source.read_new_lines().expect("missing file").1);
        std::fs::write(logs.join("stderr.log"), "ERROR ready\n").expect("create late file");
        let (lines, initial_read) = source.read_new_lines().expect("initial file read");
        assert!(initial_read);
        assert_eq!(lines, vec!["ERROR ready"]);
        std::fs::remove_dir_all(root).expect("remove temp root");
    }
}
