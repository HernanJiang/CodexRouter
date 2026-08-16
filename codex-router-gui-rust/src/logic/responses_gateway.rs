//! Local compatibility gateway in front of Sub2API `/v1/responses`.
//!
//! Codex keeps talking to a loopback URL. This process rewrites mixed-model
//! Responses payloads, then forwards the request to Sub2API so a Grok 422 or
//! a poisoned encrypted function output cannot turn into an infinite compact
//! loop or a router-wide 502.

use super::responses_compat::{
    is_compact_path, is_openai_family_model, is_responses_path, rewrite_poisoned_upstream_status,
    rewrite_provider_json, rewrite_sse_text, sanitize_responses_request,
    sanitize_responses_request_aggressive, should_retry_after_upstream_error,
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
use std::time::Duration;
use url::Url;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

struct GatewayState {
    stop: std::sync::Arc<AtomicBool>,
    listen: String,
    upstream: String,
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

pub fn ensure_responses_gateway(sub2api_host: &str) -> anyhow::Result<String> {
    let listen = responses_gateway_url(sub2api_host)?;
    let upstream = sub2api_host.trim().trim_end_matches('/').to_owned();
    let mut slot = gateway_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = slot.as_ref() {
        if state.listen == listen && state.upstream == upstream && !state.stop.load(Ordering::Relaxed)
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
        .spawn(move || run_gateway(listener_url, upstream_url, thread_stop))
        .context("failed to start the responses compatibility gateway")?;
    wait_for_listen(&listen)?;
    *slot = Some(GatewayState {
        stop,
        listen: listen.clone(),
        upstream,
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

fn run_gateway(listen: String, upstream: String, stop: std::sync::Arc<AtomicBool>) {
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
                    let _ = handle_client(stream, &upstream);
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

fn handle_client(mut client: TcpStream, upstream: &str) -> anyhow::Result<()> {
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
    if method == "POST" && is_responses_path(&path) {
        if let Ok(mut json_body) = serde_json::from_slice::<Value>(&request_body) {
            let stats = sanitize_responses_request(&path, &mut json_body);
            if is_compact_path(&path) && !stats.openai_family {
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

    let mut upstream_stream = connect_upstream(upstream)?;
    write_upstream_request(
        &mut upstream_stream,
        &method,
        &path,
        &mut headers,
        &request_body,
        upstream,
    )?;

    let (response_headers, response_leftover) = read_headers(&mut upstream_stream)?;
    let response_header_text = String::from_utf8_lossy(&response_headers);
    let status = parse_status(&response_header_text).unwrap_or(200);
    let response_headers_map = parse_headers(&response_header_text);
    if status < 400 || !is_responses_path(&path) {
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
                sanitize_responses_request_aggressive(&path, &mut json_body);
                let retry_body = serde_json::to_vec(&json_body)?;
                if let Ok((retry_status, retry_headers, retry_body)) =
                    forward_error_exchange(upstream, &path, &retry_body)
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
    body: &[u8],
) -> anyhow::Result<(u16, Vec<u8>, Vec<u8>)> {
    let mut stream = connect_upstream(upstream)?;
    let mut headers = HashMap::new();
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

fn flush_sse_events(client: &mut TcpStream, carry: &mut String) -> anyhow::Result<()> {
    while let Some(split_at) = carry.find("\n\n") {
        let complete = carry[..=split_at + 1].to_owned();
        *carry = carry[split_at + 2..].to_owned();
        client.write_all(rewrite_sse_text(&complete).as_bytes())?;
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
    loop {
        if let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") {
            let line = String::from_utf8_lossy(&raw[..line_end]).into_owned();
            let size = usize::from_str_radix(line.trim(), 16).unwrap_or(0);
            let after_size = line_end + 2;
            if size == 0 {
                flush_sse_events(client, &mut decoded)?;
                return Ok(());
            }
            if raw.len() < after_size + size + 2 {
                let mut buffer = [0_u8; 8192];
                let read = match upstream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(_) => break,
                };
                raw.extend_from_slice(&buffer[..read]);
                continue;
            }
            decoded.push_str(&String::from_utf8_lossy(&raw[after_size..after_size + size]));
            raw.drain(..after_size + size + 2);
            flush_sse_events(client, &mut decoded)?;
            continue;
        }
        let mut buffer = [0_u8; 8192];
        let read = match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        raw.extend_from_slice(&buffer[..read]);
    }
    flush_sse_events(client, &mut decoded)?;
    if !decoded.is_empty() {
        client.write_all(rewrite_sse_text(&decoded).as_bytes())?;
    }
    Ok(())
}

fn forward_plain_sse(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let mut carry = String::from_utf8_lossy(&leftover).into_owned();
    loop {
        flush_sse_events(client, &mut carry)?;
        let mut buffer = [0_u8; 8192];
        let read = match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        carry.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    flush_sse_events(client, &mut carry)?;
    if !carry.is_empty() {
        client.write_all(rewrite_sse_text(&carry).as_bytes())?;
    }
    Ok(())
}

fn forward_agent_sse(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    headers: &HashMap<String, String>,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    write_sse_prelude(client, headers)?;
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
            handle_client(stream, &upstream_url).unwrap();
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
}
