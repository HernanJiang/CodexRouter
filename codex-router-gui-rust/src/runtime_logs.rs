use eframe::egui;
use regex::Regex;
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
/// The startup tail replay exists to explain the current run. Structured
/// Router events older than this window around the GUI start belong to a
/// previous run (for example a failed upstream session from hours ago) and
/// would otherwise flood the activity log with stale `request_failure` rows.
const ROUTER_EVENTS_HISTORY_GRACE: Duration = Duration::from_secs(120);

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
    RouterEvents,
    CliProxyApi,
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
            path: crate::user_data::logs_root(root).join(relative),
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
        let started_at = chrono::Utc::now();
        let mut sources = vec![
            LogSource::new(
                &router_root,
                "Router Host",
                "router-host-stdout.log",
                LogKind::Stderr,
            ),
            LogSource::new(
                &router_root,
                "Router Host stderr",
                "router-host-stderr.log",
                LogKind::Stderr,
            ),
            LogSource::new(
                &router_root,
                "Router events",
                "router-events.jsonl",
                LogKind::RouterEvents,
            ),
            LogSource::new(
                &router_root,
                "CLIProxyAPI",
                "cli-proxy-stdout.log",
                LogKind::CliProxyApi,
            ),
            LogSource::new(
                &router_root,
                "CLIProxyAPI stderr",
                "cli-proxy-stderr.log",
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
                // Lightweight tray mode still watches only Sub2API quota failover
                // events so the first automatic channel switch is not missed.
                if let Some(source) = sources.first_mut() {
                    if let Ok((lines, _)) = source.read_new_lines() {
                        for line in lines {
                            if let Some(record) =
                                format_diagnostic_line(source.label, source.kind, &line)
                            {
                                if record.contains("openai.upstream_failover_switching")
                                    && (record.contains("upstream_status=402")
                                        || record.contains("upstream_status=429"))
                                {
                                    emitter.push(record);
                                }
                            }
                        }
                        emitter.flush();
                    }
                }
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
                                if !initial_history_line_is_current(source.kind, &line, started_at)
                                {
                                    continue;
                                }
                                if let Some(record) =
                                    format_diagnostic_line(source.label, source.kind, &line)
                                {
                                    if is_startup_quota_popup_record(&record) {
                                        continue;
                                    }
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

pub(crate) fn runtime_record_is_actionable(record: &str) -> bool {
    let lower = record.to_ascii_lowercase();
    if lower.contains("class=unclassified_error")
        && !lower.contains("code=")
        && !lower.contains("marker=")
        && !lower.contains("status=")
    {
        return false;
    }
    true
}

pub(crate) fn signals_router_health_failure(record: &str) -> bool {
    let normalized = record.to_ascii_lowercase();
    if normalized.contains("class=upstream") && !normalized.contains("sqlite") {
        return false;
    }
    let service_failure = normalized.contains("database")
        || normalized.contains("sqlite")
        || normalized.contains("connection refused")
        || normalized.contains("connection reset")
        || normalized.contains("context deadline exceeded")
        || normalized.contains("i/o timeout")
        || normalized.contains("timed out");
    let relevant_source = normalized.contains("router host")
        || normalized.contains("router events")
        || normalized.contains("cliproxyapi")
        || normalized.contains("sqlite")
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
        LogKind::RouterEvents => {
            is_router_events_diagnostic(line).then(|| format_plain_diagnostic(label, line))
        }
        LogKind::CliProxyApi => {
            is_cli_proxy_diagnostic(line).then(|| format_plain_diagnostic(label, line))
        }
        LogKind::Stderr => {
            let record = format_plain_diagnostic(label, line);
            runtime_record_is_actionable(&record).then_some(record)
        }
    }
}

/// Extract the structured `"timestamp"` field of a Router event JSONL record.
fn router_event_timestamp(line: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    static EVENT_TIMESTAMP: OnceLock<Regex> = OnceLock::new();
    let regex = EVENT_TIMESTAMP.get_or_init(|| {
        Regex::new(r#""timestamp"\s*:\s*"([^"]+)""#).expect("event timestamp regex is valid")
    });
    let value = regex.captures(line)?.get(1)?.as_str();
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .ok()
}

/// Whether a line from the initial tail replay is still relevant. Stale
/// structured Router events are dropped from the replay (they reappear live
/// only if they happen again); lines without a parsable timestamp stay
/// visible so a malformed feed never hides a real problem, and the other log
/// sources keep their existing behavior.
/// Quota / pool-switch popups must not replay from the startup tail. Live
/// events after the first usage snapshot still notify.
fn is_startup_quota_popup_record(record: &str) -> bool {
    record.contains("openai.upstream_failover_switching")
        || record.contains("event=request.pool_failover")
        || record.contains("event=request.pool_unavailable")
}

fn initial_history_line_is_current(
    kind: LogKind,
    line: &str,
    started_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    if !matches!(kind, LogKind::RouterEvents) {
        return true;
    }
    let Some(timestamp) = router_event_timestamp(line) else {
        return true;
    };
    let grace = chrono::Duration::from_std(ROUTER_EVENTS_HISTORY_GRACE)
        .expect("history grace fits into a chrono duration");
    // Future timestamps (clock skew) count as current.
    started_at.signed_duration_since(timestamp) <= grace
}

/// The structured JSONL event stream is the mandated diagnostic feed; only
/// WARN and above reaches the UI so per-request INFO traffic stays on disk.
fn is_router_events_diagnostic(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    let level_ok = [
        r#""LEVEL":"WARN""#,
        r#""LEVEL":"ERROR""#,
        r#""LEVEL":"FATAL""#,
        r#""LEVEL":"PANIC""#,
    ]
    .iter()
    .any(|needle| upper.contains(*needle));
    if !level_ok {
        // Pool switch / pool-drained events are INFO-level but user-actionable:
        // the UI turns them into a "switched account" notification.
        let event = router_event_name(line);
        if event == "request.pool_failover" || event == "request.pool_unavailable" {
            return true;
        }
        return false;
    }
    // Configuration sync emits WARN-level `configuration` rows on every Apply.
    // They are not user-actionable and flood the activity log.
    let event = router_event_name(line);
    if event.eq_ignore_ascii_case("configuration")
        || upper.contains(r#""EVENT":"CONFIGURATION""#)
        || event.eq_ignore_ascii_case("ledger.record_failed")
        || upper.contains(r#""EVENT":"LEDGER.RECORD_FAILED""#)
        || event.ends_with("quota_refresh_failed")
        || event == "backend.config_reload_deferred"
        || event == "backend.route_table_published_without_cli_ack"
        || event.ends_with("without_cli_ack")
    {
        return false;
    }
    // Apply used to emit CR-CFG-0005 on every channel while CLIProxy was
    // still reloading. Those rows are not actionable; only a real config
    // push failure should reach the activity log.
    let code = router_event_json_field(line, "error_code").unwrap_or_default();
    if code.eq_ignore_ascii_case("CR-CFG-0005") && event != "backend.config_push_failed" {
        return false;
    }
    true
}

fn router_event_json_field(line: &str, field: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;
    json.get(field)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn router_event_name(line: &str) -> String {
    router_event_json_field(line, "event")
        .or_else(|| router_event_json_field(line, "class"))
        .unwrap_or_default()
}

/// CLIProxyAPI writes Go-slog style console lines; forward only lines that
/// carry an error signal instead of mirroring every request.
fn is_cli_proxy_diagnostic(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error",
        "warn",
        "fatal",
        "panic",
        "failed",
        "timeout",
        "timed out",
        "connection refused",
        "connection reset",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn append_field(output: &mut String, key: &str, value: &str) {
    output.push_str(" | ");
    output.push_str(key);
    output.push('=');
    output.push_str(value);
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
    let event = router_event_name_from_text(line).filter(|event| {
        event.len() <= 128
            && event
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    });
    output.push_str(&summarize_error_for_display(&format!("{label} {line}")));
    let mut safe = redact_for_display(&output);
    if let Some(event) = event.as_deref() {
        let wanted = format!("event={event}");
        if !safe.contains(&wanted) {
            static EVENT_FIELD: OnceLock<Regex> = OnceLock::new();
            let regex = EVENT_FIELD.get_or_init(|| {
                Regex::new(r"event=[^\s|]+").expect("event field regex is valid")
            });
            if regex.is_match(&safe) {
                safe = regex.replace(&safe, wanted.as_str()).into_owned();
            }
        }
    }
    let event_name = event.as_deref().unwrap_or_default();
    if event_name == "request.pool_failover" || event_name == "request.pool_unavailable" {
        for (key, json_key) in [
            ("from_pool", "from_pool"),
            ("reason", "reason"),
            ("model", "public_model"),
        ] {
            if let Some(value) = router_event_json_field(line, json_key) {
                if !safe.contains(&format!("{key}=")) {
                    append_field(&mut safe, key, &value);
                }
            }
        }
    }
    limit_utf8_bytes(&safe, MAX_RECORD_BYTES)
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
    // Already-sanitized summaries must stay stable. Wrapping them in another
    // Chinese/English shell (for example "无法读取…") would otherwise reclassify
    // class=connection_refused as the generic request_failure via "无法".
    if let Some(existing) = existing_sanitized_summary(text) {
        let mut output = existing;
        if let Some(marker) = stable_error_marker(text) {
            if !output.contains("marker=") {
                append_field(&mut output, "marker", marker);
            }
        }
        return output;
    }
    let mut classes = error_classes(text);
    if classes.is_empty() {
        classes.push("unclassified_error");
    }
    let mut output = format!("class={}", classes.join("+"));
    for status in safe_http_statuses(text) {
        append_field(&mut output, "status", &status);
    }
    if let Some(marker) = stable_error_marker(text) {
        append_field(&mut output, "marker", marker);
    }
    for code in error_code_markers(text) {
        append_field(&mut output, "code", &code);
    }
    if let Some(event) = router_event_name_from_text(text) {
        if !output.contains("event=") {
            append_field(&mut output, "event", &event);
        }
    }
    output
}

fn router_event_name_from_text(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let event = router_event_name(&text[start..=end]);
    (!event.is_empty()).then_some(event)
}

/// Router error codes (`CR-XXX-NNNN`) are fixed, secret-free identifiers
/// emitted by the Router Host event stream and the desktop app. Like the
/// stable markers above they have to survive summarization, otherwise the UI
/// loses the exact failure the user must act on.
fn error_code_markers(text: &str) -> Vec<String> {
    static ERROR_CODE: OnceLock<Regex> = OnceLock::new();
    let regex = ERROR_CODE.get_or_init(|| {
        Regex::new(r"\bCR-[A-Z]{2,6}-\d{4}\b").expect("error code regex is valid")
    });
    let mut codes = Vec::new();
    for matched in regex.find_iter(text) {
        let code = matched.as_str().to_owned();
        if !codes.contains(&code) {
            codes.push(code);
        }
        if codes.len() >= 3 {
            break;
        }
    }
    codes
}

fn existing_sanitized_summary(text: &str) -> Option<String> {
    static SANITIZED: OnceLock<Regex> = OnceLock::new();
    let regex = SANITIZED.get_or_init(|| {
        Regex::new(r"(?i)\bclass=[a-z0-9_+]+(?:\s+[a-z0-9_+]+=[^\s]+)*")
            .expect("sanitized summary regex is valid")
    });
    regex
        .find(text)
        .map(|matched| matched.as_str().trim().to_owned())
}

/// Fixed, secret-free identifiers raised by the Router scripts and by the
/// desktop app. Summarization keeps only an error class, which is too coarse to
/// act on, so these markers have to survive it for the UI to explain what the
/// user must do next.
const STABLE_ERROR_MARKERS: &[&str] = &[
    "ROUTER_CONFIG_SAVE_LOCK_FAILED",
    "ROUTER_CONFIG_SAVE_CODEX_SNAPSHOT_FAILED",
    "ROUTER_CONFIG_SAVE_BACKUP_FAILED",
    "ROUTER_CONFIG_SAVE_CREDENTIALS_FAILED",
    "ROUTER_CONFIG_SAVE_FILES_FAILED",
    "ROUTER_CONFIG_SAVE_APPLY_SCRIPT_FAILED",
    "ROUTER_CONFIG_SAVE_DEPLOY_FAILED",
    "ROUTER_INSTALL_ROOT_CONFLICT",
    "ROUTER_PORT_CONFLICT",
    "ROUTER_DEPLOY_NO_MODELS",
    "ROUTER_DEPLOY_NO_SERVABLE_MODEL",
    "ROUTER_DEPLOY_COMPLIANCE_REQUIRED",
    "ROUTER_DEPLOY_COMPLIANCE_ACCEPT_FAILED",
    "ROUTER_DEPLOY_ADMIN_ACCOUNTS_FAILED",
    "ROUTER_DEPLOY_GROUP_SYNC_FAILED",
    "ROUTER_DEPLOY_API_CHANNELS_FAILED",
    "ROUTER_DEPLOY_OAUTH_SYNC_FAILED",
    "ROUTER_DEPLOY_COMPOSITE_FAILED",
    "ROUTER_CONFIG_SAVE_NATIVE_APPLY_FAILED",
    "ROUTER_PROFILE_CREDENTIAL_MISSING",
    "ROUTER_PROFILE_CREDENTIAL_READ_FAILED",
    "ROUTER_PROFILE_CREDENTIAL_WRITE_FAILED",
    "ROUTER_KIMI_CREDENTIAL_REJECTED",
    "ROUTER_PROFILE_SAVE_FAILED",
    "ROUTER_PROFILE_ROLLBACK_INCOMPLETE",
    "ROUTER_PROXY_UNSUPPORTED",
    "ROUTER_PROXY_CREDENTIAL_STORAGE_UNSUPPORTED",
    "ROUTER_PROXY_MANAGED_RESOURCE_CONFLICT",
    "ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY",
    "ROUTER_OAUTH_PREPARE_ROUTER_START",
    "ROUTER_OAUTH_PREPARE_ADMIN_LOGIN",
    "ROUTER_OAUTH_PREPARE_COMPLIANCE",
    "ROUTER_OAUTH_PREPARE_COMPONENTS",
    "ROUTER_OAUTH_PREPARE_PROCESS",
    "ROUTER_OAUTH_PREPARE_TIMEOUT",
    "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE",
    "ROUTER_OAUTH_ACCOUNTS_PARSE",
    "ROUTER_ACL_UNSUPPORTED",
];

fn stable_error_marker(text: &str) -> Option<&'static str> {
    let upper = text.to_ascii_uppercase();
    STABLE_ERROR_MARKERS
        .iter()
        .copied()
        .find(|marker| upper.contains(marker))
}

fn error_classes(text: &str) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    let mut classes = Vec::new();

    // Lifecycle markers describe why a destructive action was deliberately not
    // taken. The explanation often names the protected services, which must not
    // turn a safe deferral into database/Redis/configuration failure classes.
    if lower.contains("router_lifecycle_deferred") {
        return vec!["lifecycle_deferred"];
    }
    if lower.contains("router_lifecycle_busy") {
        return vec!["lifecycle_busy"];
    }
    if lower.contains("router_lifecycle_safety_check_failed") {
        return vec!["lifecycle_safety_check_failed"];
    }

    if lower.contains("router_install_root_conflict") || lower.contains("install_root_conflict") {
        push_unique_class(&mut classes, "install_root_conflict");
    }
    if lower.contains("router_port_conflict") || lower.contains("port_conflict") {
        push_unique_class(&mut classes, "port_conflict");
    }

    if contains_any(
        &lower,
        &["content_policy", "content policy", "内容审计", "风险规则"],
    ) {
        push_unique_class(&mut classes, "content_policy");
    }
    if contains_any(
        &lower,
        &[
            "rate limit",
            "rate-limit",
            "rate limited",
            "too many requests",
            "限流",
        ],
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
    const LONG_TOKEN: &str = r"\b[A-Za-z0-9_]{32,}\b";
    redaction_rules()
        .iter()
        .fold(text.to_owned(), |value, (regex, replacement)| {
            // Event names are long lowercase_snake tokens. The 32-char rule
            // would otherwise turn backend.route_table_published_without_cli_ack
            // into event=backend.[REDACTED].
            if regex.as_str() == LONG_TOKEN {
                regex
                    .replace_all(&value, |caps: &regex::Captures| {
                        let matched = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
                        if matched
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                        {
                            matched.to_owned()
                        } else {
                            (*replacement).to_owned()
                        }
                    })
                    .into_owned()
            } else {
                regex.replace_all(&value, *replacement).into_owned()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_switch_events_are_diagnostic_and_carry_pool_fields() {
        // INFO-level pool-switch events must reach the UI so it can pop the
        // "switched account" notification.
        assert!(is_router_events_diagnostic(
            r#"{"level":"INFO","event":"request.pool_failover","from_pool":"cr/r10a56/xai","reason":"auth_unavailable"}"#
        ));
        assert!(is_router_events_diagnostic(
            r#"{"level":"INFO","event":"request.pool_unavailable","public_model":"grok-4.6"}"#
        ));
        // Ordinary INFO traffic stays out of the UI.
        assert!(!is_router_events_diagnostic(
            r#"{"level":"INFO","event":"request.ok"}"#
        ));
        let formatted = format_plain_diagnostic(
            "Router events",
            r#"{"level":"INFO","event":"request.pool_failover","from_pool":"cr/r10a56/xai","reason":"auth_unavailable","public_model":"grok-4.6"}"#,
        );
        assert!(formatted.contains("event=request.pool_failover"));
        assert!(formatted.contains("from_pool=cr/r10a56/xai"));
        assert!(formatted.contains("reason=auth_unavailable"));
    }

    #[test]
    fn redaction_removes_credentials_and_user_paths() {
        let raw = concat!(
            "Authorization: Bearer secret-bearer-value ",
            "api_key=plain-key password=plain-password ",
            "sk-proj-abcdefghijklmnopqrstuvwxyz ",
            "eyJheader12.eyJpayload34.signatur56 ",
            "onepart1.twopart2.threepart3.fourpart4.fivepart5 ",
            concat!("gh", "p_", "abcdefghijklmnopqrstuvwxyz123456 "),
            concat!("xo", "xb-", "1234567890-abcdefghijklmnop "),
            "sk_live_abcdefghijklmnopqrstuv ",
            concat!("AK", "IA", "ABCDEFGHIJKLMNOP "),
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
            concat!("gh", "p_"),
            concat!("xo", "xb-"),
            "sk_live_",
            concat!("AK", "IA", "ABCDEFGHIJKLMNOP"),
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
    fn router_events_lines_gate_on_warn_level_and_redact_payloads() {
        let info = r#"{"event":"request.completed","level":"INFO","http_status":200}"#;
        assert!(format_diagnostic_line("Router events", LogKind::RouterEvents, info).is_none());
        let warn = concat!(
            r#"{"error_code":"CR-CFG-0005","event":"backend.config_push_failed","#,
            r#""level":"WARN","api_key":"secret-key"}"#
        );
        let safe = format_diagnostic_line("Router events", LogKind::RouterEvents, warn)
            .expect("warn event is diagnostic");
        assert!(safe.contains("[Router events]"));
        assert!(safe.contains("CR-CFG-0005"));
        assert!(safe.contains("event=backend.config_push_failed"), "{safe}");
        assert!(!safe.contains("secret-key"), "payload leaked: {safe}");
        let configuration = concat!(
            r#"{"error_code":"CR-CFG-0005","event":"configuration","#,
            r#""level":"WARN"}"#
        );
        assert!(format_diagnostic_line(
            "Router events",
            LogKind::RouterEvents,
            configuration
        )
        .is_none());
        let ledger = r#"{"error_description":"request ledger entry already exists","event":"ledger.record_failed","level":"WARN"}"#;
        assert!(format_diagnostic_line("Router events", LogKind::RouterEvents, ledger).is_none());
        let quota = r#"{"event":"control.oauth_quota_refresh_failed","error_code":"network_error","level":"WARN"}"#;
        assert!(format_diagnostic_line("Router events", LogKind::RouterEvents, quota).is_none());
        let deferred = r#"{"error_code":"CR-CFG-0005","event":"backend.route_table_published_without_cli_ack","level":"WARN"}"#;
        assert!(format_diagnostic_line("Router events", LogKind::RouterEvents, deferred).is_none());
        let network = r#"{"event":"control.account_recovery_probe_failed","error_code":"network_error","level":"WARN"}"#;
        let safe = format_diagnostic_line("Router events", LogKind::RouterEvents, network).unwrap();
        assert!(safe.contains("event=control.account_recovery_probe_failed"), "{safe}");
        assert!(!safe.contains("event=[REDACTED]"), "{safe}");
        let long_event = "event=backend.route_table_published_without_cli_ack";
        let redacted_event = redact_for_display(long_event);
        assert_eq!(redacted_event, long_event, "{redacted_event}");
        let push_failed = concat!(
            r#"{"error_code":"CR-CFG-0005","event":"backend.route_table_published_without_cli_ack","#,
            r#""level":"ERROR"}"#
        );
        assert!(
            format_diagnostic_line("Router events", LogKind::RouterEvents, push_failed).is_none(),
            "without_cli_ack must stay out of the activity log even at ERROR"
        );
    }

    #[test]
    fn cli_proxy_lines_gate_on_error_keywords() {
        assert!(format_diagnostic_line(
            "CLIProxyAPI",
            LogKind::CliProxyApi,
            "level=info msg=\"request completed\" status=200"
        )
        .is_none());
        let safe = format_diagnostic_line(
            "CLIProxyAPI",
            LogKind::CliProxyApi,
            "level=error msg=\"upstream dial failed\"",
        )
        .expect("error line is diagnostic");
        assert!(safe.contains("[CLIProxyAPI]"));
    }

    #[test]
    fn unclassified_host_noise_is_not_actionable() {
        assert!(!runtime_record_is_actionable(
            "[Router Host] class=unclassified_error"
        ));
        assert!(runtime_record_is_actionable(
            "[Router events] class=configuration | code=CR-CFG-0005"
        ));
        assert!(runtime_record_is_actionable(
            "本机 Router 健康探测失败（1/3）：class=connection_refused"
        ));
    }

    #[test]
    fn only_local_service_failures_trigger_an_immediate_health_probe() {
        assert!(signals_router_health_failure(
            "[Router Host] class=timeout | context deadline exceeded"
        ));
        assert!(signals_router_health_failure(
            "[CLIProxyAPI] connection reset by peer"
        ));
        assert!(signals_router_health_failure(
            "[Router events] {\"level\":\"ERROR\",\"error_code\":\"CR-STO-0001\",\"sqlite\":\"database is locked\"}"
        ));
        assert!(!signals_router_health_failure(
            "[Router events] class=upstream | upstream provider timed out"
        ));
        assert!(!signals_router_health_failure(
            "[OAuth stderr] connection refused"
        ));
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
    fn startup_history_does_not_replay_quota_popup_events() {
        assert!(is_startup_quota_popup_record(
            "[Router events] event=request.pool_failover | from_pool=cr/r10a56/xai"
        ));
        assert!(is_startup_quota_popup_record(
            "[Router events] event=request.pool_unavailable | model=grok-4.6"
        ));
        assert!(is_startup_quota_popup_record(
            "openai.upstream_failover_switching | upstream_status=429 | account_id=4"
        ));
        assert!(!is_startup_quota_popup_record(
            r#"{"level":"ERROR","error_code":"CR-SYS-0001"}"#
        ));
    }

    #[test]
    fn stale_router_event_history_is_not_replayed_on_startup() {
        let started_at = chrono::Utc::now();
        let event_at = |timestamp: chrono::DateTime<chrono::Utc>| {
            format!(
                r#"{{"level":"ERROR","error_code":"CR-SYS-0001","timestamp":"{}"}}"#,
                timestamp.to_rfc3339()
            )
        };
        let stale = event_at(started_at - chrono::Duration::hours(2));
        let just_before = event_at(started_at - ROUTER_EVENTS_HISTORY_GRACE);
        let live = event_at(started_at);

        assert!(!initial_history_line_is_current(
            LogKind::RouterEvents,
            &stale,
            started_at
        ));
        // Events right at the grace boundary still explain the current run.
        assert!(initial_history_line_is_current(
            LogKind::RouterEvents,
            &just_before,
            started_at
        ));
        assert!(initial_history_line_is_current(
            LogKind::RouterEvents,
            &live,
            started_at
        ));
        // Clock skew into the future is treated as current.
        let future = event_at(started_at + chrono::Duration::minutes(5));
        assert!(initial_history_line_is_current(
            LogKind::RouterEvents,
            &future,
            started_at
        ));
        // A line without a parsable timestamp never gets hidden.
        assert!(initial_history_line_is_current(
            LogKind::RouterEvents,
            r#"{"level":"ERROR","error_code":"CR-SYS-0001"}"#,
            started_at
        ));
        // Other log sources keep their existing replay behavior.
        assert!(initial_history_line_is_current(
            LogKind::Stderr,
            &stale,
            started_at
        ));
        assert!(initial_history_line_is_current(
            LogKind::CliProxyApi,
            &stale,
            started_at
        ));
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
    fn hyphenated_admin_rate_limits_are_classified() {
        let safe = summarize_error_for_display(
            "Sub2API admin login is rate-limited. Wait a few seconds and retry.",
        );
        assert_eq!(safe, "class=rate_limit");
    }

    #[test]
    fn kimi_quota_authentication_failures_are_not_misclassified_as_rate_limits() {
        let unauthorized = summarize_error_for_display(
            "ROUTER_KIMI_CREDENTIAL_REJECTED: Kimi Coding Plan quota query failed (HTTP 401).",
        );
        assert_eq!(
            unauthorized,
            "class=authentication | status=401 | marker=ROUTER_KIMI_CREDENTIAL_REJECTED"
        );

        let forbidden = summarize_error_for_display(
            "ROUTER_KIMI_CREDENTIAL_REJECTED: Kimi Coding Plan quota query failed (HTTP 403).",
        );
        assert_eq!(
            forbidden,
            "class=permission | status=403 | marker=ROUTER_KIMI_CREDENTIAL_REJECTED"
        );
    }

    #[test]
    fn lifecycle_deferrals_remain_explicit_after_error_sanitization() {
        let safe = summarize_error_for_display(
            "ROUTER_LIFECYCLE_DEFERRED: Apply Router configuration was deferred; Sub2API, Redis, and PostgreSQL were left unchanged",
        );
        assert_eq!(safe, "class=lifecycle_deferred");
    }

    #[test]
    fn stable_markers_survive_sanitization_without_leaking_details() {
        let safe = summarize_error_for_display(
            "ROUTER_DEPLOY_NO_MODELS: at least one model is required, but C:\\Users\\person\\config.json has none.",
        );
        assert!(safe.contains("marker=ROUTER_DEPLOY_NO_MODELS"), "{safe}");
        assert!(!safe.contains("person"));
        assert!(!safe.contains("config.json"));
        // Sanitizing an already sanitized summary must stay stable so the marker
        // still reaches the UI after the deployment log is summarized twice.
        assert!(summarize_error_for_display(&safe).contains("marker=ROUTER_DEPLOY_NO_MODELS"));
        assert!(!summarize_error_for_display("plain upstream failure").contains("marker="));
    }

    #[test]
    fn chinese_wrapper_does_not_erase_an_existing_error_class() {
        let inner = summarize_error_for_display(
            "Unable to connect to the remote server. connection refused 127.0.0.1:18080",
        );
        assert!(inner.contains("class=connection_refused"), "{inner}");
        let wrapped = format!("无法读取 OAuth 账号: {inner}");
        let safe = summarize_error_for_display(&wrapped);
        assert!(
            safe.contains("class=connection_refused"),
            "expected preserved class, got {safe}"
        );
        assert!(
            !safe.contains("class=request_failure"),
            "generic failure must not replace a specific class: {safe}"
        );
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
