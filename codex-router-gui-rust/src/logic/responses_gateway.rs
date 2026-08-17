//! Local compatibility gateway in front of Sub2API `/v1/responses`.
//!
//! Codex keeps talking to a loopback URL. This process rewrites mixed-model
//! Responses payloads, then forwards the request to Sub2API so a Grok 422 or
//! a poisoned encrypted function output cannot turn into an infinite compact
//! loop or a router-wide 502.

use super::responses_compat::{
    is_compact_path, is_exhausted_account_status, is_openai_family_model, is_responses_path,
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
/// Overridable through the Router config. Six retries use the staged
/// 2s / 10s / 30s / 1min / 3min / 5min waits.
pub const DEFAULT_RATE_LIMIT_RETRIES: u32 = 6;
/// Hard ceiling for a configured retry count so a typo cannot pin a worker
/// thread on a sleeping retry loop forever.
const MAX_RATE_LIMIT_RETRIES: u32 = 32;
/// Ceiling for one backoff step. A `Retry-After` hint above this is clamped
/// instead of skipping the retry, so the conversation keeps waiting.
const MAX_RATE_LIMIT_DELAY: Duration = Duration::from_secs(300);
const RATE_LIMIT_RETRY_DELAYS_SECS: [u64; 6] = [2, 10, 30, 60, 180, 300];
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
        state.stop.store(true, Ordering::Relaxed);
        poke_listener(&state.listen);
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let listener_url = listen.clone();
    let upstream_url = upstream.clone();
    let thread_stop = stop.clone();
    thread::Builder::new()
        .name("codex-responses-gateway".to_owned())
        .spawn(move || {
            run_gateway(listener_url, upstream_url, thread_stop, rate_limit_max_retries)
        })
        .context("failed to start the responses compatibility gateway")?;
    wait_for_listen(&listen)?;
    *slot = Some(GatewayState {
        stop,
        listen: listen.clone(),
        upstream,
        rate_limit_retries: rate_limit_max_retries,
    });
    Ok(listen)
}

pub fn stop_responses_gateway() {
    let mut slot = gateway_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = slot.take() {
        state.stop.store(true, Ordering::Relaxed);
        poke_listener(&state.listen);
    }
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

fn wait_for_listen(base: &str) -> anyhow::Result<()> {
    let address = socket_addr(base)?;
    for _ in 0..40 {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("responses compatibility gateway did not start on {address}")
}

fn run_gateway(
    listen: String,
    upstream: String,
    stop: std::sync::Arc<AtomicBool>,
    rate_limit_max_retries: u32,
) {
    let Ok(address) = socket_addr(&listen) else {
        return;
    };
    let Ok(listener) = TcpListener::bind(address) else {
        return;
    };
    let _ = listener.set_nonblocking(true);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let upstream = upstream.clone();
                thread::spawn(move || {
                    let _ = handle_client(stream, &upstream, rate_limit_max_retries);
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
    mut client: TcpStream,
    upstream: &str,
    rate_limit_max_retries: u32,
) -> anyhow::Result<()> {
    client
        .set_read_timeout(Some(HEADER_TIMEOUT))
        .and_then(|_| client.set_write_timeout(Some(Duration::from_secs(300))))
        .ok();
    let (header_bytes, leftover) = read_headers(&mut client)?;
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
            &mut client,
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
            send_simple(&mut client, 413, b"request too large")?;
            return Ok(());
        }
        read_exact_more(&mut client, &mut body, length)?;
        body.truncate(length);
    }

    let mut request_body = body;
    let mut openai_family = false;
    if method == "POST" && is_responses_path(&path) {
        if let Ok(mut json_body) = serde_json::from_slice::<Value>(&request_body) {
            openai_family = json_body
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(is_openai_family_model);
            if !openai_family {
                let stats = sanitize_responses_request(&path, &mut json_body);
                if is_compact_path(&path) {
                    let output = json_body
                        .get("input")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let payload =
                        serde_json::to_vec(&synthetic_compact_response(&stats.model, &output))?;
                    send_status(&mut client, 200, "application/json", &payload)?;
                    return Ok(());
                }
                request_body = serde_json::to_vec(&json_body)?;
            }
        }
    }

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
            client.write_all(&response_headers)?;
            client.write_all(&response_leftover)?;
            std::io::copy(&mut upstream_stream, &mut client)?;
            return Ok(());
        }
        if status < 400 && is_responses_path(&path) {
            let content_type = content_type_of(&response_headers_map);
            let streaming = content_type.contains("text/event-stream")
                || response_headers_map.contains_key("transfer-encoding")
                || !response_headers_map.contains_key("content-length");
            if streaming {
                return forward_agent_sse(
                    &mut client,
                    &mut upstream_stream,
                    &response_headers_map,
                    response_leftover,
                );
            }
            if content_type.contains("json") {
                return forward_rewritten_json(
                    &mut client,
                    &mut upstream_stream,
                    &response_headers_map,
                    response_leftover,
                );
            }
        }
        client.write_all(&response_headers)?;
        client.write_all(&response_leftover)?;
        std::io::copy(&mut upstream_stream, &mut client)?;
        return Ok(());
    }

    let mut response_body = response_leftover;
    if let Some(length) = response_headers_map
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        read_exact_more(&mut upstream_stream, &mut response_body, length)?;
        response_body.truncate(length);
    } else {
        upstream_stream.read_to_end(&mut response_body).ok();
    }
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
                        &mut client,
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
        &mut client,
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

