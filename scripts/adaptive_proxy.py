import argparse
import ipaddress
import re
import selectors
import socket
import socketserver
import threading
import time


MAX_HEADER_BYTES = 65536
BUFFER_SIZE = 65536
DEFAULT_MAX_BUFFER_BYTES = 256 * 1024
DEFAULT_MAX_CONNECTIONS = 128
DEFAULT_BACKLOG = 128

_HOST_LABEL = re.compile(r"^[A-Za-z0-9_](?:[A-Za-z0-9_-]{0,61}[A-Za-z0-9_])?$")
_IPV6_SCOPE = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")


class ProxyError(OSError):
    pass


class ProxyUnavailableError(ProxyError):
    pass


class ProxyRejectedError(ProxyError):
    pass


def recv_headers(
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
                    raise ValueError("HTTP proxy header is too large")
                return bytes(data[:end]), bytes(data[end:])
            if len(data) > MAX_HEADER_BYTES:
                raise ValueError("HTTP proxy header is too large")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("timed out receiving HTTP proxy headers")
            connection.settimeout(remaining)
            chunk = connection.recv(4096)
            if not chunk:
                raise ValueError("incomplete HTTP proxy headers")
            data.extend(chunk)
    except socket.timeout as exc:
        raise TimeoutError("timed out receiving HTTP proxy headers") from exc
    finally:
        connection.settimeout(previous_timeout)


def parse_connect_authority(target: str) -> tuple[str, int]:
    try:
        target.encode("ascii")
    except UnicodeEncodeError as exc:
        raise ValueError("CONNECT authority must be ASCII") from exc
    if not target or any(character.isspace() for character in target):
        raise ValueError("invalid CONNECT authority")

    if target.startswith("["):
        close = target.find("]")
        if close < 0 or close == 1 or target[close + 1 : close + 2] != ":":
            raise ValueError("invalid bracketed IPv6 CONNECT authority")
        host = target[1:close]
        port_text = target[close + 2 :]
        if not port_text:
            raise ValueError("CONNECT authority is missing a port")
        address, separator, scope = host.partition("%")
        if separator and not _IPV6_SCOPE.fullmatch(scope):
            raise ValueError("invalid IPv6 scope identifier")
        try:
            ipaddress.IPv6Address(address)
        except ValueError as exc:
            raise ValueError("invalid IPv6 CONNECT authority") from exc
    else:
        if target.count(":") != 1:
            raise ValueError("CONNECT authority must contain one host:port separator")
        host, port_text = target.rsplit(":", 1)
        if not host:
            raise ValueError("invalid CONNECT host")
        labels = host.split(".")
        if any(not _HOST_LABEL.fullmatch(label) for label in labels):
            raise ValueError("invalid CONNECT host")
        if len(host) > 253:
            raise ValueError("CONNECT host is too long")

    if not port_text.isdigit():
        raise ValueError("invalid CONNECT port")
    port = int(port_text, 10)
    if port < 1 or port > 65535:
        raise ValueError("CONNECT port is out of range")
    return host, port


def format_connect_authority(host: str, port: int) -> str:
    formatted_host = f"[{host}]" if ":" in host else host
    return f"{formatted_host}:{port}"


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


def connect_direct(host: str, port: int, timeout: float = 8.0) -> socket.socket:
    upstream = socket.create_connection((host, port), timeout=timeout)
    upstream.settimeout(None)
    configure_stream_socket(upstream)
    return upstream


def connect_ipv4(host: str, port: int, timeout: float = 8.0) -> socket.socket:
    """Compatibility alias; direct connections now support IPv4 and IPv6."""
    return connect_direct(host, port, timeout)


def connect_through_clash(
    host: str,
    port: int,
    clash_port: int,
    timeout: float = 3.0,
) -> tuple[socket.socket, bytes]:
    try:
        upstream = socket.create_connection(("127.0.0.1", clash_port), timeout=timeout)
    except OSError as exc:
        raise ProxyUnavailableError("local proxy is unavailable") from exc

    try:
        authority = format_connect_authority(host, port)
        request = (
            f"CONNECT {authority} HTTP/1.1\r\n"
            f"Host: {authority}\r\n"
            "Proxy-Connection: Keep-Alive\r\n\r\n"
        ).encode("ascii")
        upstream.settimeout(timeout)
        upstream.sendall(request)
        try:
            response_head, buffered_data = recv_headers(upstream, timeout)
        except (OSError, TimeoutError, ValueError) as exc:
            raise ProxyUnavailableError("local proxy CONNECT handshake failed") from exc
        status_line = response_head.split(b"\r\n", 1)[0]
        parts = status_line.split(b" ", 2)
        if (
            len(parts) < 2
            or parts[0] not in (b"HTTP/1.0", b"HTTP/1.1")
            or not parts[1].isdigit()
        ):
            raise ProxyRejectedError("local proxy returned an invalid status line")
        status = int(parts[1], 10)
        if status != 200:
            raise ProxyRejectedError(f"local proxy rejected CONNECT with status {status}")
        upstream.settimeout(None)
        configure_stream_socket(upstream)
        return upstream, buffered_data
    except Exception:
        upstream.close()
        raise


def connect_with_policy(
    host: str,
    port: int,
    *,
    policy: str,
    clash_port: int,
    proxy_timeout: float,
    direct_timeout: float,
) -> tuple[socket.socket, bytes]:
    if policy == "direct":
        return connect_direct(host, port, direct_timeout), b""
    if policy not in ("prefer", "required"):
        raise ValueError("invalid proxy policy")
    try:
        return connect_through_clash(host, port, clash_port, proxy_timeout)
    except ProxyUnavailableError:
        if policy == "prefer":
            return connect_direct(host, port, direct_timeout), b""
        raise


def relay(
    left: socket.socket,
    right: socket.socket,
    *,
    initial_to_left: bytes = b"",
    initial_to_right: bytes = b"",
    max_buffer_bytes: int = DEFAULT_MAX_BUFFER_BYTES,
    idle_timeout: float = 0,
) -> None:
    if max_buffer_bytes < BUFFER_SIZE:
        raise ValueError("max_buffer_bytes must be at least BUFFER_SIZE")
    to_left = bytearray(initial_to_left)
    to_right = bytearray(initial_to_right)
    if len(to_left) > max_buffer_bytes or len(to_right) > max_buffer_bytes:
        raise ProxyError("initial relay data exceeds the buffer limit")

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
                    read_size = min(BUFFER_SIZE, max_buffer_bytes - len(destination_buffer))
                    try:
                        data = source.recv(read_size)
                    except (BlockingIOError, InterruptedError):
                        data = None
                    except OSError:
                        data = b""
                    if data:
                        destination_buffer.extend(data)
                        last_activity = time.monotonic()
                    elif data == b"":
                        read_open[source] = False

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


class AdaptiveProxyHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        if self.client_address[0] not in ("127.0.0.1", "::1"):
            return
        upstream: socket.socket | None = None
        try:
            header, buffered_client_data = recv_headers(
                self.request, self.server.header_timeout
            )
            first_line = header.split(b"\r\n", 1)[0]
            parts = first_line.split(b" ")
            if len(parts) != 3 or parts[2] not in (b"HTTP/1.0", b"HTTP/1.1"):
                raise ValueError("invalid HTTP proxy request line")
            try:
                method = parts[0].decode("ascii")
                target = parts[1].decode("ascii")
            except UnicodeDecodeError as exc:
                raise ValueError("HTTP proxy request line must be ASCII") from exc
            if method.upper() != "CONNECT":
                self._send_error(b"405 Method Not Allowed")
                return
            host, port = parse_connect_authority(target)
        except (TimeoutError, ValueError):
            self._send_error(b"400 Bad Request")
            return
        except OSError:
            self._send_error(b"400 Bad Request")
            return

        try:
            upstream, buffered_upstream_data = connect_with_policy(
                host,
                port,
                policy=self.server.proxy_policy,
                clash_port=self.server.clash_port,
                proxy_timeout=self.server.proxy_connect_timeout,
                direct_timeout=self.server.direct_connect_timeout,
            )
        except (OSError, ValueError):
            self._send_error(b"502 Bad Gateway")
            return

        try:
            relay(
                self.request,
                upstream,
                initial_to_left=(
                    b"HTTP/1.1 200 Connection Established\r\n"
                    b"Proxy-Agent: Codex-Router-Adaptive\r\n\r\n"
                    + buffered_upstream_data
                ),
                initial_to_right=buffered_client_data,
                max_buffer_bytes=self.server.max_buffer_bytes,
                idle_timeout=self.server.idle_timeout,
            )
        except OSError:
            pass
        finally:
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


class AdaptiveProxyServer(BoundedThreadingTCPServer):
    def __init__(
        self,
        address: tuple[str, int],
        clash_port: int,
        *,
        proxy_policy: str = "prefer",
        max_connections: int = DEFAULT_MAX_CONNECTIONS,
        backlog: int = DEFAULT_BACKLOG,
        max_buffer_bytes: int = DEFAULT_MAX_BUFFER_BYTES,
        header_timeout: float = 15.0,
        proxy_connect_timeout: float = 3.0,
        direct_connect_timeout: float = 8.0,
        idle_timeout: float = 0,
    ):
        if proxy_policy not in ("required", "prefer", "direct"):
            raise ValueError("proxy_policy must be required, prefer, or direct")
        if max_buffer_bytes < BUFFER_SIZE:
            raise ValueError("max_buffer_bytes must be at least BUFFER_SIZE")
        if (
            header_timeout <= 0
            or proxy_connect_timeout <= 0
            or direct_connect_timeout <= 0
            or idle_timeout < 0
        ):
            raise ValueError("timeouts must be positive, except idle_timeout may be zero")
        self.clash_port = clash_port
        self.proxy_policy = proxy_policy
        self.max_buffer_bytes = max_buffer_bytes
        self.header_timeout = header_timeout
        self.proxy_connect_timeout = proxy_connect_timeout
        self.direct_connect_timeout = direct_connect_timeout
        self.idle_timeout = idle_timeout
        super().__init__(
            address,
            AdaptiveProxyHandler,
            max_connections=max_connections,
            backlog=backlog,
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen-port", type=int, default=17897)
    parser.add_argument("--clash-port", type=int, default=7897)
    parser.add_argument(
        "--proxy-policy",
        choices=("required", "prefer", "direct"),
        default="prefer",
    )
    parser.add_argument("--max-connections", type=int, default=DEFAULT_MAX_CONNECTIONS)
    parser.add_argument("--backlog", type=int, default=DEFAULT_BACKLOG)
    parser.add_argument("--max-buffer-bytes", type=int, default=DEFAULT_MAX_BUFFER_BYTES)
    parser.add_argument("--header-timeout", type=float, default=15.0)
    parser.add_argument("--proxy-connect-timeout", type=float, default=3.0)
    parser.add_argument("--direct-connect-timeout", type=float, default=8.0)
    parser.add_argument(
        "--idle-timeout",
        type=float,
        default=0,
        help="idle tunnel timeout in seconds; 0 disables application-level expiry",
    )
    args = parser.parse_args()

    with AdaptiveProxyServer(
        ("127.0.0.1", args.listen_port),
        args.clash_port,
        proxy_policy=args.proxy_policy,
        max_connections=args.max_connections,
        backlog=args.backlog,
        max_buffer_bytes=args.max_buffer_bytes,
        header_timeout=args.header_timeout,
        proxy_connect_timeout=args.proxy_connect_timeout,
        direct_connect_timeout=args.direct_connect_timeout,
        idle_timeout=args.idle_timeout,
    ) as server:
        server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()
