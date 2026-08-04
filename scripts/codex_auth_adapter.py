import argparse
import re
import selectors
import socket
import socketserver
import subprocess
import threading
import time
from pathlib import Path


MAX_HEADER_BYTES = 65536
MAX_CHUNK_LINE_BYTES = 8192
MAX_API_KEY_BYTES = 2048
BUFFER_SIZE = 65536
TRANSFORM_READ_SIZE = 1024
TRANSFORM_BURST_BYTES = 128 * 1024
DEFAULT_MAX_BUFFER_BYTES = 256 * 1024
DEFAULT_MAX_CONNECTIONS = 128
DEFAULT_BACKLOG = 128

_HEADER_NAME = re.compile(rb"^[!#$%&'*+.^_`|~0-9A-Za-z-]+$")
_HEX_SIZE = re.compile(rb"^[0-9A-Fa-f]+$")


class AdapterError(Exception):
    pass


def validate_api_key(api_key: str | bytes) -> bytes:
    if isinstance(api_key, str):
        try:
            value = api_key.encode("ascii")
        except UnicodeEncodeError as exc:
            raise AdapterError("local API key must be ASCII") from exc
    else:
        value = bytes(api_key)
    if (
        not value
        or len(value) > MAX_API_KEY_BYTES
        or b"\r" in value
        or b"\n" in value
    ):
        raise AdapterError("local API key is invalid")
    return value


def receive_request_head(
    connection: socket.socket, timeout: float = 15.0
) -> tuple[bytes, bytes]:
    data = bytearray()
    deadline = time.monotonic() + timeout
    previous_timeout = connection.gettimeout()
    try:
        while True:
            marker = data.find(b"\r\n\r\n")
            if marker >= 0:
                end = marker + 4
                if end > MAX_HEADER_BYTES:
                    raise AdapterError("request headers are too large")
                return bytes(data[:end]), bytes(data[end:])
            if len(data) > MAX_HEADER_BYTES:
                raise AdapterError("request headers are too large")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AdapterError("timed out receiving request headers")
            connection.settimeout(remaining)
            chunk = connection.recv(4096)
            if not chunk:
                raise AdapterError("incomplete request headers")
            data.extend(chunk)
    except socket.timeout as exc:
        raise AdapterError("timed out receiving request headers") from exc
    finally:
        connection.settimeout(previous_timeout)


def _header_tokens(values: list[bytes]) -> set[bytes]:
    tokens: set[bytes] = set()
    for value in values:
        for token in value.split(b","):
            token = token.strip().lower()
            if not token or not _HEADER_NAME.fullmatch(token):
                raise AdapterError("invalid Connection header token")
            tokens.add(token)
    return tokens


def _content_length(values: list[bytes]) -> int | None:
    lengths: list[int] = []
    for value in values:
        for item in value.split(b","):
            item = item.strip()
            if not item or not item.isdigit():
                raise AdapterError("invalid Content-Length header")
            lengths.append(int(item, 10))
    if not lengths:
        return None
    if any(length != lengths[0] for length in lengths[1:]):
        raise AdapterError("conflicting Content-Length headers")
    return lengths[0]


