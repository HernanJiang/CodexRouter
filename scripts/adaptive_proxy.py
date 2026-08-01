import argparse
import select
import socket
import socketserver


MAX_HEADER_BYTES = 65536


def recv_headers(connection: socket.socket) -> bytes:
    data = bytearray()
    while b"\r\n\r\n" not in data:
        chunk = connection.recv(4096)
        if not chunk:
            break
        data.extend(chunk)
        if len(data) > MAX_HEADER_BYTES:
            raise ValueError("HTTP proxy header is too large")
    return bytes(data)


def connect_ipv4(host: str, port: int, timeout: float = 8.0) -> socket.socket:
    errors = []
    for family, socktype, protocol, _, address in socket.getaddrinfo(
        host, port, socket.AF_INET, socket.SOCK_STREAM
    ):
        upstream = socket.socket(family, socktype, protocol)
        upstream.settimeout(timeout)
        try:
            upstream.connect(address)
            upstream.settimeout(None)
            return upstream
        except OSError as exc:
            errors.append(exc)
            upstream.close()
    if errors:
        raise errors[-1]
    raise OSError(f"No IPv4 address found for {host}")


def connect_through_clash(host: str, port: int, clash_port: int) -> socket.socket:
    upstream = socket.create_connection(("127.0.0.1", clash_port), timeout=1.0)
    try:
        request = (
            f"CONNECT {host}:{port} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Proxy-Connection: Keep-Alive\r\n\r\n"
        ).encode("ascii")
        upstream.sendall(request)
        response = recv_headers(upstream)
        status_line = response.split(b"\r\n", 1)[0]
        if b" 200 " not in status_line:
            raise OSError(f"Clash CONNECT failed: {status_line[:100]!r}")
        upstream.settimeout(None)
        return upstream
    except Exception:
        upstream.close()
        raise


def relay(left: socket.socket, right: socket.socket) -> None:
    sockets = [left, right]
    while True:
        readable, _, exceptional = select.select(sockets, [], sockets, 300)
        if exceptional or not readable:
            return
        for source in readable:
            destination = right if source is left else left
            data = source.recv(65536)
            if not data:
                return
            destination.sendall(data)


class AdaptiveProxyHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        self.request.settimeout(15)
        try:
            header = recv_headers(self.request)
            if not header:
                return
            first_line = header.split(b"\r\n", 1)[0].decode("latin-1")
            method, target, _ = first_line.split(" ", 2)
            if method.upper() != "CONNECT":
                self.request.sendall(
                    b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n"
                )
                return

            host, separator, port_text = target.rpartition(":")
            if not separator:
                host, port_text = target, "443"
            port = int(port_text)

            try:
                upstream = connect_through_clash(host, port, self.server.clash_port)
            except OSError:
                upstream = connect_ipv4(host, port)
        except (OSError, ValueError):
            try:
                self.request.sendall(
                    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n"
                )
            except OSError:
                pass
            return

        try:
            self.request.sendall(
                b"HTTP/1.1 200 Connection Established\r\n"
                b"Proxy-Agent: Codex-Router-Adaptive\r\n\r\n"
            )
            self.request.settimeout(None)
            relay(self.request, upstream)
        except OSError:
            pass
        finally:
            upstream.close()


class AdaptiveProxyServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, address: tuple[str, int], clash_port: int):
        super().__init__(address, AdaptiveProxyHandler)
        self.clash_port = clash_port


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen-port", type=int, default=17897)
    parser.add_argument("--clash-port", type=int, default=7897)
    args = parser.parse_args()

    with AdaptiveProxyServer(("127.0.0.1", args.listen_port), args.clash_port) as server:
        server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()
