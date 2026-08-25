//! Local compatibility gateway in front of Sub2API `/v1/responses`.
//!
//! Codex keeps talking to a loopback URL. This process rewrites mixed-model
//! Responses payloads, then forwards the request to Sub2API so a Grok 422 or
//! a poisoned encrypted function output cannot turn into an infinite compact
//! loop or a router-wide 502.

use super::responses_compat::{
    append_incomplete_turn_continuation, extract_leaked_tool_calls, is_chat_completions_path,
    is_compact_path, is_exhausted_account_status, is_openai_family_model, is_responses_path,
    is_unsupported_image_error, is_xai_family_model, prepare_official_compact_request,
    prepare_xai_official_compact_request, rewrite_poisoned_upstream_status, rewrite_provider_json,
    rewrite_sse_text, sanitize_responses_request, sanitize_responses_request_aggressive,
    sanitize_responses_request_without_images, should_continue_incomplete_agent_turn,
    should_retry_after_upstream_error, shield_desktop_auth_failure, synthetic_compact_response,
};
use super::{detect_context_defaults, detect_max_output_defaults, ModelContextBudget};
use anyhow::{bail, Context};
use flate2::read::{DeflateDecoder, GzDecoder};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Default number of automatic retries when an upstream answers 429, a
/// transient network error, or the Sub2API exhausted-account 503.
/// Overridable through the Router config. The first retry waits 5s and
/// every further retry multiplies the wait by five (5s / 25s / 125s / ...).
pub const DEFAULT_RATE_LIMIT_RETRIES: u32 = 3;

pub fn clamp_rate_limit_retries(value: u32) -> u32 {
    value.min(MAX_RATE_LIMIT_RETRIES)
}

pub fn codex_retry_count(value: u32) -> i64 {
    i64::from(clamp_rate_limit_retries(value))
}
/// Hard ceiling for a configured retry count so a typo cannot pin a worker
/// thread on a sleeping retry loop forever.
pub const MAX_RATE_LIMIT_RETRIES: u32 = 32;
/// Ceiling for one backoff step. A `Retry-After` hint above this is clamped
/// instead of skipping the retry, so the conversation keeps waiting.
const MAX_RATE_LIMIT_DELAY: Duration = Duration::from_secs(3600);
/// First retry wait in seconds. Every further retry multiplies the wait by
/// five: 5s, 25s, 125s, 625s, ... (each step clamped at the ceiling above).
const RATE_LIMIT_RETRY_BASE_DELAY_SECS: u64 = 5;
/// Cumulative backoff allowed for one Codex turn. The default three retries
/// still receive the complete 5s + 25s + 125s ladder, while a high custom
/// retry count cannot advance into 625-second and hour-long sleeps that leave
/// the task permanently active after a Router/network interruption.
const MAX_REQUEST_RETRY_WAIT: Duration = Duration::from_secs(180);
/// Before any HTTP/SSE headers reach Codex, a long retry sleep looks like a
/// hung connect. Desktop then reports "error sending request" and storms
/// new connections. Keep the silent wait inside Codex's request budget.
const MAX_PRECONTENT_RETRY_WAIT: Duration = Duration::from_secs(8);
/// While streaming an agent (non-OpenAI-family) response, the upstream socket
/// is polled on this cadence so the gateway can keep the Codex session alive
/// through long provider-side reasoning pauses.
const STREAM_POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// Total upstream silence tolerated before an agent stream is declared dead.
/// Grok/Gemini deep-reasoning pauses legitimately exceed the old fixed 300s
/// socket timeout; 30 minutes of complete silence is a genuine hang.
const STREAM_MAX_SILENCE: Duration = Duration::from_secs(1800);
/// Codex client write/read timeout after the request headers have been read.
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(300);
/// Drain window used while half-closing the Codex socket so reqwest sees a
/// FIN instead of a Windows RST ("error decoding response body").
const CLIENT_CLOSE_DRAIN: Duration = Duration::from_millis(50);
/// How many extra upstream POSTs the gateway will issue when a third-party
/// model ends an in-progress agent turn with commentary and no tool call.
/// Codex treats that shape as `task_complete`; two nudges is enough to
/// recover the live Grok "我先对照…" / "我改用看图工具" stops without looping.
const MAX_INCOMPLETE_CONTINUES: u32 = 2;
const STR_INCOMPLETE_CONTINUE: &str = "CR-STR-0010";

struct GatewayState {
    stop: std::sync::Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
    listen: String,
    upstream: String,
    rate_limit_retries: u32,
}

struct UpstreamResponse {
    stream: TcpStream,
    headers: Vec<u8>,
    leftover: Vec<u8>,
    status: u16,
    header_map: HashMap<String, String>,
}

static GATEWAY: OnceLock<Mutex<Option<GatewayState>>> = OnceLock::new();

fn gateway_slot() -> &'static Mutex<Option<GatewayState>> {
    GATEWAY.get_or_init(|| Mutex::new(None))
}

/// Optional diagnostics log for the local Codex <-> gateway link. The Codex
/// side reports "error sending request" without any detail, so every request,
/// retry, and failure is mirrored here to make the local hop diagnosable.
static GATEWAY_LOG: OnceLock<std::path::PathBuf> = OnceLock::new();
const GATEWAY_LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Per-model output token limits configured in the Router UI, keyed by the
/// public model id Codex sends in the request body. Updated live from the
/// GUI; no gateway restart is needed when the user edits a model.
static MAX_OUTPUT_TOKENS: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

fn max_output_tokens_slot() -> &'static Mutex<HashMap<String, i64>> {
    MAX_OUTPUT_TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn set_max_output_tokens_map(map: HashMap<String, i64>) {
    *max_output_tokens_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = map;
}

/// Configured output token limit for a request model id, matched
/// case-insensitively because Codex and the catalog can differ in casing.
fn max_output_tokens_for(model: &str) -> Option<i64> {
    let map = max_output_tokens_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    map.get(model)
        .copied()
        .or_else(|| map.get(&model.to_ascii_lowercase()).copied())
        .filter(|limit| *limit > 0)
}

static CONTEXT_BUDGET: OnceLock<Mutex<HashMap<String, ModelContextBudget>>> = OnceLock::new();

fn context_budget_slot() -> &'static Mutex<HashMap<String, ModelContextBudget>> {
    CONTEXT_BUDGET.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn set_context_budget_map(map: HashMap<String, ModelContextBudget>) {
    *context_budget_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = map;
}

fn context_budget_for(model: &str) -> Option<ModelContextBudget> {
    let map = context_budget_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    map.get(model)
        .copied()
        .or_else(|| map.get(&model.to_ascii_lowercase()).copied())
        .filter(|budget| budget.window > 0)
}

/// Cheap, conservative token estimate used only to size the remaining compact
/// budget. CJK/fullwidth runs count closer to 1:1; ASCII closer to 4 chars.
/// Long base64 / data-URL / encrypted blobs are not tokenizer-equivalent to
/// their byte length (a screenshot is thousands of tokens, not millions).
fn estimate_text_tokens(text: &str) -> i64 {
    let trimmed = text.trim();
    if looks_like_binary_blob(trimmed) {
        return ((trimmed.len() as i64 / 1_024) + 256).min(4_096);
    }
    let mut tokens = 0_i64;
    let mut ascii_run = 0_i64;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_run += 1;
            continue;
        }
        if ascii_run > 0 {
            tokens += (ascii_run + 3) / 4;
            ascii_run = 0;
        }
        tokens += 1;
    }
    if ascii_run > 0 {
        tokens += (ascii_run + 3) / 4;
    }
    tokens
}

fn looks_like_binary_blob(text: &str) -> bool {
    if text.len() < 256 {
        return false;
    }
    if text.starts_with("data:image")
        || text.starts_with("data:application")
        || text.starts_with("gAAAA")
    {
        return true;
    }
    let mut sample_len = text.len().min(512);
    while sample_len > 0 && !text.is_char_boundary(sample_len) {
        sample_len -= 1;
    }
    if sample_len < 256 {
        return false;
    }
    let sample = &text[..sample_len];
    let b64ish = sample
        .bytes()
        .filter(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
        })
        .count();
    !sample.bytes().any(|byte| byte.is_ascii_whitespace()) && b64ish * 100 / sample_len >= 90
}

fn json_text_tokens(value: &Value) -> i64 {
    match value {
        Value::Null | Value::Bool(_) => 1,
        Value::Number(number) => estimate_text_tokens(&number.to_string()),
        Value::String(text) => estimate_text_tokens(text),
        Value::Array(items) => items.iter().map(json_text_tokens).sum::<i64>().saturating_add(2),
        Value::Object(map) => {
            let nested: i64 = map
                .iter()
                .map(|(key, nested)| estimate_text_tokens(key).saturating_add(json_text_tokens(nested)))
                .sum();
            nested.saturating_add(2)
        }
    }
}

fn estimate_request_input_tokens(body: &Value) -> i64 {
    let mut tokens = 0_i64;
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        tokens = tokens.saturating_add(estimate_text_tokens(instructions));
    }
    if let Some(input) = body.get("input") {
        tokens = tokens.saturating_add(json_text_tokens(input));
    }
    if let Some(messages) = body.get("messages") {
        tokens = tokens.saturating_add(json_text_tokens(messages));
    }
    tokens.max(1)
}

fn context_budget_for_model(model: &str) -> ModelContextBudget {
    context_budget_for(model).unwrap_or_else(|| {
        let window = detect_context_defaults(model).window;
        ModelContextBudget {
            window,
            compact_limit: window.saturating_mul(95) / 100,
        }
    })
}

fn remaining_compact_budget(model: &str, body: &Value) -> i64 {
    let budget = context_budget_for_model(model);
    let compact_limit = if budget.compact_limit > 0 {
        budget.compact_limit.min(budget.window)
    } else {
        budget.window.saturating_mul(95) / 100
    };
    let used = estimate_request_input_tokens(body).min(budget.window);
    compact_limit.saturating_sub(used).max(1)
}

/// Tokens Codex leaves for output when auto-compact is at 95%: 5% of the
/// window. A long transcript must not collapse this to 1.
fn compact_output_reserve(model: &str) -> i64 {
    let budget = context_budget_for_model(model);
    if budget.window <= 0 {
        return 1;
    }
    let reserved = if budget.compact_limit > 0 && budget.compact_limit < budget.window {
        budget.window - budget.compact_limit
    } else {
        budget.window.saturating_mul(5) / 100
    };
    reserved.max(1)
}

fn auto_output_token_limit(model: &str, body: &Value) -> i64 {
    let defaults = detect_max_output_defaults(model);
    let remaining = remaining_compact_budget(model, body);
    let reserve = compact_output_reserve(model);
    // Keep enough room for a real Grok/Gemini reasoning+tool turn even when
    // the cheap input estimate thinks the compact budget is exhausted.
    let floor = if defaults.hard_cap {
        reserve.max(32_768).min(defaults.tokens)
    } else {
        reserve.max(32_768).max(defaults.tokens)
    }
    .max(1);
    let mut limit = remaining.max(floor);
    if defaults.hard_cap {
        limit = limit.min(defaults.tokens);
    }
    limit.max(1)
}

/// Inject the per-request output token budget.
///
/// 1. A card with `max_output_tokens > 0` always wins.
/// 2. Otherwise raise Codex's unused-percent 5% reserve toward the remaining
///    compact budget / model cap, but never shrink it below the 5% reserve
///    (or the recommended Grok default). Long agent threads used to inject 1
///    token and Grok then died with `reason: max_output_tokens`.
fn inject_max_output_tokens(path: &str, body: &mut Value) -> bool {
    let Some(model) = body.get("model").and_then(Value::as_str).map(str::to_owned) else {
        return false;
    };
    let field = if is_chat_completions_path(path) {
        "max_tokens"
    } else {
        "max_output_tokens"
    };
    let current = body.get(field).and_then(Value::as_i64).unwrap_or(0);
    let limit = match max_output_tokens_for(&model) {
        Some(limit) => limit,
        None => {
            let defaults = detect_max_output_defaults(&model);
            let mut auto = auto_output_token_limit(&model, body);
            if current > 0 {
                auto = auto.max(current);
                if defaults.hard_cap {
                    auto = auto.min(defaults.tokens);
                }
            }
            auto
        }
    };
    if current == limit {
        return false;
    }
    gateway_log(
        "request.max_output",
        &format!(
            "CR-STR-0011 model={model} field={field} from={current} to={limit} remaining={} reserve={} used={}",
            remaining_compact_budget(&model, body),
            compact_output_reserve(&model),
            estimate_request_input_tokens(body)
        ),
    );
    body[field] = Value::from(limit);
    true
}

pub fn set_gateway_log_path(path: std::path::PathBuf) {
    let _ = GATEWAY_LOG.set(path);
}

fn gateway_log(event: &str, detail: &str) {
    let Some(path) = GATEWAY_LOG.get() else {
        return;
    };
    if cfg!(test) {
        return;
    }
    let timestamp = chrono::Utc::now().to_rfc3339();
    let detail = detail.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ");
    let line = format!("{{\"ts\":\"{timestamp}\",\"event\":\"{event}\",\"detail\":\"{detail}\"}}\n");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write as _;
        let _ = file.write_all(line.as_bytes());
        if let Ok(meta) = file.metadata() {
            if meta.len() > GATEWAY_LOG_MAX_BYTES {
                drop(file);
                let previous = path.with_extension("previous.jsonl");
                let _ = std::fs::rename(path, previous);
            }
        }
    }
}

