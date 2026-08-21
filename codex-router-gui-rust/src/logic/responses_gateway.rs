//! Local compatibility gateway in front of Sub2API `/v1/responses`.
//!
//! Codex keeps talking to a loopback URL. This process rewrites mixed-model
//! Responses payloads, then forwards the request to Sub2API so a Grok 422 or
//! a poisoned encrypted function output cannot turn into an infinite compact
//! loop or a router-wide 502.

use super::responses_compat::{
    is_chat_completions_path, is_compact_path, is_exhausted_account_status, is_openai_family_model,
    is_responses_path,
    rewrite_poisoned_upstream_status, is_unsupported_image_error, rewrite_provider_json,
    rewrite_sse_text, sanitize_responses_request, sanitize_responses_request_aggressive,
    should_retry_after_upstream_error, sanitize_responses_request_without_images,
    synthetic_compact_response,
};
use anyhow::{bail, Context};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
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
/// Hard ceiling for a configured retry count so a typo cannot pin a worker
/// thread on a sleeping retry loop forever.
const MAX_RATE_LIMIT_RETRIES: u32 = 32;
/// Ceiling for one backoff step. A `Retry-After` hint above this is clamped
/// instead of skipping the retry, so the conversation keeps waiting.
const MAX_RATE_LIMIT_DELAY: Duration = Duration::from_secs(3600);
/// First retry wait in seconds. Every further retry multiplies the wait by
/// five: 5s, 25s, 125s, 625s, ... (each step clamped at the ceiling above).
const RATE_LIMIT_RETRY_BASE_DELAY_SECS: u64 = 5;
/// While streaming an agent (non-OpenAI-family) response, the upstream socket
/// is polled on this cadence so the gateway can keep the Codex session alive
/// through long provider-side reasoning pauses.
const STREAM_POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// Total upstream silence tolerated before an agent stream is declared dead.
/// Grok/Gemini deep-reasoning pauses legitimately exceed the old fixed 300s
/// socket timeout; 30 minutes of complete silence is a genuine hang.
const STREAM_MAX_SILENCE: Duration = Duration::from_secs(1800);

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