def _rewrite_and_frame(
    request_head: bytes, api_key: bytes
) -> tuple[bytes, str, int, bool]:
    if not request_head.endswith(b"\r\n\r\n"):
        raise AdapterError("incomplete request headers")
    lines = request_head[:-4].split(b"\r\n")
    request_parts = lines[0].split(b" ") if lines else []
    if (
        len(request_parts) != 3
        or not request_parts[0]
        or not request_parts[1]
        or request_parts[2] not in (b"HTTP/1.0", b"HTTP/1.1")
    ):
        raise AdapterError("invalid HTTP request line")

    headers: list[tuple[bytes, bytes, bytes]] = []
    by_name: dict[bytes, list[bytes]] = {}
    for line in lines[1:]:
        name, separator, value = line.partition(b":")
        if (
            not separator
            or name != name.strip()
            or not _HEADER_NAME.fullmatch(name)
        ):
            raise AdapterError("invalid HTTP header")
        lower_name = name.lower()
        headers.append((lower_name, value, line))
        by_name.setdefault(lower_name, []).append(value.strip())

    connection_tokens = _header_tokens(by_name.get(b"connection", []))
    if connection_tokens.intersection(
        {b"content-length", b"transfer-encoding", b"host", b"trailer"}
    ):
        raise AdapterError("Connection header names a framing header")

    transfer_values = by_name.get(b"transfer-encoding", [])
    content_length = _content_length(by_name.get(b"content-length", []))
    if transfer_values and content_length is not None:
        raise AdapterError("both Transfer-Encoding and Content-Length are present")

    if transfer_values:
        codings = [
            token.strip().lower()
            for value in transfer_values
            for token in value.split(b",")
        ]
        if not codings or codings[-1] != b"chunked" or codings.count(b"chunked") != 1:
            raise AdapterError("unsupported Transfer-Encoding framing")
        body_mode = "chunked"
        body_length = 0
    elif content_length is not None and content_length > 0:
        body_mode = "fixed"
        body_length = content_length
    else:
        body_mode = "head"
        body_length = 0

    upgrade_values = by_name.get(b"upgrade", [])
    is_upgrade = b"upgrade" in connection_tokens and bool(upgrade_values)
    is_connect = request_parts[0].upper() == b"CONNECT"
    if is_upgrade or is_connect:
        body_mode = "raw"
        body_length = 0

    always_remove = {
        b"authorization",
        b"connection",
        b"keep-alive",
        b"proxy-authorization",
        b"proxy-connection",
        b"te",
    }
    rewritten = [lines[0]]
    for lower_name, _, raw_line in headers:
        if lower_name in always_remove or lower_name in connection_tokens:
            if is_upgrade and lower_name == b"upgrade":
                rewritten.append(raw_line)
            continue
        if lower_name == b"upgrade" and not is_upgrade:
            continue
        rewritten.append(raw_line)

    rewritten.append(b"Authorization: Bearer " + api_key)
    if is_upgrade:
        rewritten.append(b"Connection: Upgrade")
    elif b"close" in connection_tokens:
        rewritten.append(b"Connection: close")
    close_after_request = b"close" in connection_tokens and body_mode != "raw"
    return (
        b"\r\n".join(rewritten) + b"\r\n\r\n",
        body_mode,
        body_length,
        close_after_request,
    )


def rewrite_request_head(request_head: bytes, api_key: str | bytes) -> bytes:
    rewritten, _, _, _ = _rewrite_and_frame(request_head, validate_api_key(api_key))
    return rewritten