pub fn responses_gateway_url(sub2api_host: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(sub2api_host.trim()).context("invalid Sub2API host")?;
    let port = url.port_or_known_default().unwrap_or(18080);
    url.set_port(Some(port.saturating_add(2)))
        .map_err(|_| anyhow::anyhow!("could not derive the responses gateway port"))?;
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub fn ensure_responses_gateway(
    sub2api_host: &str,
    rate_limit_max_retries: u32,
) -> anyhow::Result<String> {
    let listen = responses_gateway_url(sub2api_host)?;
    let upstream = sub2api_host.trim().trim_end_matches('/').to_owned();
    let rate_limit_max_retries = rate_limit_max_retries.min(MAX_RATE_LIMIT_RETRIES);
    let mut slot = gateway_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = slot.as_ref() {
        if state.listen == listen
            && state.upstream == upstream
            && state.rate_limit_retries == rate_limit_max_retries
            && !state.stop.load(Ordering::Relaxed)
        {
            return Ok(listen);
        }
        let old = slot.take().expect("gateway state should still exist");
        stop_gateway(old)?;
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let listener_url = listen.clone();
    let upstream_url = upstream.clone();
    let thread_stop = stop.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("codex-responses-gateway".to_owned())
        .spawn(move || {
            run_gateway(
                listener_url,
                upstream_url,
                thread_stop,
                rate_limit_max_retries,
                ready_tx,
            )
        })
        .context("failed to start the responses compatibility gateway")?;
    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .context("responses compatibility gateway did not report readiness")??;
    *slot = Some(GatewayState {
        stop,
        thread,
        listen: listen.clone(),
        upstream,
        rate_limit_retries: rate_limit_max_retries,
    });
    Ok(listen)
}

pub fn stop_responses_gateway() -> anyhow::Result<()> {
    let mut slot = gateway_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = slot.take() {
        stop_gateway(state)?;
    }
    Ok(())
}

fn stop_gateway(state: GatewayState) -> anyhow::Result<()> {
    state.stop.store(true, Ordering::Relaxed);
    poke_listener(&state.listen);
    state
        .thread
        .join()
        .map_err(|_| anyhow::anyhow!("responses compatibility gateway thread panicked"))
}

fn poke_listener(base: &str) {
    if let Ok(address) = socket_addr(base) {
        let _ = TcpStream::connect_timeout(&address, Duration::from_millis(200));
    }
}

fn listen_host_port(base: &str) -> anyhow::Result<(String, u16)> {
    let url = Url::parse(base).context("invalid gateway URL")?;
    let host = url.host_str().unwrap_or("127.0.0.1").to_owned();
    let port = url.port_or_known_default().unwrap_or(18082);
    Ok((host, port))
}

fn socket_addr(base: &str) -> anyhow::Result<SocketAddr> {
    let (host, port) = listen_host_port(base)?;
    format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid address {host}:{port}"))
}

fn run_gateway(
    listen: String,
    upstream: String,
    stop: std::sync::Arc<AtomicBool>,
    rate_limit_max_retries: u32,
    ready: std::sync::mpsc::SyncSender<anyhow::Result<()>>,
) {
    let address = match socket_addr(&listen) {
        Ok(address) => address,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let listener = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(error) => {
            let _ = ready.send(Err(error).context(format!(
                "responses compatibility gateway could not bind {address}"
            )));
            return;
        }
    };
    let _ = listener.set_nonblocking(true);
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let upstream = upstream.clone();
                thread::spawn(move || {
                    let mut client = stream;
                    let result = handle_client(&mut client, &upstream, rate_limit_max_retries);
                    close_client_gracefully(&mut client);
                    if let Err(error) = result {
                        if !client_connection_ended(&error) {
                            gateway_log("request.error", &format!("{error:#}"));
                        }
                    }
                });
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn handle_client(
    client: &mut TcpStream,
    upstream: &str,
    rate_limit_max_retries: u32,
) -> anyhow::Result<()> {
    prepare_client_socket(client)?;
    let (header_bytes, leftover) = read_headers(client)?;
    let header_text = String::from_utf8_lossy(&header_bytes);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").to_owned();
    let mut headers = HashMap::<String, String>::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    if method == "GET"
        && (path == "/"
            || path == "/v1"
            || path.starts_with("/login?")
            || path == "/login")
    {
        send_status(
            client,
            200,
            "text/plain; charset=utf-8",
            b"Codex-Router Responses gateway is running. API endpoint: /v1/responses. Admin endpoint: http://127.0.0.1:18080/admin/accounts.",
        )?;
        return Ok(());
    }
    let mut body = leftover;
    let request_chunked = headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    if request_chunked {
        // Codex may switch to HTTP chunked framing for large or rapidly
        // changing Responses bodies. The gateway must decode that framing
        // before JSON parsing; forwarding chunk headers as body bytes makes
        // Router Host report "request body is not valid JSON".
        body = read_chunked_body(client, body)?;
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > MAX_BODY_BYTES {
            send_simple(client, 413, b"request too large")?;
            return Ok(());
        }
        read_exact_more(client, &mut body, length)?;
        body.truncate(length);
    }
    if let Some(content_encoding) = headers.get("content-encoding").cloned() {
        body = decode_request_content_encoding(&content_encoding, &body)?;
        headers.remove("content-encoding");
    }

    gateway_log(
        "request.start",
        &format!("{method} {path} bytes={}", body.len()),
    );
    // The request is fully buffered; stretch the socket timeout so a long
    // Kimi/ChatGPT stream is not cut by the 30s header deadline.
    client.set_read_timeout(Some(CLIENT_IO_TIMEOUT)).ok();
    client.set_write_timeout(Some(CLIENT_IO_TIMEOUT)).ok();
    let mut request_body = body;
    let mut continue_body: Option<Value> = None;
    let mut hold_text_completed = false;
    if method == "POST" && (is_responses_path(&path) || is_chat_completions_path(&path)) {
        if let Ok(mut json_body) = serde_json::from_slice::<Value>(&request_body) {
            let openai_family = json_body
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(is_openai_family_model);
            if !openai_family {
                let stats = sanitize_responses_request(&path, &mut json_body);
                inject_max_output_tokens(&path, &mut json_body);
                if is_compact_path(&path) && is_xai_family_model(&stats.model) {
                    prepare_xai_official_compact_request(&mut json_body);
                    request_body = serde_json::to_vec(&json_body)?;
                } else if is_compact_path(&path) {
                    let output = json_body
                        .get("input")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let payload =
                        serde_json::to_vec(&synthetic_compact_response(&stats.model, &output))?;
                    send_status(client, 200, "application/json", &payload)?;
                    return Ok(());
                } else {
                    if is_responses_path(&path) && is_xai_family_model(&stats.model) {
                        hold_text_completed = true;
                        continue_body = Some(json_body.clone());
                    }
                    request_body = serde_json::to_vec(&json_body)?;
                }
            } else if is_compact_path(&path) {
                // ChatGPT remote compaction v2 expects exactly one
                // compaction output item. Generation controls such as
                // max_output_tokens/tools/stream turn this into a normal
                // response with extra non-compaction items.
                prepare_official_compact_request(&mut json_body);
                request_body = serde_json::to_vec(&json_body)?;
            } else if inject_max_output_tokens(&path, &mut json_body) {
                // OpenAI-family models pass through unsanitized; only
                // re-serialize when a configured limit was injected.
                request_body = serde_json::to_vec(&json_body)?;
            }
        }
    }

    // One retry budget shared by every pre-content stage: opening the
    // upstream, outwaiting a silent pre-stream stall, and buffering a
    // complete body. Until real SSE events (or a buffered body) reach the
    // Codex client a transparent upstream retry is seamless; afterwards a
    // reconnect would replay the answer from scratch, so late failures
    // surface a clean `response.failed` instead of a retry.
    let mut retry_budget = RequestRetryBudget::new(rate_limit_max_retries);
    let mut sse_prelude_sent = false;
    let mut stream_resume: Option<StreamResume> = None;
    let mut continue_point: Option<ContinuePoint> = None;
    let mut continue_attempts: u32 = 0;
    let mut last_held_completed: Option<String> = None;
    let (status, response_headers_map, response_body) = loop {
        retry_budget.set_max_wait(if sse_prelude_sent {
            MAX_REQUEST_RETRY_WAIT
        } else {
            MAX_PRECONTENT_RETRY_WAIT
        });
        let response = match open_upstream_with_rate_limit_retry(
            upstream,
            &method,
            &path,
            &headers,
            &request_body,
            &mut retry_budget,
            Some(client),
        ) {
            Ok(response) => response,
            Err(error) => {
                if let Some(held) = last_held_completed.take() {
                    write_all_socket(client, held.as_bytes())?;
                    return Ok(());
                }
                if sse_prelude_sent {
                    finish_sse(client, "", false)?;
                } else {
                    send_gateway_unavailable(client, &error)?;
                }
                return Ok(());
            }
        };
        let mut upstream_stream = response.stream;
        let response_headers = response.headers;
        let response_leftover = response.leftover;
        let status = response.status;
        let response_headers_map = response.header_map;
        if sse_prelude_sent && status >= 400 && is_responses_path(&path) {
            // The client already accepted one HTTP 200 SSE response. A retry
            // may fail with 429/5xx, but writing that second HTTP response
            // into the open event stream corrupts the protocol and leaves
            // Codex waiting on an invalid active turn. Close the existing SSE
            // response with its terminal failure event instead.
            if let Some(held) = last_held_completed.take() {
                write_all_socket(client, held.as_bytes())?;
                return Ok(());
            }
            finish_sse(client, "", false)?;
            return Ok(());
        }
        if status < 400 || !is_responses_path(&path) {
            if status < 400 && is_responses_path(&path) {
                let content_type = content_type_of(&response_headers_map);
                let streaming = content_type.contains("text/event-stream");
                if streaming {
                    if !sse_prelude_sent {
                        write_sse_prelude(client, &response_headers_map)?;
                        sse_prelude_sent = true;
                    }
                    match forward_agent_sse(
                        client,
                        &mut upstream_stream,
                        &response_headers_map,
                        response_leftover,
                        stream_resume.take(),
                        continue_point.clone(),
                        hold_text_completed,
                    )? {
                        SseForward::Done => return Ok(()),
                        SseForward::ReconcileFailed => {
                            // The retry diverged from the delivered text; the
                            // client still holds a consistent partial answer,
                            // so close with a clean terminal event.
                            finish_sse(client, "", false)?;
                            return Ok(());
                        }
                        SseForward::RetryableBeforeFirstEvent => {
                            let Some(delay) = retry_budget.reserve(None) else {
                                if let Some(held) = last_held_completed.take() {
                                    write_all_socket(client, held.as_bytes())?;
                                    return Ok(());
                                }
                                finish_sse(client, "", false)?;
                                return Ok(());
                            };
                            gateway_log(
                                "request.retry",
                                &format!(
                                    "{method} {path} pre-content stream retry #{}",
                                    retry_budget.retries_used
                                ),
                            );
                            sleep_for_retry_while_connected(delay, Some(client))?;
                            continue;
                        }
                        SseForward::RetryableWithPrefix(resume) => {
                            let Some(delay) = retry_budget.reserve(None) else {
                                if let Some(held) = last_held_completed.take() {
                                    write_all_socket(client, held.as_bytes())?;
                                    return Ok(());
                                }
                                finish_sse(client, "", false)?;
                                return Ok(());
                            };
                            gateway_log(
                                "request.retry",
                                &format!(
                                    "{method} {path} mid-stream retry #{} delivered_chars={}",
                                    retry_budget.retries_used,
                                    resume.prefix.len()
                                ),
                            );
                            stream_resume = Some(resume);
                            sleep_for_retry_while_connected(delay, Some(client))?;
                            continue;
                        }
                        SseForward::IncompleteAgentTurn(turn) => {
                            last_held_completed = Some(turn.held_completed.clone());
                            let model = continue_body
                                .as_ref()
                                .and_then(|body| body.get("model"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let should = continue_body.as_ref().is_some_and(|body| {
                                should_continue_incomplete_agent_turn(
                                    &model,
                                    body,
                                    &turn.delivered_text,
                                    false,
                                )
                            });
                            if should && continue_attempts < MAX_INCOMPLETE_CONTINUES {
                                if let Some(body) = continue_body.as_mut() {
                                    continue_attempts += 1;
                                    let append_text = if continue_attempts == 1 {
                                        turn.delivered_text.as_str()
                                    } else {
                                        turn.turn_text.as_str()
                                    };
                                    append_incomplete_turn_continuation(body, append_text);
                                    request_body = serde_json::to_vec(body)?;
                                    continue_point = Some(ContinuePoint {
                                        response_id: turn.response_id.clone(),
                                        new_response_id: None,
                                    });
                                    stream_resume = None;
                                    gateway_log(
                                        "request.incomplete_continue",
                                        &format!(
                                            "{STR_INCOMPLETE_CONTINUE} {method} {path} model={model} attempt={continue_attempts} chars={} turn_chars={}",
                                            turn.delivered_text.chars().count(),
                                            turn.turn_text.chars().count()
                                        ),
                                    );
                                    let _ = send_sse_keepalive(client);
                                    continue;
                                }
                            }
                            write_all_socket(client, turn.held_completed.as_bytes())?;
                            return Ok(());
                        }
                    }
                }
                if content_type.contains("json") {
                    match read_full_body(
                        &mut upstream_stream,
                        &response_headers_map,
                        response_leftover,
                    ) {
                        Ok(body) => {
                            return forward_rewritten_json_body(
                                client,
                                &response_headers_map,
                                body,
                            );
                        }
                        Err(error) => {
                            let Some(delay) = retry_budget.reserve(None) else {
                                send_gateway_unavailable(client, &error)?;
                                return Ok(());
                            };
                            gateway_log(
                                "request.retry",
                                &format!(
                                    "{method} {path} body read retry #{}",
                                    retry_budget.retries_used
                                ),
                            );
                            sleep_for_retry_while_connected(delay, Some(client))?;
                            continue;
                        }
                    }
                }
            }
            write_headers_forced_close(client, &response_headers)?;
            write_all_socket(client, &response_leftover)?;
            copy_socket(&mut upstream_stream, client)?;
            return Ok(());
        }
        // Error statuses are fully buffered before anything reaches the
        // client, so an upstream drop mid-body retries the whole request.
        match read_full_body(&mut upstream_stream, &response_headers_map, response_leftover) {
            Ok(body) => break (status, response_headers_map, body),
            Err(error) => {
                let Some(delay) = retry_budget.reserve(None) else {
                    send_gateway_unavailable(client, &error)?;
                    return Ok(());
                };
                gateway_log(
                    "request.retry",
                    &format!(
                        "{method} {path} error body read retry #{}",
                        retry_budget.retries_used
                    ),
                );
                sleep_for_retry_while_connected(delay, Some(client))?;
                continue;
            }
        }
    };
    let body_text = String::from_utf8_lossy(&response_body);
    if is_compact_path(&path)
        && !is_openai_family_model(
            serde_json::from_slice::<Value>(&request_body)
                .ok()
                .and_then(|body| {
                    body.get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default()
                .as_str(),
        )
        && (status >= 400
            || body_text.to_ascii_lowercase().contains("streaming not supported"))
    {
        if let Ok(json_body) = serde_json::from_slice::<Value>(&request_body) {
            let model = json_body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("grok-4.6");
            let output = json_body
                .get("input")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            gateway_log(
                "request.compact_fallback",
                &format!("{method} {path} status={status} local compact fallback"),
            );
            let payload = serde_json::to_vec(&synthetic_compact_response(model, &output))?;
            send_status(client, 200, "application/json", &payload)?;
            return Ok(());
        }
    }
    if should_retry_after_upstream_error(status, &body_text) {
        if let Ok(mut json_body) = serde_json::from_slice::<Value>(&request_body) {
            if !is_openai_family_model(
                json_body
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ) {
                if is_unsupported_image_error(&body_text) {
                    sanitize_responses_request_without_images(&path, &mut json_body);
                } else {
                    sanitize_responses_request_aggressive(&path, &mut json_body);
                }
                let retry_body = serde_json::to_vec(&json_body)?;
                if let Ok((retry_status, retry_headers, retry_body)) =
                    forward_error_exchange(upstream, &path, &headers, &retry_body)
                {
                    if retry_status < 400 {
                        write_all_socket(client, &retry_headers)?;
                        write_all_socket(client, &retry_body)?;
                        return Ok(());
                    }
                    let retry_text = String::from_utf8_lossy(&retry_body);
                    let (forward_status, forward_body) =
                        shield_desktop_auth_failure(retry_status, &retry_body);
                    if retry_status == 401 {
                        gateway_log(
                            "request.desktop_auth_shield",
                            &format!("{method} {path} upstream 401 remapped to {forward_status}"),
                        );
                    }
                    send_raw(
                        client,
                        &rebuild_response(
                            if retry_status == 401 {
                                forward_status
                            } else {
                                rewrite_poisoned_upstream_status(retry_status, &retry_text)
                            },
                            &parse_headers(&String::from_utf8_lossy(&retry_headers)),
                            &forward_body,
                        ),
                    )?;
                    return Ok(());
                }
            }
        }
    }
    let (forward_status, forward_body) = shield_desktop_auth_failure(status, &response_body);
    if status == 401 {
        gateway_log(
            "request.desktop_auth_shield",
            &format!("{method} {path} upstream 401 remapped to {forward_status}"),
        );
    }
    send_raw(
        client,
        &rebuild_response(
            if status == 401 {
                forward_status
            } else {
                rewrite_poisoned_upstream_status(status, &body_text)
            },
            &response_headers_map,
            &forward_body,
        ),
    )?;
    Ok(())
}

fn connect_upstream(upstream: &str) -> anyhow::Result<TcpStream> {
    let address = socket_addr(upstream)?;
    let stream = TcpStream::connect_timeout(&address, UPSTREAM_CONNECT_TIMEOUT)
        .context("could not connect to Sub2API")?;
    prepare_upstream_socket(&stream);
    Ok(stream)
}

fn open_upstream_with_rate_limit_retry(
    upstream: &str,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    retry_budget: &mut RequestRetryBudget,
    client: Option<&TcpStream>,
) -> anyhow::Result<UpstreamResponse> {
    loop {
        ensure_client_connected(client)?;
        let mut stream = match connect_upstream(upstream) {
            Ok(stream) => stream,
            Err(error) => {
                let Some(delay) = retry_budget.reserve(None) else {
                    return Err(error);
                };
                log_retry_wait(method, path, "connect", retry_budget.retries_used, delay);
                sleep_for_retry_while_connected(delay, client)?;
                continue;
            }
        };
        let mut request_headers = headers.clone();
        if let Err(error) = write_upstream_request(
            &mut stream,
            method,
            path,
            &mut request_headers,
            body,
            upstream,
        ) {
            let Some(delay) = retry_budget.reserve(None) else {
                return Err(error);
            };
            log_retry_wait(method, path, "request-write", retry_budget.retries_used, delay);
            sleep_for_retry_while_connected(delay, client)?;
            continue;
        }
        let (response_headers, mut leftover) = match read_headers(&mut stream) {
            Ok(parts) => parts,
            Err(error) => {
                let Some(delay) = retry_budget.reserve(None) else {
                    return Err(error);
                };
                log_retry_wait(method, path, "response-header", retry_budget.retries_used, delay);
                sleep_for_retry_while_connected(delay, client)?;
                continue;
            }
        };
        let response_header_text = String::from_utf8_lossy(&response_headers);
        let status = parse_status(&response_header_text).unwrap_or(200);
        let response_headers_map = parse_headers(&response_header_text);
        // A literal 429 is always retryable. Sub2API also reports an account
        // pool drained by upstream rate limiting as 503; classify it from the
        // (small, fully buffered) error body so it retries like a 429.
        let auth_rejected = status == 401;
        if auth_rejected {
            buffer_error_body(&mut stream, &response_headers_map, &mut leftover);
        }
        let rate_limited = if status == 429 {
            true
        } else if status == 503 {
            buffer_error_body(&mut stream, &response_headers_map, &mut leftover);
            is_exhausted_account_status(status, &String::from_utf8_lossy(&leftover))
        } else {
            false
        };
        let transient_status = matches!(status, 408 | 425 | 502 | 504) || auth_rejected;
        if !rate_limited && !transient_status {
            return Ok(UpstreamResponse {
                stream,
                headers: response_headers,
                leftover,
                status,
                header_map: response_headers_map,
            });
        }
        let Some(delay) = retry_budget.reserve(Some(&response_headers_map)) else {
            return Ok(UpstreamResponse {
                stream,
                headers: response_headers,
                leftover,
                status,
                header_map: response_headers_map,
            });
        };
        log_retry_wait(method, path, "upstream-status", retry_budget.retries_used, delay);
        sleep_for_retry_while_connected(delay, client)?;
    }
}

fn log_retry_wait(method: &str, path: &str, stage: &str, retry: usize, delay: Duration) {
    gateway_log(
        "request.retry",
        &format!(
            "{method} {path} stage={stage} retry=#{retry} wait={}s",
            delay.as_secs()
        ),
    );
}

struct RequestRetryBudget {
    max_retries: usize,
    retries_used: usize,
    reserved_wait: Duration,
    max_wait: Duration,
}

impl RequestRetryBudget {
    fn new(max_retries: u32) -> Self {
        Self {
            max_retries: max_retries.min(MAX_RATE_LIMIT_RETRIES) as usize,
            retries_used: 0,
            reserved_wait: Duration::ZERO,
            max_wait: MAX_REQUEST_RETRY_WAIT,
        }
    }

    fn set_max_wait(&mut self, max_wait: Duration) {
        self.max_wait = max_wait;
    }

    fn reserve(&mut self, headers: Option<&HashMap<String, String>>) -> Option<Duration> {
        if self.retries_used >= self.max_retries {
            return None;
        }
        let delay = rate_limit_retry_delay(headers, self.retries_used);
        let next_wait = self.reserved_wait.saturating_add(delay);
        if next_wait > self.max_wait {
            return None;
        }
        self.retries_used += 1;
        self.reserved_wait = next_wait;
        Some(delay)
    }
}

/// Read the rest of a small error response so it can be classified and, when
/// no retry remains, still be forwarded intact through the returned leftover.
fn buffer_error_body(
    stream: &mut TcpStream,
    headers: &HashMap<String, String>,
    leftover: &mut Vec<u8>,
) {
    const MAX_ERROR_BODY_BYTES: usize = 1024 * 1024;
    if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        let target = length.min(MAX_ERROR_BODY_BYTES);
        while leftover.len() < target {
            let mut buffer = [0_u8; 8192];
            match read_socket(stream, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => leftover.extend_from_slice(&buffer[..read]),
            }
        }
        return;
    }
    let mut buffer = [0_u8; 8192];
    while leftover.len() < MAX_ERROR_BODY_BYTES {
        match read_socket(stream, &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => leftover.extend_from_slice(&buffer[..read]),
        }
    }
}

/// Stepped backoff: 5s for the first retry, then x5 per extra retry
/// (5s, 25s, 125s, 625s, ...), saturating and clamped per step.
/// A `Retry-After` hint can only lengthen a step, never shorten it, so a
/// 1-second burst hint cannot burn the retry budget.
fn rate_limit_retry_delay(
    headers: Option<&HashMap<String, String>>,
    attempt: usize,
) -> Duration {
    let staged = RATE_LIMIT_RETRY_BASE_DELAY_SECS.saturating_pow(attempt as u32 + 1);
    let hinted = headers
        .and_then(|headers| headers.get("retry-after"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_secs(staged.max(hinted)).min(MAX_RATE_LIMIT_DELAY)
}

fn sleep_for_retry_while_connected(
    duration: Duration,
    client: Option<&TcpStream>,
) -> anyhow::Result<()> {
    ensure_client_connected(client)?;
    if cfg!(test) {
        return Ok(());
    }
    let started = Instant::now();
    while started.elapsed() < duration {
        let remaining = duration.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(100)));
        ensure_client_connected(client)?;
    }
    Ok(())
}

fn ensure_client_connected(client: Option<&TcpStream>) -> anyhow::Result<()> {
    let Some(client) = client else {
        return Ok(());
    };
    let previous_timeout = client.read_timeout().ok().flatten();
    client.set_read_timeout(Some(Duration::from_millis(1))).ok();
    let mut byte = [0_u8; 1];
    let result = client.peek(&mut byte);
    client.set_read_timeout(previous_timeout).ok();
    match result {
        Ok(0) => bail!(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Codex client disconnected during upstream retry",
        )),
        Ok(_) => Ok(()),
        Err(error) if stream_read_timed_out(&error) => Ok(()),
        Err(error) => Err(error).context("could not inspect Codex client during upstream retry"),
    }
}

fn write_upstream_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &mut HashMap<String, String>,
    body: &[u8],
    upstream: &str,
) -> anyhow::Result<()> {
    let (host, port) = listen_host_port(upstream)?;
    headers.insert("host".to_owned(), format!("{host}:{port}"));
    headers.insert("content-length".to_owned(), body.len().to_string());
    headers.remove("transfer-encoding");
    headers.insert("connection".to_owned(), "close".to_owned());
    let mut outgoing = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers.iter() {
        if name == "expect" {
            continue;
        }
        outgoing.push_str(name);
        outgoing.push_str(": ");
        outgoing.push_str(value);
        outgoing.push_str("\r\n");
    }
    outgoing.push_str("\r\n");
    write_all_socket(stream, outgoing.as_bytes())?;
    write_all_socket(stream, body)?;
    Ok(())
}

fn forward_error_exchange(
    upstream: &str,
    path: &str,
    original_headers: &HashMap<String, String>,
    body: &[u8],
) -> anyhow::Result<(u16, Vec<u8>, Vec<u8>)> {
    let mut stream = connect_upstream(upstream)?;
    let mut headers = original_headers.clone();
    headers.insert("content-type".to_owned(), "application/json".to_owned());
    write_upstream_request(&mut stream, "POST", path, &mut headers, body, upstream)?;
    let (response_headers, leftover) = read_headers(&mut stream)?;
    let status = parse_status(&String::from_utf8_lossy(&response_headers)).unwrap_or(502);
    let mapped = parse_headers(&String::from_utf8_lossy(&response_headers));
    let mut response_body = leftover;
    if let Some(length) = mapped
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        read_exact_more(&mut stream, &mut response_body, length)?;
        response_body.truncate(length);
    } else {
        let _ = read_to_end_socket(&mut stream, &mut response_body);
    }
    Ok((status, response_headers, response_body))
}

fn read_exact_more(stream: &mut TcpStream, body: &mut Vec<u8>, length: usize) -> anyhow::Result<()> {
    while body.len() < length {
        let mut buffer = [0_u8; 8192];
        let read = read_socket(stream, &mut buffer)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
        if body.len() > MAX_BODY_BYTES {
            bail!("HTTP body is too large");
        }
    }
    if body.len() < length {
        bail!("incomplete HTTP body: expected {length} bytes, received {}", body.len());
    }
    Ok(())
}

fn rebuild_response(status: u16, headers: &HashMap<String, String>, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
    let mut wrote_length = false;
    for (name, value) in headers {
        if name == "content-length" {
            out.push_str(&format!("content-length: {}\r\n", body.len()));
            wrote_length = true;
            continue;
        }
        if name == "transfer-encoding" || name == "connection" {
            continue;
        }
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    if !wrote_length {
        out.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    out.push_str("connection: close\r\n\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn parse_status(header_text: &str) -> Option<u16> {
    header_text
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn parse_headers(header_text: &str) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for line in header_text.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    headers
}

fn read_headers(stream: &mut TcpStream) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let read = read_socket(stream, &mut buffer)?;
        if read == 0 {
            bail!("incomplete HTTP headers");
        }
        data.extend_from_slice(&buffer[..read]);
        if let Some(index) = find_double_crlf(&data) {
            let headers = data[..index + 4].to_vec();
            let leftover = data[index + 4..].to_vec();
            return Ok((headers, leftover));
        }
        if data.len() > MAX_HEADER_BYTES {
            bail!("HTTP headers are too large");
        }
    }
}

fn find_double_crlf(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_type_of(headers: &HashMap<String, String>) -> String {
    headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default()
}

fn write_sse_prelude(
    client: &mut TcpStream,
    headers: &HashMap<String, String>,
) -> anyhow::Result<()> {
    let mut prelude = String::from("HTTP/1.1 200 OK\r\n");
    for (name, value) in headers {
        if name == "content-length" || name == "transfer-encoding" || name == "connection" {
            continue;
        }
        prelude.push_str(name);
        prelude.push_str(": ");
        prelude.push_str(value);
        prelude.push_str("\r\n");
    }
    prelude.push_str("connection: close\r\n\r\n");
    write_all_socket(client, prelude.as_bytes())?;
    Ok(())
}

fn take_sse_event(carry: &mut String) -> Option<String> {
    let lf = carry.find("\n\n");
    let crlf = carry.find("\r\n\r\n");
    let (end, skip) = match (lf, crlf) {
        (Some(lf_at), Some(crlf_at)) if crlf_at <= lf_at => (crlf_at, 4),
        (Some(lf_at), _) => (lf_at, 2),
        (None, Some(crlf_at)) => (crlf_at, 4),
        (None, None) => return None,
    };
    let event = carry[..end + skip].to_owned();
    carry.replace_range(..end + skip, "");
    Some(event)
}

fn sse_event_is_terminal(event: &str) -> bool {
    event.lines().any(|line| {
        let data = line.trim_start().strip_prefix("data:").map(str::trim);
        if data == Some("[DONE]") {
            return true;
        }
        data.and_then(|value| serde_json::from_str::<Value>(value).ok())
            .and_then(|json| json.get("type").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|kind| {
                matches!(
                    kind.as_str(),
                    "response.completed" | "response.failed" | "response.incomplete"
                )
            })
    })
}

/// What a mid-stream retry must reproduce before new content may flow to the
/// client: the exact output text already delivered, plus the stream ids so
/// post-resume events can be rewritten to match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StreamResume {
    prefix: String,
    response_id: Option<String>,
    item_id: Option<String>,
}

/// Result of forwarding an SSE stream. `RetryableBeforeFirstEvent` means the
/// upstream died (or exceeded the silence cap) before any real event reached
/// the client, so the whole request can be transparently retried.
/// `RetryableWithPrefix` means only plain output text reached the client, so
/// a retry can resume by suppressing the duplicated prefix. `ReconcileFailed`
/// means the retry diverged from the delivered text; nothing extra was
/// forwarded, so the caller can still close with a clean terminal event.
/// `IncompleteAgentTurn` means the model ended with commentary and no tool
/// call; `response.completed` was held so the caller can nudge a follow-up.
#[derive(Debug, PartialEq, Eq)]
enum SseForward {
    Done,
    RetryableBeforeFirstEvent,
    RetryableWithPrefix(StreamResume),
    ReconcileFailed,
    IncompleteAgentTurn(IncompleteTurn),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IncompleteTurn {
    delivered_text: String,
    turn_text: String,
    response_id: Option<String>,
    item_id: Option<String>,
    held_completed: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ContinuePoint {
    response_id: Option<String>,
    new_response_id: Option<String>,
}

enum SseFlow {
    Continue,
    AbortReconcile,
}

/// State for a mid-stream retry: the new stream is suppressed until it
/// regenerates exactly the text the client already received.
struct ReconcileState {
    resume: StreamResume,
    regenerated: String,
    caught_up: bool,
    retry_response_id: Option<String>,
    retry_item_id: Option<String>,
}

/// Everything tracked while forwarding an SSE stream: whether real events
/// reached the client, how much output text was delivered, and whether the
/// stream stayed plain-text (a reconnect replays the answer from scratch, so
/// only plain-text partial streams can be reconciled by skipping the
/// duplicated prefix).
struct SseSink<'a> {
    client: &'a mut TcpStream,
    terminal: bool,
    sent_events: bool,
    delivered_text: String,
    turn_text: String,
    text_only: bool,
    had_tool_call: bool,
    hold_text_completed: bool,
    held_completed: Option<String>,
    response_id: Option<String>,
    item_id: Option<String>,
    reconcile: Option<ReconcileState>,
    continue_point: Option<ContinuePoint>,
}

/// Event types a mid-stream text retry can reproduce or suppress safely.
/// Anything else (tool calls, reasoning items, unknown events) makes the
/// partial stream non-retryable.
const TEXT_RETRY_EVENT_TYPES: [&str; 11] = [
    "response.created",
    "response.in_progress",
    "response.output_item.added",
    "response.content_part.added",
    "response.output_text.delta",
    "response.output_text.done",
    "response.content_part.done",
    "response.output_item.done",
    "response.completed",
    "response.failed",
    "response.incomplete",
];

fn sse_event_data(event: &str) -> Option<Value> {
    event.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix("data:")
            .map(str::trim)
            .and_then(|data| serde_json::from_str::<Value>(data).ok())
    })
}

fn sse_event_type(event: &str) -> Option<String> {
    sse_event_data(event)
        .and_then(|json| json.get("type").and_then(Value::as_str).map(str::to_owned))
}

fn sse_event_item_type(event: &str) -> Option<String> {
    sse_event_data(event).and_then(|json| {
        json.get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn sse_error_code_and_message(event: &str) -> (String, String) {
    let Some(json) = sse_event_data(event) else {
        return (String::new(), String::new());
    };
    let error = json
        .get("response")
        .and_then(|response| response.get("error"))
        .or_else(|| json.get("error"));
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    (code, message)
}

/// Host synthesizes `response.failed` with CR-UP-0014 when CLIProxyAPI
/// closes the SSE body before a terminal event. That is a dropped stream,
/// not a model/business failure, so the gateway must retry instead of
/// showing Codex "upstream stream ended before a terminal event".
fn sse_event_has_tool_call(event: &str) -> bool {
    if matches!(
        sse_event_item_type(event).as_deref(),
        Some(
            "function_call"
                | "custom_tool_call"
                | "local_shell_call"
                | "computer_call"
                | "apply_patch_call"
        )
    ) {
        return true;
    }
    matches!(
        sse_event_type(event).as_deref(),
        Some("response.function_call_arguments.delta" | "response.function_call_arguments.done")
    )
}

fn sse_event_is_retryable_interrupt(event: &str) -> bool {
    if sse_event_type(event).as_deref() != Some("response.failed") {
        return false;
    }
    let (code, message) = sse_error_code_and_message(event);
    let message = message.to_ascii_lowercase();
    // Host also stamps CR-UP-0014 onto genuine failed events that had no
    // code. Only treat it as a dropped stream when the message says so.
    code.eq_ignore_ascii_case("upstream_stream_interrupted")
        || message.contains("before a terminal event")
        || message.contains("ended before completion")
        || message.contains("stream interrupted")
}

/// Rewrite an `output_text.delta` event so it carries only the not-yet
/// delivered suffix, with retry-stream ids replaced by the ids the client
/// already saw.
fn rewrite_delta_event(
    event: &str,
    new_delta: &str,
    id_rewrites: &[(String, String)],
) -> Option<String> {
    let mut out = String::new();
    for line in event.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(data) = line.trim_start().strip_prefix("data:") {
            let mut json = serde_json::from_str::<Value>(data.trim()).ok()?;
            json["delta"] = Value::String(new_delta.to_owned());
            let mut data_text = serde_json::to_string(&json).ok()?;
            for (from, to) in id_rewrites {
                data_text = data_text.replace(from.as_str(), to.as_str());
            }
            out.push_str("data: ");
            out.push_str(&data_text);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
    Some(out)
}

fn forward_event_with_rewrites(
    client: &mut TcpStream,
    event: &str,
    id_rewrites: &[(String, String)],
) -> anyhow::Result<()> {
    let mut text = rewrite_sse_text(event);
    for (from, to) in id_rewrites {
        text = text.replace(from.as_str(), to.as_str());
    }
    write_all_socket(client, text.as_bytes())?;
    Ok(())
}

impl<'a> SseSink<'a> {
    #[cfg(test)]
    fn new(client: &'a mut TcpStream, resume: Option<StreamResume>) -> SseSink<'a> {
        Self::with_policy(client, resume, None, false)
    }

    fn with_policy(
        client: &'a mut TcpStream,
        resume: Option<StreamResume>,
        continue_point: Option<ContinuePoint>,
        hold_text_completed: bool,
    ) -> SseSink<'a> {
        let delivered = resume
            .as_ref()
            .map(|resume| resume.prefix.clone())
            .unwrap_or_default();
        let response_id = continue_point
            .as_ref()
            .and_then(|point| point.response_id.clone())
            .or_else(|| resume.as_ref().and_then(|resume| resume.response_id.clone()));
        let item_id = resume.as_ref().and_then(|resume| resume.item_id.clone());
        SseSink {
            client,
            terminal: false,
            sent_events: false,
            delivered_text: delivered,
            turn_text: String::new(),
            text_only: true,
            had_tool_call: false,
            hold_text_completed,
            held_completed: None,
            response_id,
            item_id,
            reconcile: resume.map(|resume| ReconcileState {
                resume,
                regenerated: String::new(),
                caught_up: false,
                retry_response_id: None,
                retry_item_id: None,
            }),
            continue_point,
        }
    }

    fn continue_rewrites(&self) -> Vec<(String, String)> {
        match (
            self.continue_point
                .as_ref()
                .and_then(|point| point.new_response_id.clone()),
            self.response_id.clone(),
        ) {
            (Some(from), Some(to)) if from != to => vec![(from, to)],
            _ => Vec::new(),
        }
    }

    fn resume_point(&self) -> StreamResume {
        StreamResume {
            prefix: self.delivered_text.clone(),
            response_id: self.response_id.clone(),
            item_id: self.item_id.clone(),
        }
    }


    fn push_event(&mut self, event: &str) -> anyhow::Result<SseFlow> {
        let kind = sse_event_type(event);
        if sse_event_is_retryable_interrupt(event) {
            // Do not forward Host's synthetic failed event and do not mark
            // the stream terminal. The caller then hits EOF and the existing
            // retry/reconcile path can recover.
            return Ok(SseFlow::Continue);
        }
        if sse_event_has_tool_call(event) {
            self.had_tool_call = true;
            self.text_only = false;
        }
        if let Some(json) = sse_event_data(event) {
            if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                if !extract_leaked_tool_calls(delta).1.is_empty() {
                    self.had_tool_call = true;
                }
            }
        }
        if self.continue_point.is_some() {
            match kind.as_deref() {
                Some("response.created") => {
                    if let Some(point) = self.continue_point.as_mut() {
                        point.new_response_id = sse_event_data(event)
                            .and_then(|json| json.get("response").cloned())
                            .and_then(|response| {
                                response.get("id").and_then(Value::as_str).map(str::to_owned)
                            });
                    }
                    return Ok(SseFlow::Continue);
                }
                Some("response.in_progress") => return Ok(SseFlow::Continue),
                _ => {}
            }
        }
        let hold_completed = self.hold_text_completed
            && kind.as_deref() == Some("response.completed")
            && !self.had_tool_call;
        if hold_completed && self.reconcile.is_none() {
            let mut held = rewrite_sse_text(event);
            for (from, to) in self.continue_rewrites() {
                held = held.replace(&from, &to);
            }
            self.held_completed = Some(held);
            return Ok(SseFlow::Continue);
        }
        let self_continue_rewrites = self.continue_rewrites();
        let SseSink {
            client,
            terminal,
            sent_events,
            delivered_text,
            turn_text,
            text_only,
            had_tool_call: _,
            hold_text_completed: _,
            held_completed,
            response_id,
            item_id,
            reconcile,
            continue_point: _,
        } = self;
        if let Some(recon) = reconcile {
            let Some(kind) = kind else {
                // Keep-alive comments and event-less lines carry no content.
                return Ok(SseFlow::Continue);
            };
            match kind.as_str() {
                "response.created" => {
                    recon.retry_response_id = sse_event_data(event)
                        .and_then(|json| json.get("response").cloned())
                        .and_then(|response| response.get("id").and_then(Value::as_str).map(str::to_owned));
                    Ok(SseFlow::Continue)
                }
                "response.in_progress" => Ok(SseFlow::Continue),
                "response.output_item.added" | "response.content_part.added" => {
                    if kind == "response.output_item.added"
                        && sse_event_item_type(event).as_deref() != Some("message")
                    {
                        // The retry switched to tool calls; cannot reconcile.
                        return Ok(SseFlow::AbortReconcile);
                    }
                    Ok(SseFlow::Continue)
                }
                "response.output_text.delta" => {
                    let json = sse_event_data(event);
                    let delta = json
                        .as_ref()
                        .and_then(|json| json.get("delta"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if recon.retry_item_id.is_none() {
                        recon.retry_item_id = json
                            .as_ref()
                            .and_then(|json| json.get("item_id"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    if recon.caught_up {
                        delivered_text.push_str(delta);
                        turn_text.push_str(delta);
                        let mut rewrites = Vec::new();
                        if let (Some(from), Some(to)) = (&recon.retry_response_id, response_id.as_ref()) {
                            rewrites.push((from.clone(), to.clone()));
                        }
                        if let (Some(from), Some(to)) = (&recon.retry_item_id, item_id.as_ref()) {
                            rewrites.push((from.clone(), to.clone()));
                        }
                        forward_event_with_rewrites(client, event, &rewrites)?;
                        *sent_events = true;
                        return Ok(SseFlow::Continue);
                    }
                    let new_regenerated = format!("{}{delta}", recon.regenerated);
                    turn_text.push_str(delta);
                    if new_regenerated.len() <= recon.resume.prefix.len() {
                        if !recon.resume.prefix.starts_with(&new_regenerated) {
                            return Ok(SseFlow::AbortReconcile);
                        }
                        recon.regenerated = new_regenerated;
                        return Ok(SseFlow::Continue);
                    }
                    if !new_regenerated.starts_with(&recon.resume.prefix) {
                        return Ok(SseFlow::AbortReconcile);
                    }
                    let suffix = new_regenerated[recon.resume.prefix.len()..].to_owned();
                    recon.regenerated = new_regenerated;
                    recon.caught_up = true;
                    delivered_text.push_str(&suffix);
                    let mut rewrites = Vec::new();
                    if let (Some(from), Some(to)) = (&recon.retry_response_id, response_id.as_ref()) {
                        rewrites.push((from.clone(), to.clone()));
                    }
                    if let (Some(from), Some(to)) = (&recon.retry_item_id, item_id.as_ref()) {
                        rewrites.push((from.clone(), to.clone()));
                    }
                    if let Some(rewritten) = rewrite_delta_event(event, &suffix, &rewrites) {
                        write_all_socket(client, rewrite_sse_text(&rewritten).as_bytes())?;
                        *sent_events = true;
                    }
                    Ok(SseFlow::Continue)
                }
                "response.completed" | "response.failed" | "response.incomplete" => {
                    if !recon.caught_up
                        && kind == "response.completed"
                        && recon.regenerated != recon.resume.prefix
                    {
                        // The retry finished with different text; never
                        // forward a mismatched completion.
                        return Ok(SseFlow::AbortReconcile);
                    }
                    let mut rewrites = Vec::new();
                    if let (Some(from), Some(to)) = (&recon.retry_response_id, response_id.as_ref()) {
                        rewrites.push((from.clone(), to.clone()));
                    }
                    if let (Some(from), Some(to)) = (&recon.retry_item_id, item_id.as_ref()) {
                        rewrites.push((from.clone(), to.clone()));
                    }
                    if hold_completed {
                        let mut held = rewrite_sse_text(event);
                        for (from, to) in &rewrites {
                            held = held.replace(from.as_str(), to.as_str());
                        }
                        *held_completed = Some(held);
                        return Ok(SseFlow::Continue);
                    }
                    *terminal = true;
                    forward_event_with_rewrites(client, event, &rewrites)?;
                    *sent_events = true;
                    Ok(SseFlow::Continue)
                }
                "response.output_text.done"
                | "response.content_part.done"
                | "response.output_item.done" => {
                    if recon.caught_up {
                        let mut rewrites = Vec::new();
                        if let (Some(from), Some(to)) = (&recon.retry_response_id, response_id.as_ref()) {
                            rewrites.push((from.clone(), to.clone()));
                        }
                        if let (Some(from), Some(to)) = (&recon.retry_item_id, item_id.as_ref()) {
                            rewrites.push((from.clone(), to.clone()));
                        }
                        forward_event_with_rewrites(client, event, &rewrites)?;
                        *sent_events = true;
                    }
                    Ok(SseFlow::Continue)
                }
                _ => {
                    if recon.caught_up {
                        let mut rewrites = Vec::new();
                        if let (Some(from), Some(to)) = (&recon.retry_response_id, response_id.as_ref()) {
                            rewrites.push((from.clone(), to.clone()));
                        }
                        if let (Some(from), Some(to)) = (&recon.retry_item_id, item_id.as_ref()) {
                            rewrites.push((from.clone(), to.clone()));
                        }
                        forward_event_with_rewrites(client, event, &rewrites)?;
                        *sent_events = true;
                        Ok(SseFlow::Continue)
                    } else {
                        Ok(SseFlow::AbortReconcile)
                    }
                }
            }
        } else {
            if let Some(kind) = kind.as_deref() {
                if !TEXT_RETRY_EVENT_TYPES.contains(&kind) {
                    *text_only = false;
                }
                match kind {
                    "response.created" => {
                        if response_id.is_none() {
                            *response_id = sse_event_data(event)
                                .and_then(|json| json.get("response").cloned())
                                .and_then(|response| {
                                    response.get("id").and_then(Value::as_str).map(str::to_owned)
                                });
                        }
                    }
                    "response.output_item.added" | "response.output_item.done" => {
                        if sse_event_item_type(event).as_deref() != Some("message") {
                            *text_only = false;
                        }
                    }
                    "response.output_text.delta" => {
                        if let Some(json) = sse_event_data(event) {
                            if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                                delivered_text.push_str(delta);
                                turn_text.push_str(delta);
                            }
                            if item_id.is_none() {
                                *item_id = json
                                    .get("item_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned);
                            }
                        }
                    }
                    _ => {}
                }
            }
            *terminal |= sse_event_is_terminal(event);
            forward_event_with_rewrites(client, event, &self_continue_rewrites)?;
            *sent_events = true;
            Ok(SseFlow::Continue)
        }
    }

    /// Upstream ended (EOF, read error, silence cap, or corrupt frame).
    fn eof_outcome(&mut self, carry: &str) -> anyhow::Result<SseForward> {
        if let Some(held) = self.held_completed.take() {
            if !carry.is_empty() {
                write_all_socket(self.client, rewrite_sse_text(carry).as_bytes())?;
            }
            return Ok(SseForward::IncompleteAgentTurn(IncompleteTurn {
                delivered_text: self.delivered_text.clone(),
                turn_text: self.turn_text.clone(),
                response_id: self.response_id.clone(),
                item_id: self.item_id.clone(),
                held_completed: held,
            }));
        }
        if self.terminal {
            self.finish(carry)?;
            return Ok(SseForward::Done);
        }
        if self.continue_point.is_some() && self.sent_events {
            if !carry.is_empty() {
                write_all_socket(self.client, rewrite_sse_text(carry).as_bytes())?;
            }
            let payload = serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id.clone().unwrap_or_default(),
                    "status": "completed",
                }
            });
            write_all_socket(
                self.client,
                format!("data: {payload}\n\n").as_bytes(),
            )?;
            return Ok(SseForward::Done);
        }
        if self.reconcile.is_some() {
            // The retry died before completing; keep retrying with whatever
            // text has been delivered so far.
            return Ok(SseForward::RetryableWithPrefix(self.resume_point()));
        }
        if !self.sent_events {
            return Ok(SseForward::RetryableBeforeFirstEvent);
        }
        // Tool calls and reasoning items already reached the client, but the
        // upstream still dropped before a terminal event. Retry with the
        // delivered prefix so a sudden disconnect does not end the turn.
        Ok(SseForward::RetryableWithPrefix(self.resume_point()))
    }

    fn finish(&mut self, carry: &str) -> anyhow::Result<()> {
        finish_sse(self.client, carry, self.terminal)
    }
}

/// Forward every complete SSE event in `carry`; incomplete tails stay
/// buffered. Returns `AbortReconcile` when a retry stream diverged from the
/// text the client already received.
fn push_sse_events(sink: &mut SseSink, carry: &mut String) -> anyhow::Result<SseFlow> {
    while let Some(complete) = take_sse_event(carry) {
        if let SseFlow::AbortReconcile = sink.push_event(&complete)? {
            return Ok(SseFlow::AbortReconcile);
        }
    }
    Ok(SseFlow::Continue)
}

fn finish_sse(client: &mut TcpStream, carry: &str, terminal: bool) -> anyhow::Result<()> {
    if !carry.is_empty() {
        write_all_socket(client, rewrite_sse_text(carry).as_bytes())?;
    }
    if !terminal {
        write_all_socket(client, b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"upstream_stream_interrupted\",\"message\":\"Upstream stream ended before completion.\"}}}\n\n")?;
    }
    Ok(())
}

fn decode_chunked_to_sse(
    sink: &mut SseSink,
    upstream: &mut TcpStream,
    leftover: Vec<u8>,
) -> anyhow::Result<SseForward> {
    let mut raw = leftover;
    let mut decoded = String::new();
    let mut last_activity = Instant::now();
    loop {
        if let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") {
            let line = String::from_utf8_lossy(&raw[..line_end]).into_owned();
            let size = match parse_chunk_size(&line) {
                Ok(size) => size,
                Err(_) => {
                    // A corrupt chunk frame is treated like an upstream drop:
                    // pre-content it retries transparently, mid-content a
                    // plain-text prefix reconciles on the next attempt.
                    if let SseFlow::AbortReconcile = push_sse_events(sink, &mut decoded)? {
                        return Ok(SseForward::ReconcileFailed);
                    }
                    return sink.eof_outcome("");
                }
            };
            let after_size = line_end + 2;
            if size == 0 {
                if let SseFlow::AbortReconcile = push_sse_events(sink, &mut decoded)? {
                    return Ok(SseForward::ReconcileFailed);
                }
                return sink.eof_outcome(&decoded);
            }
            if raw.len() < after_size + size + 2 {
                match read_upstream_chunk(upstream, &mut raw, &mut last_activity, sink.client)? {
                    ChunkRead::Received => continue,
                    ChunkRead::Ended => return sink.eof_outcome(&decoded),
                }
            }
            decoded.push_str(&String::from_utf8_lossy(&raw[after_size..after_size + size]));
            raw.drain(..after_size + size + 2);
            if let SseFlow::AbortReconcile = push_sse_events(sink, &mut decoded)? {
                return Ok(SseForward::ReconcileFailed);
            }
            continue;
        }
        match read_upstream_chunk(upstream, &mut raw, &mut last_activity, sink.client)? {
            ChunkRead::Received => continue,
            ChunkRead::Ended => return sink.eof_outcome(&decoded),
        }
    }
}

enum ChunkRead {
    Received,
    Ended,
}

/// Read more upstream bytes for the chunked SSE decoder. A read timeout only
/// means the provider is quiet; the Codex session is kept alive with an SSE
/// comment and the wait continues until the absolute silence cap is reached.
fn read_upstream_chunk(
    upstream: &mut TcpStream,
    raw: &mut Vec<u8>,
    last_activity: &mut Instant,
    client: &mut TcpStream,
) -> anyhow::Result<ChunkRead> {
    let mut buffer = [0_u8; 8192];
    match read_socket(upstream, &mut buffer) {
        Ok(0) => Ok(ChunkRead::Ended),
        Ok(read) => {
            *last_activity = Instant::now();
            raw.extend_from_slice(&buffer[..read]);
            Ok(ChunkRead::Received)
        }
        Err(error) if stream_read_timed_out(&error) => {
            if last_activity.elapsed() >= STREAM_MAX_SILENCE {
                return Ok(ChunkRead::Ended);
            }
            if send_sse_keepalive(client).is_err() {
                // The Codex client is gone; stop the worker quietly.
                return Err(error).context("client disconnected during an upstream pause");
            }
            Ok(ChunkRead::Received)
        }
        Err(_) => Ok(ChunkRead::Ended),
    }
}

fn stream_read_timed_out(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ) || matches!(error.raw_os_error(), Some(10035 | 10060 | 11 | 35))
}

/// An SSE comment line is ignored by event parsers but resets Codex's stream
/// idle timer, so long Grok/Gemini reasoning pauses no longer abort the task.
fn send_sse_keepalive(client: &mut TcpStream) -> anyhow::Result<()> {
    write_all_socket(client, b": codex-router keep-alive\n\n")?;
    Ok(())
}

fn parse_chunk_size(line: &str) -> anyhow::Result<usize> {
    let size_text = line.split(';').next().unwrap_or_default().trim();
    usize::from_str_radix(size_text, 16).context("invalid upstream chunk size")
}

fn forward_plain_sse(
    sink: &mut SseSink,
    upstream: &mut TcpStream,
    leftover: Vec<u8>,
) -> anyhow::Result<SseForward> {
    let mut carry = String::from_utf8_lossy(&leftover).into_owned();
    let mut last_activity = Instant::now();
    loop {
        if let SseFlow::AbortReconcile = push_sse_events(sink, &mut carry)? {
            return Ok(SseForward::ReconcileFailed);
        }
        let mut buffer = [0_u8; 8192];
        let read = match read_socket(upstream, &mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                last_activity = Instant::now();
                read
            }
            Err(error) if stream_read_timed_out(&error) => {
                if last_activity.elapsed() >= STREAM_MAX_SILENCE {
                    return sink.eof_outcome(&carry);
                }
                if send_sse_keepalive(sink.client).is_err() {
                    // The Codex client is gone; stop the worker quietly.
                    return Ok(SseForward::Done);
                }
                continue;
            }
            Err(_) => return sink.eof_outcome(&carry),
        };
        carry.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    if let SseFlow::AbortReconcile = push_sse_events(sink, &mut carry)? {
        return Ok(SseForward::ReconcileFailed);
    }
    sink.eof_outcome(&carry)
}

fn forward_agent_sse(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    headers: &HashMap<String, String>,
    leftover: Vec<u8>,
    resume: Option<StreamResume>,
    continue_point: Option<ContinuePoint>,
    hold_text_completed: bool,
) -> anyhow::Result<SseForward> {
    // The SSE prelude is written by the caller, once per client connection,
    // so a transparent retry never duplicates response headers. A mid-stream
    // retry carries the delivered text prefix: the new stream is suppressed
    // until it regenerates that exact prefix, then only the suffix flows.
    // Poll the upstream on a short cadence instead of one long blocking read
    // so keep-alive comments can hold the Codex session open while Grok or
    // Gemini is silent during long reasoning or provider-side failover.
    upstream.set_nonblocking(false).ok();
    upstream
        .set_read_timeout(Some(STREAM_POLL_TIMEOUT))
        .ok();
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let mut sink = SseSink::with_policy(client, resume, continue_point, hold_text_completed);
    if chunked {
        decode_chunked_to_sse(&mut sink, upstream, leftover)
    } else {
        forward_plain_sse(&mut sink, upstream, leftover)
    }
}

/// Buffer the rest of an upstream response body. Read errors propagate so
/// the caller can retry the whole request while nothing has reached the
/// client yet.
fn read_full_body(
    stream: &mut TcpStream,
    headers: &HashMap<String, String>,
    mut body: Vec<u8>,
) -> anyhow::Result<Vec<u8>> {
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return read_chunked_body(stream, body);
    }
    if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        read_exact_more(stream, &mut body, length)?;
        if body.len() < length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "upstream closed the connection mid-body",
            )
            .into());
        }
        body.truncate(length);
    } else {
        read_to_end_socket(stream, &mut body)?;
    }
    Ok(body)
}

fn read_chunked_body(stream: &mut TcpStream, mut raw: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = loop {
            if let Some(position) = raw.windows(2).position(|window| window == b"\r\n") {
                break position;
            }
            let mut buffer = [0_u8; 8192];
            let read = read_socket(stream, &mut buffer)?;
            if read == 0 {
                bail!("upstream closed during chunk header");
            }
            raw.extend_from_slice(&buffer[..read]);
        };
        let line = String::from_utf8_lossy(&raw[..line_end]);
        let size = parse_chunk_size(&line)?;
        let data_start = line_end + 2;
        let required = data_start + size + 2;
        while raw.len() < required {
            let mut buffer = [0_u8; 8192];
            let read = read_socket(stream, &mut buffer)?;
            if read == 0 {
                bail!("upstream closed during chunk body");
            }
            raw.extend_from_slice(&buffer[..read]);
            if decoded.len() + raw.len() > MAX_BODY_BYTES {
                bail!("HTTP body is too large");
            }
        }
        if raw.get(data_start + size..required) != Some(b"\r\n") {
            bail!("invalid upstream chunk terminator");
        }
        if size == 0 {
            return Ok(decoded);
        }
        decoded.extend_from_slice(&raw[data_start..data_start + size]);
        if decoded.len() > MAX_BODY_BYTES {
            bail!("HTTP body is too large");
        }
        raw.drain(..required);
    }
}