/// Staged backoff: 2s, 10s, 30s, 1min, 3min, 5min.
/// A `Retry-After` hint can only lengthen a step, never shorten it, so a
/// 1-second burst hint cannot burn the retry budget.
fn rate_limit_retry_delay(
    headers: Option<&HashMap<String, String>>,
    attempt: usize,
) -> Duration {
    let staged = RATE_LIMIT_RETRY_DELAYS_SECS[attempt.min(RATE_LIMIT_RETRY_DELAYS_SECS.len() - 1)];
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

fn flush_sse_events(
    client: &mut TcpStream,
    carry: &mut String,
    terminal: &mut bool,
) -> anyhow::Result<()> {
    while let Some(complete) = take_sse_event(carry) {
        *terminal |= sse_event_is_terminal(&complete);
        client.write_all(rewrite_sse_text(&complete).as_bytes())?;
    }
    Ok(())
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
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let mut raw = leftover;
    let mut decoded = String::new();
    let mut terminal = false;
    let mut last_activity = Instant::now();
    loop {
        if let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") {
            let line = String::from_utf8_lossy(&raw[..line_end]).into_owned();
            let size = match parse_chunk_size(&line) {
                Ok(size) => size,
                Err(_) => {
                    // A corrupt chunk frame must not silently drop the session.
                    // Flush the complete events already decoded and close with
                    // an explicit terminal event instead of an abrupt EOF.
                    flush_sse_events(client, &mut decoded, &mut terminal)?;
                    return finish_sse(client, "", terminal);
                }
            };
            let after_size = line_end + 2;
            if size == 0 {
                flush_sse_events(client, &mut decoded, &mut terminal)?;
                return finish_sse(client, &decoded, terminal);
            }
            if raw.len() < after_size + size + 2 {
                match read_upstream_chunk(upstream, &mut raw, &mut last_activity, client)? {
                    ChunkRead::Received => continue,
                    ChunkRead::Ended => return finish_sse(client, &decoded, terminal),
                }
            }
            decoded.push_str(&String::from_utf8_lossy(&raw[after_size..after_size + size]));
            raw.drain(..after_size + size + 2);
            flush_sse_events(client, &mut decoded, &mut terminal)?;
            continue;
        }
        match read_upstream_chunk(upstream, &mut raw, &mut last_activity, client)? {
            ChunkRead::Received => continue,
            ChunkRead::Ended => return finish_sse(client, &decoded, terminal),
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
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let mut carry = String::from_utf8_lossy(&leftover).into_owned();
    let mut terminal = false;
    let mut last_activity = Instant::now();
    loop {
        flush_sse_events(client, &mut carry, &mut terminal)?;
        let mut buffer = [0_u8; 8192];
        let read = match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                last_activity = Instant::now();
                read
            }
            Err(error) if stream_read_timed_out(&error) => {
                if last_activity.elapsed() >= STREAM_MAX_SILENCE {
                    return finish_sse(client, &carry, terminal);
                }
                if send_sse_keepalive(client).is_err() {
                    // The Codex client is gone; stop the worker quietly.
                    return Ok(());
                }
                continue;
            }
            Err(_) => return finish_sse(client, &carry, terminal),
        };
        carry.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    flush_sse_events(client, &mut carry, &mut terminal)?;
    finish_sse(client, &carry, terminal)
}

fn forward_agent_sse(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    headers: &HashMap<String, String>,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    write_sse_prelude(client, headers)?;
    // Poll the upstream on a short cadence instead of one long blocking read
    // so keep-alive comments can hold the Codex session open while Grok or
    // Gemini is silent during long reasoning or provider-side failover.
    upstream
        .set_read_timeout(Some(STREAM_POLL_TIMEOUT))
        .ok();
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    if chunked {
        decode_chunked_to_sse(client, upstream, leftover)
    } else {
        forward_plain_sse(client, upstream, leftover)
    }
}

fn forward_rewritten_json(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    headers: &HashMap<String, String>,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let mut body = leftover;
    if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        read_exact_more(upstream, &mut body, length)?;
        body.truncate(length);
    } else {
        upstream.read_to_end(&mut body).ok();
    }
    if let Ok(mut json) = serde_json::from_slice::<Value>(&body) {
        rewrite_provider_json(&mut json);
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

#[cfg(test)]
mod tests {
    use super::*;
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
            let (stream, _) = gateway.accept().unwrap();
            handle_client(stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
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
            let (stream, _) = gateway.accept().unwrap();
            handle_client(stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
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
            let (stream, _) = gateway.accept().unwrap();
            handle_client(stream, &upstream_url, DEFAULT_RATE_LIMIT_RETRIES).unwrap();
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
    fn interrupted_plain_sse_emits_a_failed_terminal_event() {
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
            forward_plain_sse(&mut client, &mut upstream_stream, Vec::new()).unwrap();
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_interrupted"));
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
            forward_plain_sse(&mut client, &mut upstream_stream, Vec::new()).unwrap();
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
    fn corrupt_chunk_frame_closes_with_a_terminal_event_instead_of_silence() {
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
            decode_chunked_to_sse(&mut client, &mut upstream_stream, Vec::new()).unwrap();
        });
        let mut client = TcpStream::connect(client_addr).unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).unwrap();
        worker.join().unwrap();
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_interrupted"));
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
                Some(&HashMap::from([("retry-after".to_owned(), "600".to_owned())])),
                0,
            ),
            MAX_RATE_LIMIT_DELAY
        );
        assert_eq!(
            rate_limit_retry_delay(
                Some(&HashMap::from([("retry-after".to_owned(), "3".to_owned())])),
                0,
            ),
            Duration::from_secs(3)
        );
        assert_eq!(
            rate_limit_retry_delay(
                Some(&HashMap::from([("retry-after".to_owned(), "1".to_owned())])),
                0,
            ),
            Duration::from_secs(2)
        );
        assert_eq!(
            rate_limit_retry_delay(None, 0),
            Duration::from_secs(2)
        );
        assert_eq!(
            rate_limit_retry_delay(None, 1),
            Duration::from_secs(10)
        );
        assert_eq!(
            rate_limit_retry_delay(None, 2),
            Duration::from_secs(30)
        );
        assert_eq!(
            rate_limit_retry_delay(None, 3),
            Duration::from_secs(60)
        );
        assert_eq!(
            rate_limit_retry_delay(None, 4),
            Duration::from_secs(180)
        );
        assert_eq!(
            rate_limit_retry_delay(None, 5),
            Duration::from_secs(300)
        );
    }
}