/// Inject the user-configured output token limit into the request body. The
/// user setting always wins over whatever the client sent; models left at
/// the default (0) keep the upstream behaviour of not sending the field.
fn inject_max_output_tokens(path: &str, body: &mut Value) -> bool {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return false;
    };
    let Some(limit) = max_output_tokens_for(model) else {
        return false;
    };
    let field = if is_chat_completions_path(path) {
        "max_tokens"
    } else {
        "max_output_tokens"
    };
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
                    if let Err(error) = handle_client(&mut client, &upstream, rate_limit_max_retries)
                    {
                        gateway_log("request.error", &format!("{error:#}"));
                    }
                    close_client_gracefully(client);
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
    client
        .set_read_timeout(Some(HEADER_TIMEOUT))
        .and_then(|_| client.set_write_timeout(Some(Duration::from_secs(300))))
        .ok();
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
    if let Some(length) = headers
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

    gateway_log(
        "request.start",
        &format!("{method} {path} bytes={}", body.len()),
    );
    let mut request_body = body;
    let mut openai_family = false;
    if method == "POST" && (is_responses_path(&path) || is_chat_completions_path(&path)) {
        if let Ok(mut json_body) = serde_json::from_slice::<Value>(&request_body) {
            openai_family = json_body
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(is_openai_family_model);
            if !openai_family {
                let stats = sanitize_responses_request(&path, &mut json_body);
                inject_max_output_tokens(&path, &mut json_body);
                if is_compact_path(&path) {
                    let output = json_body
                        .get("input")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let payload =
                        serde_json::to_vec(&synthetic_compact_response(&stats.model, &output))?;
                    send_status(client, 200, "application/json", &payload)?;
                    return Ok(());
                }
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
    let stage_max_retries = rate_limit_max_retries.min(MAX_RATE_LIMIT_RETRIES) as usize;
    let mut stage_attempt = 0_usize;
    let mut sse_prelude_sent = false;
    let mut stream_resume: Option<StreamResume> = None;
    let (status, response_headers_map, response_body) = loop {
        let response = open_upstream_with_rate_limit_retry(
            upstream,
            &method,
            &path,
            &headers,
            &request_body,
            rate_limit_max_retries,
        )?;
        let mut upstream_stream = response.stream;
        let response_headers = response.headers;
        let response_leftover = response.leftover;
        let status = response.status;
        let response_headers_map = response.header_map;
        if status < 400 || !is_responses_path(&path) {
            if status < 400 && is_responses_path(&path) && openai_family {
                write_headers_forced_close(client, &response_headers)?;
                client.write_all(&response_leftover)?;
                std::io::copy(&mut upstream_stream, client)?;
                return Ok(());
            }
            if status < 400 && is_responses_path(&path) {
                let content_type = content_type_of(&response_headers_map);
                let streaming = content_type.contains("text/event-stream")
                    || response_headers_map.contains_key("transfer-encoding")
                    || !response_headers_map.contains_key("content-length");
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
                            if stage_attempt >= stage_max_retries {
                                finish_sse(client, "", false)?;
                                return Ok(());
                            }
                            gateway_log(
                                "request.retry",
                                &format!("{method} {path} pre-content stream retry #{}", stage_attempt + 1),
                            );
                            sleep_for_retry(rate_limit_retry_delay(None, stage_attempt));
                            stage_attempt += 1;
                            continue;
                        }
                        SseForward::RetryableWithPrefix(resume) => {
                            if stage_attempt >= stage_max_retries {
                                finish_sse(client, "", false)?;
                                return Ok(());
                            }
                            gateway_log(
                                "request.retry",
                                &format!(
                                    "{method} {path} mid-stream retry #{} delivered_chars={}",
                                    stage_attempt + 1,
                                    resume.prefix.len()
                                ),
                            );
                            stream_resume = Some(resume);
                            sleep_for_retry(rate_limit_retry_delay(None, stage_attempt));
                            stage_attempt += 1;
                            continue;
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
                            if stage_attempt >= stage_max_retries {
                                return Err(error);
                            }
                            gateway_log(
                                "request.retry",
                                &format!("{method} {path} body read retry #{}", stage_attempt + 1),
                            );
                            sleep_for_retry(rate_limit_retry_delay(None, stage_attempt));
                            stage_attempt += 1;
                            continue;
                        }
                    }
                }
            }
            write_headers_forced_close(client, &response_headers)?;
            client.write_all(&response_leftover)?;
            std::io::copy(&mut upstream_stream, client)?;
            return Ok(());
        }
        // Error statuses are fully buffered before anything reaches the
        // client, so an upstream drop mid-body retries the whole request.
        match read_full_body(&mut upstream_stream, &response_headers_map, response_leftover) {
            Ok(body) => break (status, response_headers_map, body),
            Err(error) => {
                if stage_attempt >= stage_max_retries {
                    return Err(error);
                }
                gateway_log(
                    "request.retry",
                    &format!("{method} {path} error body read retry #{}", stage_attempt + 1),
                );
                sleep_for_retry(rate_limit_retry_delay(None, stage_attempt));
                stage_attempt += 1;
                continue;
            }
        }
    };
    let body_text = String::from_utf8_lossy(&response_body);
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
                        client.write_all(&retry_headers)?;
                        client.write_all(&retry_body)?;
                        return Ok(());
                    }
                    let retry_text = String::from_utf8_lossy(&retry_body);
                    send_raw(
                        client,
                        &rebuild_response(
                            rewrite_poisoned_upstream_status(retry_status, &retry_text),
                            &parse_headers(&String::from_utf8_lossy(&retry_headers)),
                            &retry_body,
                        ),
                    )?;
                    return Ok(());
                }
            }
        }
    }
    send_raw(
        client,
        &rebuild_response(
            rewrite_poisoned_upstream_status(status, &body_text),
            &response_headers_map,
            &response_body,
        ),
    )?;
    Ok(())
}

fn connect_upstream(upstream: &str) -> anyhow::Result<TcpStream> {
    let address = socket_addr(upstream)?;
    let stream = TcpStream::connect_timeout(&address, UPSTREAM_CONNECT_TIMEOUT)
        .context("could not connect to Sub2API")?;
    stream.set_read_timeout(Some(Duration::from_secs(300))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(300))).ok();
    Ok(stream)
}