fn decode_request_content_encoding(encoding: &str, body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoded = body.to_vec();
    for coding in encoding
        .split(',')
        .map(str::trim)
        .filter(|coding| !coding.is_empty() && !coding.eq_ignore_ascii_case("identity"))
        .rev()
    {
        let mut output = Vec::new();
        match coding.to_ascii_lowercase().as_str() {
            "gzip" => GzDecoder::new(decoded.as_slice()).read_to_end(&mut output)?,
            "deflate" => DeflateDecoder::new(decoded.as_slice()).read_to_end(&mut output)?,
            "zstd" => {
                output = zstd::stream::decode_all(decoded.as_slice())?;
                output.len()
            }
            other => bail!("unsupported request content encoding: {other}"),
        };
        decoded = output;
    }
    Ok(decoded)
}

fn forward_rewritten_json_body(
    client: &mut TcpStream,
    headers: &HashMap<String, String>,
    mut body: Vec<u8>,
) -> anyhow::Result<()> {
    if let Ok(mut json) = serde_json::from_slice::<Value>(&body) {
        rewrite_provider_json(&mut json);
        crate::logic::responses_compat::strip_think_tags_from_value(&mut json);
        body = serde_json::to_vec(&json)?;
    }
    send_raw(client, &rebuild_response(200, headers, &body))?;
    Ok(())
}