class RequestStreamRewriter:
    def __init__(self, api_key: str | bytes):
        self.api_key = validate_api_key(api_key)
        self.pending = bytearray()
        self.state = "head"
        self.remaining = 0
        self.trailer_bytes = 0
        self.close_after_request = False

    def feed(self, data: bytes) -> bytes:
        if data:
            self.pending.extend(data)
        output = bytearray()

        while self.pending:
            if self.state == "raw":
                output.extend(self.pending)
                self.pending.clear()
                break

            if self.state == "closed":
                raise AdapterError("data followed a Connection: close request")

            if self.state == "head":
                marker = self.pending.find(b"\r\n\r\n")
                if marker < 0:
                    if len(self.pending) > MAX_HEADER_BYTES:
                        raise AdapterError("request headers are too large")
                    break
                end = marker + 4
                if end > MAX_HEADER_BYTES:
                    raise AdapterError("request headers are too large")
                head = bytes(self.pending[:end])
                del self.pending[:end]
                (
                    rewritten,
                    self.state,
                    self.remaining,
                    self.close_after_request,
                ) = _rewrite_and_frame(head, self.api_key)
                output.extend(rewritten)
                if self.state == "head":
                    if self.close_after_request:
                        self.state = "closed"
                    continue
                if self.state == "chunked":
                    self.state = "chunk_size"
                continue

            if self.state == "fixed":
                take = min(self.remaining, len(self.pending))
                output.extend(self.pending[:take])
                del self.pending[:take]
                self.remaining -= take
                if self.remaining == 0:
                    self.state = "closed" if self.close_after_request else "head"
                continue

            if self.state == "chunk_size":
                marker = self.pending.find(b"\r\n")
                if marker < 0:
                    if len(self.pending) > MAX_CHUNK_LINE_BYTES:
                        raise AdapterError("chunk size line is too large")
                    break
                line_end = marker + 2
                line = bytes(self.pending[:line_end])
                size_text = line[:-2].split(b";", 1)[0].strip()
                if not size_text or not _HEX_SIZE.fullmatch(size_text):
                    raise AdapterError("invalid chunk size")
                self.remaining = int(size_text, 16)
                output.extend(line)
                del self.pending[:line_end]
                if self.remaining == 0:
                    self.state = "chunk_trailers"
                    self.trailer_bytes = 0
                else:
                    self.state = "chunk_data"
                continue

            if self.state == "chunk_data":
                take = min(self.remaining, len(self.pending))
                output.extend(self.pending[:take])
                del self.pending[:take]
                self.remaining -= take
                if self.remaining == 0:
                    self.state = "chunk_crlf"
                continue

            if self.state == "chunk_crlf":
                if len(self.pending) < 2:
                    break
                if self.pending[:2] != b"\r\n":
                    raise AdapterError("invalid chunk terminator")
                output.extend(b"\r\n")
                del self.pending[:2]
                self.state = "chunk_size"
                continue

            if self.state == "chunk_trailers":
                marker = self.pending.find(b"\r\n")
                if marker < 0:
                    if self.trailer_bytes + len(self.pending) > MAX_HEADER_BYTES:
                        raise AdapterError("chunk trailers are too large")
                    break
                line_end = marker + 2
                line = bytes(self.pending[:line_end])
                self.trailer_bytes += line_end
                if self.trailer_bytes > MAX_HEADER_BYTES:
                    raise AdapterError("chunk trailers are too large")
                if line != b"\r\n":
                    name, separator, _ = line[:-2].partition(b":")
                    if not separator or not _HEADER_NAME.fullmatch(name):
                        raise AdapterError("invalid chunk trailer")
                output.extend(line)
                del self.pending[:line_end]
                if line == b"\r\n":
                    self.state = "closed" if self.close_after_request else "head"
                    self.trailer_bytes = 0
                continue

            raise AdapterError("invalid request parser state")

        return bytes(output)

    def finish(self) -> None:
        if self.state in ("raw", "closed"):
            return
        if self.state != "head" or self.pending:
            raise AdapterError("client closed during an HTTP request")