fn open_upstream_with_rate_limit_retry(
    upstream: &str,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    max_retries: u32,
) -> anyhow::Result<UpstreamResponse> {
    let max_retries = max_retries.min(MAX_RATE_LIMIT_RETRIES) as usize;
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..=max_retries {
        let mut stream = match connect_upstream(upstream) {
            Ok(stream) => stream,
            Err(error) => {
                last_error = Some(error);
                if attempt >= max_retries {
                    break;
                }
                sleep_for_retry(rate_limit_retry_delay(None, attempt));
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
            last_error = Some(error);
            if attempt >= max_retries {
                break;
            }
            sleep_for_retry(rate_limit_retry_delay(None, attempt));
            continue;
        }
        let (response_headers, mut leftover) = match read_headers(&mut stream) {
            Ok(parts) => parts,
            Err(error) => {
                last_error = Some(error);
                if attempt >= max_retries {
                    break;
                }
                sleep_for_retry(rate_limit_retry_delay(None, attempt));
                continue;
            }
        };
        let response_header_text = String::from_utf8_lossy(&response_headers);
        let status = parse_status(&response_header_text).unwrap_or(200);
        let response_headers_map = parse_headers(&response_header_text);
        // A literal 429 is always retryable. Sub2API also reports an account
        // pool drained by upstream rate limiting as 503; classify it from the
        // (small, fully buffered) error body so it retries like a 429.
        let rate_limited = if status == 429 {
            true
        } else if status == 503 {
            buffer_error_body(&mut stream, &response_headers_map, &mut leftover);
            is_exhausted_account_status(status, &String::from_utf8_lossy(&leftover))
        } else {
            false
        };
        let transient_status = matches!(status, 408 | 425 | 502 | 504);
        if (!rate_limited && !transient_status) || attempt >= max_retries {
            return Ok(UpstreamResponse {
                stream,
                headers: response_headers,
                leftover,
                status,
                header_map: response_headers_map,
            });
        }
        sleep_for_retry(rate_limit_retry_delay(Some(&response_headers_map), attempt));
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("upstream retry budget exhausted")))
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
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => leftover.extend_from_slice(&buffer[..read]),
            }
        }
        return;
    }
    let mut buffer = [0_u8; 8192];
    while leftover.len() < MAX_ERROR_BODY_BYTES {
        match stream.read(&mut buffer) {
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

fn sleep_for_retry(duration: Duration) {
    if cfg!(test) {
        return;
    }
    thread::sleep(duration);
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
    stream.write_all(outgoing.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
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
        stream.read_to_end(&mut response_body).ok();
    }
    Ok((status, response_headers, response_body))
}

fn read_exact_more(stream: &mut TcpStream, body: &mut Vec<u8>, length: usize) -> anyhow::Result<()> {
    while body.len() < length {
        let mut buffer = [0_u8; 8192];
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
        if body.len() > MAX_BODY_BYTES {
            bail!("HTTP body is too large");
        }
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
        let read = stream.read(&mut buffer)?;
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
    client.write_all(prelude.as_bytes())?;
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
#[derive(Debug, PartialEq, Eq)]
enum SseForward {
    Done,
    RetryableBeforeFirstEvent,
    RetryableWithPrefix(StreamResume),
    ReconcileFailed,
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
    text_only: bool,
    response_id: Option<String>,
    item_id: Option<String>,
    reconcile: Option<ReconcileState>,
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
    client.write_all(text.as_bytes())?;
    Ok(())
}

impl<'a> SseSink<'a> {
    fn new(client: &'a mut TcpStream, resume: Option<StreamResume>) -> SseSink<'a> {
        let delivered = resume
            .as_ref()
            .map(|resume| resume.prefix.clone())
            .unwrap_or_default();
        SseSink {
            client,
            terminal: false,
            sent_events: false,
            delivered_text: delivered,
            text_only: true,
            response_id: resume
                .as_ref()
                .and_then(|resume| resume.response_id.clone()),
            item_id: resume.as_ref().and_then(|resume| resume.item_id.clone()),
            reconcile: resume.map(|resume| ReconcileState {
                resume,
                regenerated: String::new(),
                caught_up: false,
                retry_response_id: None,
                retry_item_id: None,
            }),
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
        let SseSink {
            client,
            terminal,
            sent_events,
            delivered_text,
            text_only,
            response_id,
            item_id,
            reconcile,
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
                        client.write_all(rewrite_sse_text(&rewritten).as_bytes())?;
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
                    *terminal = true;
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
            client.write_all(rewrite_sse_text(event).as_bytes())?;
            *sent_events = true;
            Ok(SseFlow::Continue)
        }
    }

    /// Upstream ended (EOF, read error, silence cap, or corrupt frame).
    fn eof_outcome(&mut self, carry: &str) -> anyhow::Result<SseForward> {
        if self.terminal {
            self.finish(carry)?;
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
        if self.text_only {
            return Ok(SseForward::RetryableWithPrefix(self.resume_point()));
        }
        self.finish(carry)?;
        Ok(SseForward::Done)
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
        client.write_all(rewrite_sse_text(carry).as_bytes())?;
    }
    if !terminal {
        client.write_all(b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"upstream_stream_interrupted\",\"message\":\"Upstream stream ended before completion.\"}}}\n\n")?;
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
    match upstream.read(&mut buffer) {
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
    )
}

/// An SSE comment line is ignored by event parsers but resets Codex's stream
/// idle timer, so long Grok/Gemini reasoning pauses no longer abort the task.
fn send_sse_keepalive(client: &mut TcpStream) -> anyhow::Result<()> {
    client.write_all(b": codex-router keep-alive\n\n")?;
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
        let read = match upstream.read(&mut buffer) {
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
) -> anyhow::Result<SseForward> {
    // The SSE prelude is written by the caller, once per client connection,
    // so a transparent retry never duplicates response headers. A mid-stream
    // retry carries the delivered text prefix: the new stream is suppressed
    // until it regenerates that exact prefix, then only the suffix flows.
    // Poll the upstream on a short cadence instead of one long blocking read
    // so keep-alive comments can hold the Codex session open while Grok or
    // Gemini is silent during long reasoning or provider-side failover.
    upstream
        .set_read_timeout(Some(STREAM_POLL_TIMEOUT))
        .ok();
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let mut sink = SseSink::new(client, resume);
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
        stream.read_to_end(&mut body)?;
    }
    Ok(body)
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
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn send_raw(stream: &mut TcpStream, payload: &[u8]) -> anyhow::Result<()> {
    stream.write_all(payload)?;
    Ok(())
}

/// Write upstream response headers to the client with `connection: close`
/// forced. The gateway always closes the client socket after one response;
/// forwarding an upstream keep-alive header would let the Codex client pool a
/// connection that is already gone, and the next request on that pooled
/// connection fails instantly with "error sending request".
fn write_headers_forced_close(client: &mut TcpStream, raw_headers: &[u8]) -> anyhow::Result<()> {
    let text = String::from_utf8_lossy(raw_headers);
    let mut out = String::new();
    let mut wrote_connection = false;
    for (index, line) in text.split("
").enumerate() {
        if line.is_empty() {
            continue;
        }
        if index > 0 && line.to_ascii_lowercase().starts_with("connection:") {
            wrote_connection = true;
            out.push_str("connection: close
");
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !wrote_connection {
        out.push_str("connection: close
");
    }
    out.push('\n');
    client.write_all(out.as_bytes())?;
    Ok(())
}

/// Close the client socket gracefully: signal EOF for our writes, then drain
/// whatever the client already pipelined before dropping the socket. Dropping
/// a socket with unread inbound data makes Windows answer with RST instead of
/// FIN, and the Codex HTTP client can then lose track of the closed pooled
/// connection.
fn close_client_gracefully(mut client: TcpStream) {
    let _ = client.shutdown(std::net::Shutdown::Write);
    let _ = client.set_read_timeout(Some(Duration::from_millis(150)));
    let mut buffer = [0_u8; 8192];
    let mut drained = 0_usize;
    loop {
        match client.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                drained += read;
                if drained >= 256 * 1024 {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn inject_max_output_tokens_honors_the_configured_model_limit() {
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

        // Unmapped models keep the upstream default: nothing is injected.
        let mut unmapped = serde_json::json!({"model":"mot-fixture-unmapped"});
        assert!(!inject_max_output_tokens("/v1/responses", &mut unmapped));
        assert!(unmapped.get("max_output_tokens").is_none());

        set_max_output_tokens_map(HashMap::new());
        assert!(!inject_max_output_tokens("/v1/responses", &mut body));
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
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n11\r\ndata: {\"ok\":1}\r\n\r\n\r\n0\r\n\r\n")
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
        assert_eq!(forwarded, body);
        let text = String::from_utf8_lossy(&response);
        assert!(text.contains("Transfer-Encoding: chunked") || text.contains("transfer-encoding: chunked"));
        assert!(text.contains("data: {\"ok\":1}"));
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

        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            3,
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
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

        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            3,
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

        let response = open_upstream_with_rate_limit_retry(
            &format!("http://127.0.0.1:{}", upstream_addr.port()),
            "POST",
            "/v1/responses",
            &HashMap::new(),
            b"{}",
            2,
        )
        .unwrap();
        // 1 initial attempt + 2 retries, then the 429 is passed through so the
        // caller can surface a normal rate-limit error to the user.
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(response.status, 429);
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
        client
            .write_all(b"POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 18\r\nConnection: close\r\n\r\n{\"model\":\"grok-t\"}")
            .unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        (output, attempts.load(Ordering::Relaxed))
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
    fn tool_call_partial_stream_is_not_retryable_and_fails_cleanly() {
        // A partial stream containing tool-call items cannot be reconciled by
        // text prefix, so it keeps the clean terminal-event behavior.
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
            assert_eq!(outcome, SseForward::Done);
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("function_call"));
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_interrupted"));
    }
}