fn send_simple(stream: &mut TcpStream, status: u16, body: &[u8]) -> anyhow::Result<()> {
    send_status(stream, status, "text/plain", body)
}

fn send_gateway_unavailable(stream: &mut TcpStream, error: &anyhow::Error) -> anyhow::Result<()> {
    gateway_log("request.upstream_unavailable", &format!("{error:#}"));
    let body = br#"{"error":{"code":"router_upstream_unavailable","message":"Router upstream connection was interrupted. Please retry this task."}}"#;
    send_status(stream, 502, "application/json", body)
}

fn send_status(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    write_all_socket(stream, header.as_bytes())?;
    write_all_socket(stream, body)?;
    Ok(())
}

fn send_raw(stream: &mut TcpStream, payload: &[u8]) -> anyhow::Result<()> {
    write_all_socket(stream, payload)?;
    Ok(())
}

/// Write upstream response headers to the client with `connection: close`
/// forced. The gateway always closes the client socket after one response;
/// forwarding an upstream keep-alive header would let the Codex client pool a
/// connection that is already gone, and the next request on that pooled
/// connection fails instantly with "error sending request".
fn write_headers_forced_close(client: &mut TcpStream, raw_headers: &[u8]) -> anyhow::Result<()> {
    write_all_socket(client, &force_connection_close_headers(raw_headers))?;
    Ok(())
}