def read_local_api_key(powershell: Path, credential_script: Path) -> str:
    creation_flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    try:
        result = subprocess.run(
            [
                str(powershell),
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                str(credential_script),
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=5,
            creationflags=creation_flags,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise AdapterError("could not load the local API key") from exc

    api_key = result.stdout.strip()
    validate_api_key(api_key)
    return api_key


def configure_stream_socket(connection: socket.socket) -> None:
    try:
        connection.setsockopt(socket.SOL_SOCKET, socket.SO_KEEPALIVE, 1)
    except OSError:
        pass
    try:
        connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    except OSError:
        pass
    if hasattr(socket, "SIO_KEEPALIVE_VALS"):
        try:
            connection.ioctl(socket.SIO_KEEPALIVE_VALS, (1, 60000, 20000))
        except OSError:
            pass


def relay(
    left: socket.socket,
    right: socket.socket,
    *,
    transform_left_to_right: RequestStreamRewriter | None = None,
    initial_to_left: bytes = b"",
    initial_to_right: bytes = b"",
    max_buffer_bytes: int = DEFAULT_MAX_BUFFER_BYTES,
    idle_timeout: float = 0,
) -> None:
    if max_buffer_bytes < BUFFER_SIZE:
        raise ValueError("max_buffer_bytes must be at least BUFFER_SIZE")

    to_left = bytearray(initial_to_left)
    to_right = bytearray(initial_to_right)
    if len(to_left) > max_buffer_bytes or len(to_right) > max_buffer_bytes + TRANSFORM_BURST_BYTES:
        raise AdapterError("initial relay data exceeds the buffer limit")

    configure_stream_socket(left)
    configure_stream_socket(right)
    left.setblocking(False)
    right.setblocking(False)
    read_open = {left: True, right: True}
    write_open = {left: True, right: True}
    registered: dict[socket.socket, int] = {}
    last_activity = time.monotonic()

    def update(selector: selectors.BaseSelector, sock: socket.socket, events: int) -> None:
        current = registered.get(sock)
        if events == 0:
            if current is not None:
                selector.unregister(sock)
                registered.pop(sock, None)
        elif current is None:
            selector.register(sock, events)
            registered[sock] = events
        elif current != events:
            selector.modify(sock, events)
            registered[sock] = events

    def stop_reading(sock: socket.socket) -> None:
        read_open[sock] = False
        try:
            sock.shutdown(socket.SHUT_RD)
        except OSError:
            pass

    def stop_writing(sock: socket.socket) -> None:
        write_open[sock] = False
        try:
            sock.shutdown(socket.SHUT_WR)
        except OSError:
            pass

    with selectors.DefaultSelector() as selector:
        while True:
            if not read_open[left] and not to_right and write_open[right]:
                stop_writing(right)
            if not read_open[right] and not to_left and write_open[left]:
                stop_writing(left)
            if not write_open[right] and read_open[left]:
                stop_reading(left)
            if not write_open[left] and read_open[right]:
                stop_reading(right)

            if not read_open[left] and not read_open[right] and not to_left and not to_right:
                return

            left_events = 0
            if read_open[left] and write_open[right] and len(to_right) < max_buffer_bytes:
                left_events |= selectors.EVENT_READ
            if write_open[left] and to_left:
                left_events |= selectors.EVENT_WRITE
            right_events = 0
            if read_open[right] and write_open[left] and len(to_left) < max_buffer_bytes:
                right_events |= selectors.EVENT_READ
            if write_open[right] and to_right:
                right_events |= selectors.EVENT_WRITE
            update(selector, left, left_events)
            update(selector, right, right_events)

            if not registered:
                return
            wait = None
            if idle_timeout > 0:
                wait = max(0.0, idle_timeout - (time.monotonic() - last_activity))
                if wait == 0:
                    return
            events = selector.select(wait)
            if not events:
                return

            for key, mask in events:
                source = key.fileobj
                if mask & selectors.EVENT_READ:
                    destination_buffer = to_right if source is left else to_left
                    capacity = max_buffer_bytes - len(destination_buffer)
                    read_size = min(
                        TRANSFORM_READ_SIZE
                        if source is left and transform_left_to_right is not None
                        else BUFFER_SIZE,
                        capacity,
                    )
                    try:
                        data = source.recv(read_size)
                    except (BlockingIOError, InterruptedError):
                        data = None
                    except OSError:
                        data = b""
                    if data:
                        if source is left and transform_left_to_right is not None:
                            data = transform_left_to_right.feed(data)
                            if len(destination_buffer) + len(data) > max_buffer_bytes + TRANSFORM_BURST_BYTES:
                                raise AdapterError("rewritten request burst exceeds the buffer limit")
                        destination_buffer.extend(data)
                        last_activity = time.monotonic()
                    elif data == b"":
                        read_open[source] = False
                        if source is left and transform_left_to_right is not None:
                            transform_left_to_right.finish()

                if mask & selectors.EVENT_WRITE:
                    output = to_left if source is left else to_right
                    try:
                        sent = source.send(output)
                    except (BlockingIOError, InterruptedError):
                        sent = 0
                    except OSError:
                        sent = -1
                    if sent > 0:
                        del output[:sent]
                        last_activity = time.monotonic()
                    elif sent < 0:
                        write_open[source] = False
                        output.clear()


class BoundedThreadingTCPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    block_on_close = False

    def __init__(
        self,
        address: tuple[str, int],
        handler: type[socketserver.BaseRequestHandler],
        *,
        max_connections: int,
        backlog: int,
    ):
        if max_connections < 1 or backlog < 1:
            raise ValueError("connection and backlog limits must be positive")
        self._connection_slots = threading.BoundedSemaphore(max_connections)
        self.request_queue_size = backlog
        super().__init__(address, handler)

    def process_request(self, request: socket.socket, client_address: tuple) -> None:
        if not self._connection_slots.acquire(blocking=False):
            try:
                request.settimeout(1)
                request.sendall(
                    b"HTTP/1.1 503 Service Unavailable\r\n"
                    b"Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
            except OSError:
                pass
            finally:
                self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self._connection_slots.release()
            raise

    def process_request_thread(self, request: socket.socket, client_address: tuple) -> None:
        try:
            super().process_request_thread(request, client_address)
        finally:
            self._connection_slots.release()


class CodexAuthAdapterHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        if self.client_address[0] not in ("127.0.0.1", "::1"):
            return

        upstream: socket.socket | None = None
        proxying_started = False
        try:
            request_head, buffered_body = receive_request_head(
                self.request, self.server.header_timeout
            )
            rewriter = RequestStreamRewriter(self.server.api_key)
            initial = rewriter.feed(request_head + buffered_body)
            upstream = socket.create_connection(
                (self.server.upstream_host, self.server.upstream_port),
                timeout=self.server.connect_timeout,
            )
            proxying_started = True
            relay(
                self.request,
                upstream,
                transform_left_to_right=rewriter,
                initial_to_right=initial,
                max_buffer_bytes=self.server.max_buffer_bytes,
                idle_timeout=self.server.idle_timeout,
            )
        except AdapterError:
            if not proxying_started:
                self._send_error(b"400 Bad Request")
        except OSError:
            if not proxying_started:
                self._send_error(b"502 Bad Gateway")
        finally:
            if upstream is not None:
                upstream.close()

    def _send_error(self, status: bytes) -> None:
        try:
            self.request.sendall(
                b"HTTP/1.1 "
                + status
                + b"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
        except OSError:
            pass


class CodexAuthAdapterServer(BoundedThreadingTCPServer):
    def __init__(
        self,
        address: tuple[str, int],
        upstream_host: str,
        upstream_port: int,
        powershell: Path | None = None,
        credential_script: Path | None = None,
        *,
        api_key: str | bytes | None = None,
        max_connections: int = DEFAULT_MAX_CONNECTIONS,
        backlog: int = DEFAULT_BACKLOG,
        max_buffer_bytes: int = DEFAULT_MAX_BUFFER_BYTES,
        header_timeout: float = 15.0,
        connect_timeout: float = 5.0,
        idle_timeout: float = 0,
    ):
        if max_buffer_bytes < BUFFER_SIZE:
            raise ValueError("max_buffer_bytes must be at least BUFFER_SIZE")
        if header_timeout <= 0 or connect_timeout <= 0 or idle_timeout < 0:
            raise ValueError("timeouts must be positive, except idle_timeout may be zero")
        if api_key is None:
            if powershell is None or credential_script is None:
                raise ValueError("credential loader paths are required")
            api_key = read_local_api_key(powershell, credential_script)
        self.api_key = validate_api_key(api_key)
        self.upstream_host = upstream_host
        self.upstream_port = upstream_port
        self.max_buffer_bytes = max_buffer_bytes
        self.header_timeout = header_timeout
        self.connect_timeout = connect_timeout
        self.idle_timeout = idle_timeout
        super().__init__(
            address,
            CodexAuthAdapterHandler,
            max_connections=max_connections,
            backlog=backlog,
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen-port", type=int, default=18081)
    parser.add_argument("--upstream-port", type=int, default=18080)
    parser.add_argument("--credential-script", type=Path, required=True)
    parser.add_argument(
        "--powershell",
        type=Path,
        default=Path(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        ),
    )
    parser.add_argument("--max-connections", type=int, default=DEFAULT_MAX_CONNECTIONS)
    parser.add_argument("--backlog", type=int, default=DEFAULT_BACKLOG)
    parser.add_argument("--max-buffer-bytes", type=int, default=DEFAULT_MAX_BUFFER_BYTES)
    parser.add_argument("--header-timeout", type=float, default=15.0)
    parser.add_argument("--connect-timeout", type=float, default=5.0)
    parser.add_argument(
        "--idle-timeout",
        type=float,
        default=0,
        help="idle tunnel timeout in seconds; 0 disables application-level expiry",
    )
    args = parser.parse_args()

    with CodexAuthAdapterServer(
        ("127.0.0.1", args.listen_port),
        "127.0.0.1",
        args.upstream_port,
        args.powershell,
        args.credential_script,
        max_connections=args.max_connections,
        backlog=args.backlog,
        max_buffer_bytes=args.max_buffer_bytes,
        header_timeout=args.header_timeout,
        connect_timeout=args.connect_timeout,
        idle_timeout=args.idle_timeout,
    ) as server:
        server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()