fn client_connection_ended(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            ) || matches!(io.raw_os_error(), Some(10053 | 10054 | 32 | 104))
        })
    })
}

fn io_interrupted(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Interrupted || matches!(error.raw_os_error(), Some(4))
}

fn io_would_block(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || matches!(error.raw_os_error(), Some(10035 | 11 | 35))
}

fn prepare_client_socket(stream: &TcpStream) -> anyhow::Result<()> {
    // Windows inherits non-blocking from the listener. Leaving the accepted
    // socket non-blocking turns a full send buffer into WSAEWOULDBLOCK
    // (os error 10035), which Codex surfaces as "error sending request".
    stream
        .set_nonblocking(false)
        .context("could not switch the Codex socket to blocking I/O")?;
    stream.set_nodelay(true).ok();
    stream
        .set_read_timeout(Some(HEADER_TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT)))
        .context("could not set Codex socket timeouts")?;
    Ok(())
}

fn prepare_upstream_socket(stream: &TcpStream) {
    stream.set_nonblocking(false).ok();
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT)).ok();
}

fn read_socket(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(error) if io_interrupted(&error) => continue,
            other => return other,
        }
    }
}

fn write_all_socket(stream: &mut TcpStream, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        match stream.write(buf) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "socket write returned zero bytes",
                ));
            }
            Ok(wrote) => buf = &buf[wrote..],
            Err(error) if io_interrupted(&error) || io_would_block(&error) => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    stream.flush()
}

fn copy_socket(src: &mut TcpStream, dst: &mut TcpStream) -> std::io::Result<u64> {
    let mut buffer = [0_u8; 8192];
    let mut total = 0_u64;
    loop {
        match read_socket(src, &mut buffer)? {
            0 => return Ok(total),
            n => {
                write_all_socket(dst, &buffer[..n])?;
                total += n as u64;
            }
        }
    }
}

fn read_to_end_socket(stream: &mut TcpStream, body: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut buffer = [0_u8; 8192];
    let mut total = 0;
    loop {
        match read_socket(stream, &mut buffer)? {
            0 => return Ok(total),
            n => {
                body.extend_from_slice(&buffer[..n]);
                total += n;
                if body.len() > MAX_BODY_BYTES {
                    return Err(std::io::Error::other("HTTP body is too large"));
                }
            }
        }
    }
}

fn close_client_gracefully(stream: &mut TcpStream) {
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    stream.set_read_timeout(Some(CLIENT_CLOSE_DRAIN)).ok();
    let mut buf = [0_u8; 512];
    for _ in 0..8 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

fn force_connection_close_headers(raw_headers: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw_headers);
    let mut out = String::new();
    let mut wrote_connection = false;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.to_ascii_lowercase().starts_with("connection:") {
            wrote_connection = true;
            out.push_str("connection: close\r\n");
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !wrote_connection {
        out.push_str("connection: close\r\n");
    }
    out.push_str("\r\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    static OUTPUT_BUDGET_TEST: Mutex<()> = Mutex::new(());

    #[test]
    fn inject_max_output_tokens_honors_the_configured_model_limit() {
        let _guard = OUTPUT_BUDGET_TEST.lock().unwrap();
        // Unique fixture model ids keep this test isolated from the gateway
        // end-to-end tests that share the process-wide map in parallel.
        set_max_output_tokens_map(HashMap::from([
            ("mot-fixture-sol".to_owned(), 8_192),
            ("mot-fixture-flash".to_owned(), 4_096),
        ]));
        // Case-insensitive match against the catalog id.
        let mut body = serde_json::json!({"model":"MOT-Fixture-Sol","input":[]});
        assert!(inject_max_output_tokens("/v1/responses", &mut body));
        assert_eq!(body["max_output_tokens"], serde_json::json!(8_192));

        let mut chat = serde_json::json!({"model":"mot-fixture-flash","messages":[]});
        assert!(inject_max_output_tokens("/v1/chat/completions", &mut chat));
        assert_eq!(chat["max_tokens"], serde_json::json!(4_096));

        // Unmapped models still receive the remaining compact budget,
        // capped at the recommended output floor.
        let mut unmapped = serde_json::json!({"model":"mot-fixture-unmapped"});
        assert!(inject_max_output_tokens("/v1/responses", &mut unmapped));
        let unmapped_limit = unmapped["max_output_tokens"].as_i64().unwrap();
        assert!(unmapped_limit > 0);
        assert!(unmapped_limit <= detect_context_defaults("mot-fixture-unmapped").window);

        set_max_output_tokens_map(HashMap::new());
        assert!(inject_max_output_tokens("/v1/responses", &mut body));
        let recomputed = body["max_output_tokens"].as_i64().unwrap();
        assert!(recomputed > 8_192);
        assert!(recomputed <= detect_context_defaults("MOT-Fixture-Sol").window);
    }

    #[test]
    fn grok_raises_codex_five_percent_output_cap() {
        let _guard = OUTPUT_BUDGET_TEST.lock().unwrap();
        set_max_output_tokens_map(HashMap::new());
        set_context_budget_map(HashMap::from([(
            "grok-4.6".to_owned(),
            ModelContextBudget {
                window: 500_000,
                compact_limit: 475_000,
            },
        )]));
        let mut grok = serde_json::json!({
            "model": "grok-4.6",
            "max_output_tokens": 25_000,
            "input": []
        });
        let expected = remaining_compact_budget("grok-4.6", &grok);
        assert!(expected > 25_000);
        assert!(inject_max_output_tokens("/v1/responses", &mut grok));
        assert_eq!(grok["max_output_tokens"], serde_json::json!(expected));

        let mut already_at_remaining = serde_json::json!({
            "model": "grok-4.6",
            "max_output_tokens": expected,
            "input": []
        });
        assert!(!inject_max_output_tokens("/v1/responses", &mut already_at_remaining));
        assert_eq!(already_at_remaining["max_output_tokens"], serde_json::json!(expected));

        // A huge transcript used to inject 1 token because remaining compact
        // budget underflowed. Grok must still receive the recommended floor.
        let huge = "x".repeat(4 * 600_000);
        let mut packed = serde_json::json!({
            "model": "grok-4.6",
            "max_output_tokens": 25_000,
            "input": [{"content": huge}]
        });
        assert!(inject_max_output_tokens("/v1/responses", &mut packed));
        let packed_limit = packed["max_output_tokens"].as_i64().unwrap();
        assert!(packed_limit >= 128_000);
        assert!(packed_limit >= 25_000);

        let screenshot = format!("data:image/png;base64,{}", "A".repeat(3_000_000));
        assert!(estimate_text_tokens(&screenshot) <= 4_096);
        let chinese = "用".repeat(300);
        assert!(estimate_text_tokens(&chinese) > 0);

        set_max_output_tokens_map(HashMap::from([("grok-4.6".to_owned(), 8_192)]));
        let mut user_limit = serde_json::json!({"model": "grok-4.6", "max_output_tokens": 25_000});
        assert!(inject_max_output_tokens("/v1/responses", &mut user_limit));
        assert_eq!(user_limit["max_output_tokens"], serde_json::json!(8_192));
        set_max_output_tokens_map(HashMap::new());
        set_context_budget_map(HashMap::new());
    }

    #[test]
    fn gemini_raises_codex_five_percent_output_cap() {
        let _guard = OUTPUT_BUDGET_TEST.lock().unwrap();
        set_max_output_tokens_map(HashMap::new());
        set_context_budget_map(HashMap::new());
        let mut gemini = serde_json::json!({
            "model": "gemini-3.7-flash",
            "max_output_tokens": 52_428,
            "input": []
        });
        assert!(inject_max_output_tokens("/v1/responses", &mut gemini));
        assert_eq!(gemini["max_output_tokens"], serde_json::json!(65_536));

        let mut already_at_cap = serde_json::json!({
            "model": "gemini-3.1-pro",
            "max_output_tokens": 65_536
        });
        assert!(!inject_max_output_tokens("/v1/responses", &mut already_at_cap));
        assert_eq!(already_at_cap["max_output_tokens"], serde_json::json!(65_536));

        set_max_output_tokens_map(HashMap::from([("gemini-3.7-flash".to_owned(), 8_192)]));
        let mut user_limit = serde_json::json!({
            "model": "gemini-3.7-flash",
            "max_output_tokens": 52_428
        });
        assert!(inject_max_output_tokens("/v1/responses", &mut user_limit));
        assert_eq!(user_limit["max_output_tokens"], serde_json::json!(8_192));
        set_max_output_tokens_map(HashMap::new());
    }

    #[test]
    fn remaining_compact_budget_caps_unconfigured_output() {
        let _guard = OUTPUT_BUDGET_TEST.lock().unwrap();
        set_max_output_tokens_map(HashMap::new());
        set_context_budget_map(HashMap::from([(
            "gemini-3.7-flash".to_owned(),
            ModelContextBudget {
                window: 1_048_576,
                compact_limit: 996_147,
            },
        )]));
        let used = "x".repeat(4 * 400_000);
        let mut gemini = serde_json::json!({
            "model": "gemini-3.7-flash",
            "max_output_tokens": 52_428,
            "input": [{"content": used}]
        });
        assert!(inject_max_output_tokens("/v1/responses", &mut gemini));
        assert_eq!(gemini["max_output_tokens"], serde_json::json!(65_536));

        let used_near_compact = "x".repeat(4 * 950_000);
        let mut near = serde_json::json!({
            "model": "gemini-3.7-flash",
            "max_output_tokens": 52_428,
            "input": [{"content": used_near_compact}]
        });
        // Near the compact point the leftover input budget is ~46k, but the
        // 5% output reserve (and Codex's 52_428) must not be shrunk.
        inject_max_output_tokens("/v1/responses", &mut near);
        let near_limit = near["max_output_tokens"].as_i64().unwrap();
        assert!(near_limit >= 52_428);
        assert!(near_limit <= 65_536);
        set_context_budget_map(HashMap::new());
    }
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn stop_waits_until_the_gateway_port_can_be_rebound() {
        static GATEWAY_TEST: Mutex<()> = Mutex::new(());
        let _guard = GATEWAY_TEST.lock().unwrap();
        stop_responses_gateway().unwrap();
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_port = reserved.local_addr().unwrap().port();
        assert!(gateway_port > 2);
        drop(reserved);
        let host = format!("http://127.0.0.1:{}", gateway_port - 2);
        ensure_responses_gateway(&host, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
        stop_responses_gateway().unwrap();
        TcpListener::bind((Ipv4Addr::LOCALHOST, gateway_port))
            .expect("gateway stop returned before releasing its listener");
    }
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn gateway_url_is_offset_from_sub2api() {
        assert_eq!(
            responses_gateway_url("http://127.0.0.1:18080").unwrap(),
            "http://127.0.0.1:18082"
        );
    }

    #[test]
    fn browser_navigation_does_not_forward_login_redirects() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_addr = gateway.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        thread::spawn(move || {
            let (mut stream, _) = gateway.accept().unwrap();
            handle_client(&mut stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
        });

        let mut client = TcpStream::connect(gateway_addr).unwrap();
        client
            .write_all(b"GET /v1 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("200 OK"));
        assert!(response.contains("Responses gateway is running"));
        assert!(upstream.set_nonblocking(true).is_ok());
        assert!(upstream.accept().is_err());
    }

    #[test]
    fn gateway_sanitizes_grok_responses_before_upstream() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let received = std::sync::Arc::new(Mutex::new(None::<Vec<u8>>));
        let received_clone = received.clone();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            let (headers, leftover) = read_headers(&mut socket).unwrap();
            let header_text = String::from_utf8_lossy(&headers);
            let length = parse_headers(&header_text)
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = leftover;
            read_exact_more(&mut socket, &mut body, length).unwrap();
            body.truncate(length);
            *received_clone.lock().unwrap() = Some(body);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });

        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_addr = gateway.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        thread::spawn(move || {
            let (mut stream, _) = gateway.accept().unwrap();
            handle_client(&mut stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
        });

        let mut client = TcpStream::connect(gateway_addr).unwrap();
        let body = br#"{"model":"grok-4.6","include":["reasoning.encrypted_content"],"input":[{"type":"function_call_output","call_id":"c1","encrypted_content":"plain tool output"}]}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        client.write_all(body).unwrap();
        let mut response = String::new();
        client.take(1024).read_to_string(&mut response).ok();
        assert!(response.contains("200"));
        let forwarded = loop {
            if let Some(value) = received.lock().unwrap().clone() {
                break value;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let json: Value = serde_json::from_slice(&forwarded).unwrap();
        assert!(json.get("include").is_none());
        assert_eq!(json["input"][0]["output"], "plain tool output");
        assert!(json["input"][0].get("encrypted_content").is_none());
    }

    #[test]
    fn chatgpt_request_is_forwarded_without_rewriting() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let received = std::sync::Arc::new(Mutex::new(None::<Vec<u8>>));
        let received_clone = received.clone();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            let (headers, leftover) = read_headers(&mut socket).unwrap();
            let header_text = String::from_utf8_lossy(&headers);
            let length = parse_headers(&header_text)
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = leftover;
            read_exact_more(&mut socket, &mut body, length).unwrap();
            body.truncate(length);
            *received_clone.lock().unwrap() = Some(body);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\ndata: {\"type\":\"response.completed\"}\n\n")
                .unwrap();
        });

        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_addr = gateway.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        thread::spawn(move || {
            let (mut stream, _) = gateway.accept().unwrap();
            handle_client(&mut stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
        });

        let mut client = TcpStream::connect(gateway_addr).unwrap();
        let body = br#"{"model":"gpt-5.6-sol","include":["reasoning.encrypted_content"],"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        client.write_all(body).unwrap();
        let mut response = Vec::new();
        client.take(2048).read_to_end(&mut response).ok();
        let forwarded = loop {
            if let Some(value) = received.lock().unwrap().clone() {
                break value;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let forwarded_json: serde_json::Value = serde_json::from_slice(&forwarded).unwrap();
        let original_json: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(forwarded_json["model"], original_json["model"]);
        assert_eq!(forwarded_json["include"], original_json["include"]);
        assert_eq!(forwarded_json["input"], original_json["input"]);
        let injected = forwarded_json["max_output_tokens"].as_i64().unwrap();
        let expected = remaining_compact_budget("gpt-5.6-sol", &original_json);
        assert_eq!(injected, expected);
        assert!(injected > 0);
        let text = String::from_utf8_lossy(&response);
        assert!(text.to_ascii_lowercase().contains("connection: close"));
        assert!(text.contains("response.output_text.delta"));
        assert!(text.contains("response.completed"));
        assert!(!text.contains("response.failed"));
    }

    #[test]
    fn chatgpt_compact_request_drops_generation_controls() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let received = std::sync::Arc::new(Mutex::new(None::<Vec<u8>>));
        let received_clone = received.clone();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            let (headers, leftover) = read_headers(&mut socket).unwrap();
            let header_text = String::from_utf8_lossy(&headers);
            let length = parse_headers(&header_text)
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = leftover;
            read_exact_more(&mut socket, &mut body, length).unwrap();
            body.truncate(length);
            *received_clone.lock().unwrap() = Some(body);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"id\":\"cmp_1\",\"output\":[{\"type\":\"compaction\",\"encrypted_content\":\"blob\"}]}")
                .unwrap();
        });

        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_addr = gateway.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        thread::spawn(move || {
            let (mut stream, _) = gateway.accept().unwrap();
            handle_client(&mut stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
        });

        let mut client = TcpStream::connect(gateway_addr).unwrap();
        let body = br#"{"model":"gpt-5.6-sol","stream":true,"tools":[{"type":"function","name":"exec_command"}],"parallel_tool_calls":true,"include":["reasoning.encrypted_content"],"max_output_tokens":128000,"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"compact"}]}]}"#;
        let request = format!(
            "POST /v1/responses/compact HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        client.write_all(body).unwrap();
        let mut response = Vec::new();
        client.take(2048).read_to_end(&mut response).ok();
        let forwarded = loop {
            if let Some(value) = received.lock().unwrap().clone() {
                break value;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let forwarded_json: serde_json::Value = serde_json::from_slice(&forwarded).unwrap();
        assert_eq!(forwarded_json["model"], "gpt-5.6-sol");
        assert!(forwarded_json.get("stream").is_none());
        assert!(forwarded_json.get("tools").is_none());
        assert!(forwarded_json.get("parallel_tool_calls").is_none());
        assert!(forwarded_json.get("include").is_none());
        assert!(forwarded_json.get("max_output_tokens").is_none());
        assert_eq!(forwarded_json["input"][0]["type"], "message");
    }

    #[test]
    fn sse_events_split_on_crlf_boundaries() {
        let mut carry = "data: one\r\n\r\ndata: two\n\npartial".to_owned();
        assert_eq!(take_sse_event(&mut carry).as_deref(), Some("data: one\r\n\r\n"));
        assert_eq!(take_sse_event(&mut carry).as_deref(), Some("data: two\n\n"));
        assert_eq!(take_sse_event(&mut carry), None);
        assert_eq!(carry, "partial");
    }

    #[test]
    fn sse_terminal_events_and_chunk_extensions_are_recognized() {
        assert!(sse_event_is_terminal(
            "data: {\"type\":\"response.completed\"}\n\n"
        ));
        assert!(sse_event_is_terminal("data: [DONE]\n\n"));
        assert!(!sse_event_is_terminal(
            "data: {\"type\":\"response.output_text.delta\"}\n\n"
        ));
        assert_eq!(parse_chunk_size("1a;foo=bar").unwrap(), 26);
        assert!(parse_chunk_size("not-hex").is_err());
        assert!(sse_event_is_retryable_interrupt(
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"CR-UP-0014\",\"message\":\"upstream stream ended before a terminal event\"}}}\n\n"
        ));
        assert!(sse_event_is_retryable_interrupt(
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"upstream_stream_interrupted\",\"message\":\"Upstream stream ended before completion.\"}}}\n\n"
        ));
        assert!(!sse_event_is_retryable_interrupt(
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"insufficient_quota\",\"message\":\"quota\"}}}\n\n"
        ));
        assert!(!sse_event_is_retryable_interrupt(
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"CR-UP-0014\",\"message\":\"upstream failed\"}}}\n\n"
        ));
    }

    #[test]
    fn interrupted_text_stream_reports_retryable_with_the_delivered_prefix() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"half\"}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            // Plain-text partial output is retryable: the caller reopens the
            // upstream instead of ending the session.
            assert_eq!(
                outcome,
                SseForward::RetryableWithPrefix(StreamResume {
                    prefix: "half".to_owned(),
                    response_id: None,
                    item_id: None,
                })
            );
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.output_text.delta"));
        assert!(!output.contains("response.failed"));
    }

    #[test]
    fn idle_upstream_sends_keepalive_and_the_session_resumes() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"thinking\"}\n\n")
                .unwrap();
            // Simulate a long provider-side reasoning pause: stay silent past
            // several poll intervals, then finish the task normally.
            thread::sleep(Duration::from_millis(600));
            socket
                .write_all(b"data: {\"type\":\"response.completed\"}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        upstream_stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.output_text.delta"));
        // The pause produced keep-alive comments instead of a failed event...
        assert!(output.contains(": codex-router keep-alive"));
        // ...and the upstream completed the task normally afterwards.
        assert!(output.contains("response.completed"));
        assert!(!output.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn corrupt_chunk_frame_reports_retryable_instead_of_silence() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"2d\r\ndata: {\"type\":\"response.output_text.delta\"}\n\n\r\nzz\r\n???\r\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        upstream_stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = decode_chunked_to_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            // A corrupt frame is handled like an upstream drop: the delivered
            // plain-text prefix can be reconciled on a retry.
            assert!(matches!(outcome, SseForward::RetryableWithPrefix(_)));
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.output_text.delta"));
        assert!(!output.contains("response.failed"));
    }

    #[test]
    fn rate_limited_request_retries_until_a_healthy_account_answers() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        thread::spawn(move || {
            for index in 0..3 {
                let (mut socket, _) = upstream.accept().unwrap();
                let (headers, leftover) = read_headers(&mut socket).unwrap();
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = leftover;
                read_exact_more(&mut socket, &mut body, length).unwrap();
                attempts_clone.fetch_add(1, Ordering::Relaxed);
                let response = if index < 2 {
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .as_slice()
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                        .as_slice()
                };
                socket.write_all(response).unwrap();
            }
        });

        let mut retry_budget = RequestRetryBudget::new(3);
        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            &mut retry_budget,
            None,
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn upstream_401_retries_then_is_shielded_from_desktop() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        thread::spawn(move || {
            for index in 0..3 {
                let (mut socket, _) = upstream.accept().unwrap();
                let (headers, leftover) = read_headers(&mut socket).unwrap();
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = leftover;
                read_exact_more(&mut socket, &mut body, length).unwrap();
                attempts_clone.fetch_add(1, Ordering::Relaxed);
                let response = if index < 2 {
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"error\":\"unauthenticated\"}"
                        .as_slice()
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                        .as_slice()
                };
                socket.write_all(response).unwrap();
            }
        });

        let mut retry_budget = RequestRetryBudget::new(3);
        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            &mut retry_budget,
            None,
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(attempts.load(Ordering::Relaxed), 3);

        let (shielded_status, shielded_body) =
            shield_desktop_auth_failure(401, br#"{"error":"unauthenticated"}"#);
        assert_eq!(shielded_status, 503);
        assert!(!String::from_utf8_lossy(&shielded_body).contains("unauthenticated"));
    }

    #[test]
    fn exhausted_account_pool_503_retries_like_a_literal_429() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        thread::spawn(move || {
            for index in 0..2 {
                let (mut socket, _) = upstream.accept().unwrap();
                let (headers, leftover) = read_headers(&mut socket).unwrap();
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = leftover;
                read_exact_more(&mut socket, &mut body, length).unwrap();
                attempts_clone.fetch_add(1, Ordering::Relaxed);
                if index == 0 {
                                    socket
                                        .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 39\r\nConnection: close\r\n\r\n{\"error\":\"no available accounts (429)\"}")
                                        .unwrap();
                } else {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                        .unwrap();
                }
            }
        });

        let mut retry_budget = RequestRetryBudget::new(3);
        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            &mut retry_budget,
            None,
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn rate_limit_retries_respect_the_configured_maximum() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        thread::spawn(move || {
            while let Ok((mut socket, _)) = upstream.accept() {
                let (headers, leftover) = read_headers(&mut socket).unwrap();
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = leftover;
                read_exact_more(&mut socket, &mut body, length).unwrap();
                attempts_clone.fetch_add(1, Ordering::Relaxed);
                socket
                    .write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
        });

        let mut retry_budget = RequestRetryBudget::new(2);
        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            &mut retry_budget,
            None,
        )
        .unwrap();
        // 1 initial attempt + 2 retries, then the 429 is passed through so the
        // caller can surface a normal rate-limit error to the user.
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(response.status, 429);
    }

    #[test]
    fn one_request_cannot_wait_through_an_unbounded_retry_ladder() {
        let (output, attempts) = run_client_against_mock(
            |_attempt, socket| {
                socket
                    .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"error\":\"rate limit\"}")
                    .unwrap();
            },
            MAX_RATE_LIMIT_RETRIES,
        );
        assert!(output.starts_with("HTTP/1.1 429"));
        assert!(
            attempts <= 4,
            "one Codex turn must stop before the configured 32-retry ladder reaches hour-long waits"
        );
    }

    #[test]
    fn rate_limit_backoff_is_tiered_and_long_hints_are_clamped() {
        // A Retry-After beyond the ceiling is clamped instead of skipping the
        // retry, so the conversation keeps waiting rather than ending.
        assert_eq!(
            rate_limit_retry_delay(
                Some(&HashMap::from([("retry-after".to_owned(), "8000".to_owned())])),
                0,
            ),
            MAX_RATE_LIMIT_DELAY
        );
        // A hint shorter than the current step never shortens the wait.
        assert_eq!(
            rate_limit_retry_delay(
                Some(&HashMap::from([("retry-after".to_owned(), "3".to_owned())])),
                0,
            ),
            Duration::from_secs(5)
        );
        // A hint longer than the current step lengthens the wait.
        assert_eq!(
            rate_limit_retry_delay(
                Some(&HashMap::from([("retry-after".to_owned(), "30".to_owned())])),
                0,
            ),
            Duration::from_secs(30)
        );
        // Stepped ladder: 5s for the first retry, then x5 per extra retry.
        assert_eq!(rate_limit_retry_delay(None, 0), Duration::from_secs(5));
        assert_eq!(rate_limit_retry_delay(None, 1), Duration::from_secs(25));
        assert_eq!(rate_limit_retry_delay(None, 2), Duration::from_secs(125));
        assert_eq!(rate_limit_retry_delay(None, 3), Duration::from_secs(625));
        assert_eq!(rate_limit_retry_delay(None, 4), Duration::from_secs(3125));
        // Later steps saturate and clamp at the per-step ceiling.
        assert_eq!(rate_limit_retry_delay(None, 5), MAX_RATE_LIMIT_DELAY);
        assert_eq!(rate_limit_retry_delay(None, 32), MAX_RATE_LIMIT_DELAY);
    }

    /// Spin a mock Sub2API that answers per attempt, then run one client
    /// request through `handle_client` and return what the client received.
    fn run_client_against_mock<F>(answer: F, max_retries: u32) -> (String, usize)
    where
        F: Fn(usize, &mut TcpStream) + Send + 'static,
    {
        run_model_client_against_mock("grok-t", answer, max_retries)
    }

    fn run_model_client_against_mock<F>(
        model: &str,
        answer: F,
        max_retries: u32,
    ) -> (String, usize)
    where
        F: Fn(usize, &mut TcpStream) + Send + 'static,
    {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        thread::spawn(move || {
            while let Ok((mut socket, _)) = upstream.accept() {
                let (headers, leftover) = match read_headers(&mut socket) {
                    Ok(parts) => parts,
                    Err(_) => continue,
                };
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = leftover;
                if read_exact_more(&mut socket, &mut body, length).is_err() {
                    continue;
                }
                let attempt = attempts_clone.fetch_add(1, Ordering::Relaxed);
                answer(attempt, &mut socket);
            }
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        let worker = thread::spawn(move || {
            let (mut stream, _) = client_listener.accept().unwrap();
            handle_client(&mut stream, &upstream_url, max_retries).unwrap();
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let body = format!(r#"{{"model":"{model}"}}"#);
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        (output, attempts.load(Ordering::Relaxed))
    }

    #[test]
    fn chatgpt_sse_disconnect_uses_valid_terminal_semantics() {
        let (output, attempts) = run_model_client_against_mock(
            "gpt-5.6-sol",
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\ndata: {\"type\":\"response.completed\"}\n\n")
                    .unwrap();
            },
            1,
        );
        assert_eq!(attempts, 2);
        assert_eq!(output.matches("HTTP/1.1 200").count(), 1);
        assert!(output.contains("response.completed"));
        assert!(!output.contains("upstream_stream_interrupted"));

        let (failed, attempts) = run_model_client_against_mock(
            "gpt-5.6-sol",
            |_attempt, socket| {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                    .unwrap();
            },
            0,
        );
        assert_eq!(attempts, 1);
        assert!(failed.contains("response.failed"));
        assert!(failed.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn retry_error_after_sse_prelude_closes_with_one_terminal_event() {
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"error\":\"rate limit\"}")
                    .unwrap();
            },
            1,
        );
        assert_eq!(attempts, 2);
        assert_eq!(output.matches("HTTP/1.1").count(), 1);
        assert_eq!(output.matches("response.failed").count(), 2);
        assert!(output.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn client_cancellation_errors_are_not_gateway_failures() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let error = anyhow::Error::new(std::io::Error::new(kind, "fixture"));
            assert!(client_connection_ended(&error));
        }
        let aborted = std::io::Error::from_raw_os_error(10053);
        assert!(client_connection_ended(&anyhow::Error::new(aborted)));
    }

    #[test]
    fn cancelled_client_stops_upstream_retry_immediately() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        upstream.set_nonblocking(true).unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let upstream_worker = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                let (mut socket, _) = match upstream.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("mock upstream accept failed: {error}"),
                };
                let (headers, leftover) = read_headers(&mut socket).unwrap();
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = leftover;
                read_exact_more(&mut socket, &mut body, length).unwrap();
                attempts_clone.fetch_add(1, Ordering::Relaxed);
                socket
                    .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
        });

        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_addr = gateway.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = std::sync::mpsc::sync_channel(0);
        let gateway_worker = thread::spawn(move || {
            let (mut stream, _) = gateway.accept().unwrap();
            accepted_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            handle_client(&mut stream, &upstream_url, 5)
        });

        let mut client = TcpStream::connect(gateway_addr).unwrap();
        accepted_rx.recv().unwrap();
        let body = br#"{"model":"gpt-5.6-sol"}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        client.write_all(body).unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        drop(client);
        continue_tx.send(()).unwrap();

        let _ = gateway_worker.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        upstream_worker.join().unwrap();
        assert!(
            attempts.load(Ordering::Relaxed) <= 1,
            "a cancelled Codex request must not consume the remaining upstream retry budget"
        );
    }

    #[test]
    fn would_block_is_retried_instead_of_treated_as_client_cancel() {
        for kind in [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut] {
            let error = anyhow::Error::new(std::io::Error::new(kind, "fixture"));
            assert!(!client_connection_ended(&error));
        }
        let would_block = std::io::Error::from_raw_os_error(10035);
        assert!(io_would_block(&would_block));
        assert!(!client_connection_ended(&anyhow::Error::new(would_block)));
    }

    #[test]
    fn force_connection_close_headers_uses_crlf_and_replaces_keep_alive() {
        let rewritten = force_connection_close_headers(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
        );
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("connection: close\r\n"));
        assert!(!text.to_ascii_lowercase().contains("keep-alive"));
        assert!(text.ends_with("\r\n\r\n"));
        assert!(!text.contains("connection: close\nconnection"));
    }

    #[test]
    fn handle_client_survives_nonblocking_listener_accept() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            let (headers, leftover) = read_headers(&mut socket).unwrap();
            let length = parse_headers(&String::from_utf8_lossy(&headers))
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = leftover;
            read_exact_more(&mut socket, &mut body, length).unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\ndata: {\"type\":\"response.completed\"}\n\n")
                .unwrap();
        });

        let gateway = TcpListener::bind("127.0.0.1:0").unwrap();
        gateway.set_nonblocking(true).unwrap();
        let gateway_addr = gateway.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        let worker = thread::spawn(move || {
            let mut stream = loop {
                match gateway.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            handle_client(&mut stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
            close_client_gracefully(&mut stream);
        });

        let mut client = TcpStream::connect(gateway_addr).unwrap();
        let body = br#"{"model":"gpt-5.6-sol","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#;
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        client.write_all(body).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        assert!(response.to_ascii_lowercase().contains("connection: close"));
        assert!(response.contains("response.output_text.delta"));
        assert!(response.contains("response.completed"));
        assert!(!response.contains("response.failed"));
    }

    #[test]
    fn pre_content_sse_disconnect_retries_transparently() {
        // First attempt sends SSE headers then dies without a single event;
        // the client has only seen the prelude, so a retry is seamless.
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\ndata: {\"type\":\"response.completed\"}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("response.completed"));
        // The answer is delivered exactly once: no duplicated prefix, no
        // failure event, and the HTTP prelude is written only once.
        assert_eq!(output.matches("response.output_text.delta").count(), 1);
        assert_eq!(output.matches("HTTP/1.1 200").count(), 1);
        assert!(!output.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn pre_content_sse_disconnect_emits_failed_event_after_budget_exhaustion() {
        // Every attempt dies before the first event; after the retry budget
        // the client still gets a clean terminal event instead of an EOF.
        let (output, attempts) = run_client_against_mock(
            |_attempt, socket| {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                    .unwrap();
            },
            2,
        );
        assert_eq!(attempts, 3);
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn truncated_json_body_retries_the_whole_request() {
        // First attempt promises 100 bytes but dies mid-body; nothing reached
        // the client, so the gateway retries and serves the full JSON.
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"partial\":true,")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        assert!(output.contains("HTTP/1.1 200"));
        assert!(output.contains("\"ok\":true"));
        assert!(!output.contains("partial"));
    }

    #[test]
    fn truncated_chunked_json_retries_and_decodes_before_forwarding() {
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nb\r\n{\"partial")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nb\r\n{\"ok\":true}\r\n0\r\n\r\n")
                    .unwrap();
            },
            1,
        );
        assert_eq!(attempts, 2);
        assert!(output.contains("{\"ok\":true}"));
        assert!(!output.contains("partial"));
        assert!(!output.to_ascii_lowercase().contains("transfer-encoding"));
        assert!(output.to_ascii_lowercase().contains("content-length: 11"));
    }

    #[test]
    fn chunked_client_request_body_is_decoded_before_json_rewriting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (_headers, leftover) = read_headers(&mut socket).unwrap();
            read_chunked_body(&mut socket, leftover).unwrap()
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(
                b"POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n17\r\n{\"model\":\"gpt-5.6-sol\"}\r\n0\r\n\r\n",
            )
            .unwrap();
        let body = worker.join().unwrap();
        assert_eq!(body, br#"{"model":"gpt-5.6-sol"}"#);
    }

    #[test]
    fn gzip_request_body_is_decoded_before_json_rewriting() {
        let body = br#"{"model":"gpt-5.6-sol","input":[]}"#;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body).unwrap();
        let encoded = encoder.finish().unwrap();
        let decoded = decode_request_content_encoding("gzip", &encoded).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn zstd_request_body_is_decoded_before_json_rewriting() {
        let body = br#"{"model":"gpt-5.6-sol","input":[]}"#;
        let encoded = zstd::stream::encode_all(body.as_slice(), 1).unwrap();
        let decoded = decode_request_content_encoding("zstd", &encoded).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn truncated_error_body_retries_the_whole_request() {
        // Error responses are buffered before forwarding; a mid-body drop
        // retries instead of hanging up on the Codex client.
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"err")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: 17\r\nConnection: close\r\n\r\nupstream exploded")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        assert!(output.contains("upstream exploded"));
        assert!(!output.contains("{\"err"));
    }

    #[test]
    fn empty_sse_eof_reports_retryable_before_any_event() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (_socket, _) = upstream.accept().unwrap();
            // Immediate EOF without a single byte.
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            assert!(matches!(outcome, SseForward::RetryableBeforeFirstEvent));
        });
        let client = TcpStream::connect(client_addr).unwrap();
        worker.join().unwrap();
        drop(client);
    }

    #[test]
    fn mid_stream_text_retry_reconciles_and_completes() {
        // Attempt 1 dies after delivering "hello "; attempt 2 regenerates the
        // same text and continues. The client must see the answer exactly
        // once, with the original stream ids, and no failure event.
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\ndata: {\"type\":\"response.in_progress\"}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i1\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.content_part.added\",\"item_id\":\"i1\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \",\"item_id\":\"i1\"}\n\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r2\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i2\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.content_part.added\",\"item_id\":\"i2\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \",\"item_id\":\"i2\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"world\",\"item_id\":\"i2\"}\n\ndata: {\"type\":\"response.output_text.done\",\"text\":\"hello world\",\"item_id\":\"i2\"}\n\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"i2\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\"}}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        // "hello " was delivered before the drop; the retry delivered only the
        // "world" suffix, so the assembled answer appears exactly once.
        assert_eq!(output.matches("\"delta\":\"hello ").count(), 1);
        assert_eq!(output.matches("\"delta\":\"world").count(), 1);
        assert_eq!(output.matches("response.completed").count(), 1);
        assert!(!output.contains("response.failed"));
        assert!(!output.contains("upstream_stream_interrupted"));
        // Retry-stream ids are rewritten to the ids the client already saw.
        assert!(!output.contains("r2"));
        assert!(!output.contains("i2"));
        assert_eq!(output.matches("HTTP/1.1 200").count(), 1);
    }

    #[test]
    fn mid_stream_retry_aborts_cleanly_when_the_text_diverges() {
        // Attempt 2 regenerates different text; nothing extra is forwarded and
        // the session closes with a clean failure event.
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i1\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \",\"item_id\":\"i1\"}\n\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r3\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i3\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"goodbye\",\"item_id\":\"i3\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r3\"}}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        assert_eq!(output.matches("hello ").count(), 1);
        // The divergent retry is fully suppressed; no duplicated or garbled
        // content ever reaches the client.
        assert!(!output.contains("goodbye"));
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn structural_only_eof_retries_with_an_empty_prefix() {
        // Only response.created/in_progress reached the client before the
        // drop; a retry can resume with an empty text prefix.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r9\"}}\n\ndata: {\"type\":\"response.in_progress\"}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            assert_eq!(
                outcome,
                SseForward::RetryableWithPrefix(StreamResume {
                    prefix: String::new(),
                    response_id: Some("r9".to_owned()),
                    item_id: None,
                })
            );
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.created"));
        assert!(!output.contains("response.failed"));
    }

    #[test]
    fn tool_call_partial_stream_is_retryable_after_disconnect() {
        // A sudden drop after a tool-call item must still retry; otherwise
        // Codex shows "stream disconnected before completion" with no reconnect.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"f1\",\"type\":\"function_call\"}}\n\ndata: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{}\",\"item_id\":\"f1\"}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            assert!(matches!(outcome, SseForward::RetryableWithPrefix(_)));
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("function_call"));
        assert!(!output.contains("response.failed"));
        assert!(!output.contains("upstream_stream_interrupted"));
    }

    #[test]
    fn host_synthetic_failed_after_text_is_retryable() {
        // Host wraps CLIProxyAPI EOF as response.failed / CR-UP-0014. That
        // must look like a dropped stream so mid-stream text can resume.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \",\"item_id\":\"i1\"}\n\nevent: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"CR-UP-0014\",\"message\":\"upstream stream ended before a terminal event\"}},\"request_id\":\"req-1\"}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            assert_eq!(
                outcome,
                SseForward::RetryableWithPrefix(StreamResume {
                    prefix: "hello ".to_owned(),
                    response_id: Some("r1".to_owned()),
                    item_id: Some("i1".to_owned()),
                })
            );
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("hello "));
        assert!(!output.contains("response.failed"));
        assert!(!output.contains("CR-UP-0014"));
        assert!(!output.contains("before a terminal event"));
    }

    #[test]
    fn host_synthetic_failed_before_content_is_retryable() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"CR-UP-0014\",\"message\":\"upstream stream failed before a terminal event\"}}}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            assert!(matches!(outcome, SseForward::RetryableBeforeFirstEvent));
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(!output.contains("response.failed"));
        assert!(!output.contains("CR-UP-0014"));
    }

    #[test]
    fn genuine_upstream_failed_is_still_forwarded() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .write_all(b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"insufficient_quota\",\"message\":\"quota exceeded\"}}}\n\n")
                .unwrap();
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let mut upstream_stream = TcpStream::connect(upstream_addr).unwrap();
        let worker = thread::spawn(move || {
            let (mut client, _) = client_listener.accept().unwrap();
            let mut sink = SseSink::new(&mut client, None);
            let outcome = forward_plain_sse(&mut sink, &mut upstream_stream, Vec::new()).unwrap();
            assert_eq!(outcome, SseForward::Done);
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.failed"));
        assert!(output.contains("insufficient_quota"));
        assert!(output.contains("quota exceeded"));
    }

    #[test]
    fn host_synthetic_failed_mid_stream_retries_and_reconciles() {
        let (output, attempts) = run_client_against_mock(
            |attempt, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i1\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \",\"item_id\":\"i1\"}\n\nevent: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"CR-UP-0014\",\"message\":\"upstream stream ended before a terminal event\"}}}\n\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r2\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i2\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \",\"item_id\":\"i2\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"world\",\"item_id\":\"i2\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\"}}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        assert_eq!(output.matches("\"delta\":\"hello ").count(), 1);
        assert_eq!(output.matches("\"delta\":\"world").count(), 1);
        assert_eq!(output.matches("response.completed").count(), 1);
        assert!(!output.contains("response.failed"));
        assert!(!output.contains("CR-UP-0014"));
        assert!(!output.contains("before a terminal event"));
        assert!(!output.contains("r2"));
        assert!(!output.contains("i2"));
        assert_eq!(output.matches("HTTP/1.1 200").count(), 1);
    }

    fn run_agent_body_against_mock<F>(
        body: &str,
        answer: F,
        max_retries: u32,
    ) -> (String, usize, Vec<Vec<u8>>)
    where
        F: Fn(usize, &[u8], &mut TcpStream) + Send + 'static,
    {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        let received = std::sync::Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let received_clone = received.clone();
        thread::spawn(move || {
            while let Ok((mut socket, _)) = upstream.accept() {
                let (headers, leftover) = match read_headers(&mut socket) {
                    Ok(parts) => parts,
                    Err(_) => continue,
                };
                let length = parse_headers(&String::from_utf8_lossy(&headers))
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut request_body = leftover;
                if read_exact_more(&mut socket, &mut request_body, length).is_err() {
                    continue;
                }
                request_body.truncate(length);
                received_clone.lock().unwrap().push(request_body.clone());
                let attempt = attempts_clone.fetch_add(1, Ordering::Relaxed);
                answer(attempt, &request_body, &mut socket);
            }
        });
        let client_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream_addr.port());
        let worker = thread::spawn(move || {
            let (mut stream, _) = client_listener.accept().unwrap();
            handle_client(&mut stream, &upstream_url, max_retries).unwrap();
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        let captured = received.lock().unwrap().clone();
        (output, attempts.load(Ordering::Relaxed), captured)
    }

    fn grok_mid_agent_request() -> String {
        serde_json::json!({
            "model": "grok-4.6",
            "tools": [{"type":"function","name":"exec_command","parameters":{"type":"object","properties":{"cmd":{"type":"string"}}}}],
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"修登录循环"}]},
                {"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"function_call_output","call_id":"c1","output":"ok"}
            ]
        })
        .to_string()
    }

    #[test]
    fn grok_commentary_without_tools_is_continued_into_a_function_call() {
        let body = grok_mid_agent_request();
        let (output, attempts, requests) = run_agent_body_against_mock(
            &body,
            |attempt, _request, socket| {
                if attempt == 0 {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"i1\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"\\u6211\\u5148\\u5bf9\\u7167 0.2.15 \\u622a\\u56fe\",\"item_id\":\"i1\"}\n\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"i1\",\"type\":\"message\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\"}}\n\n")
                        .unwrap();
                    return;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r2\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"f1\",\"type\":\"function_call\",\"name\":\"exec_command\",\"call_id\":\"c2\",\"arguments\":\"{\\\"cmd\\\":\\\"Get-Content shot.png\\\"}\"}}\n\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"f1\",\"type\":\"function_call\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\"}}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 2);
        assert_eq!(requests.len(), 2);
        let follow_up: Value = serde_json::from_slice(&requests[1]).unwrap();
        let follow_text = follow_up.to_string();
        assert!(follow_text.contains("\\u81ea\\u52a8\\u7eed\\u8dd1") || follow_text.contains("自动续跑"));
        assert_eq!(output.matches("response.completed").count(), 1);
        assert!(!output.contains("\"id\":\"r2\""));
        assert!(output.contains("function_call"));
        assert!(output.contains("exec_command"));
        assert!(!output.contains("response.failed"));
        assert_eq!(output.matches("HTTP/1.1 200").count(), 1);
    }

    #[test]
    fn grok_finished_mid_agent_answer_is_not_continued() {
        let body = grok_mid_agent_request();
        let (output, attempts, requests) = run_agent_body_against_mock(
            &body,
            |_attempt, _request, socket| {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"\\u4efb\\u52a1\\u5df2\\u5b8c\\u6210\\u3002\\u767b\\u5f55\\u5faa\\u73af\\u5df2\\u4fee\\u597d\\u3002\"}\n\ndata: {\"type\":\"response.completed\"}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 1);
        assert_eq!(requests.len(), 1);
        assert_eq!(output.matches("response.completed").count(), 1);
        assert!(!output.contains("response.failed"));
    }

    #[test]
    fn grok_plain_chat_without_tools_is_not_continued() {
        let body = r#"{"model":"grok-4.6","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#;
        let (output, attempts, _) = run_agent_body_against_mock(
            body,
            |_attempt, _request, socket| {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\ndata: {\"type\":\"response.completed\"}\n\n")
                    .unwrap();
            },
            3,
        );
        assert_eq!(attempts, 1);
        assert_eq!(output.matches("response.completed").count(), 1);
    }

    #[test]
    fn grok_incomplete_continue_stops_after_two_nudges() {
        let body = grok_mid_agent_request();
        let (output, attempts, requests) = run_agent_body_against_mock(
            &body,
            |_attempt, _request, socket| {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"rx\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"\\u6211\\u5148\\u770b\\u4e00\\u4e0b\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"rx\"}}\n\n")
                    .unwrap();
            },
            3,
        );
        // Original + two continuation POSTs, then the held completed is flushed.
        assert_eq!(attempts, 3);
        assert_eq!(requests.len(), 3);
        assert_eq!(output.matches("response.completed").count(), 1);
        assert!(!output.contains("response.failed"));
    }
}
