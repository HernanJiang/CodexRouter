import os
import socket
import socketserver
import sys
import threading
import time
import unittest
from pathlib import Path
from unittest import mock


sys.dont_write_bytecode = True
SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import adaptive_proxy
import codex_auth_adapter


def receive_until(connection: socket.socket, marker: bytes, timeout: float = 3) -> bytes:
    connection.settimeout(timeout)
    data = bytearray()
    while marker not in data:
        chunk = connection.recv(4096)
        if not chunk:
            break
        data.extend(chunk)
    return bytes(data)


def receive_all(connection: socket.socket, timeout: float = 3) -> bytes:
    connection.settimeout(timeout)
    data = bytearray()
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            return bytes(data)
        data.extend(chunk)


class RunningServer:
    def __init__(self, server: socketserver.BaseServer):
        self.server = server
        self.thread = threading.Thread(target=server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self.server

    def __exit__(self, exc_type, exc, traceback):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(3)


class KeepAliveUpstreamHandler(socketserver.BaseRequestHandler):
    def handle(self):
        with self.server.record_lock:
            self.server.connection_count += 1
        pending = bytearray()
        while True:
            while b"\r\n\r\n" not in pending:
                chunk = self.request.recv(4096)
                if not chunk:
                    return
                pending.extend(chunk)
            marker = pending.find(b"\r\n\r\n") + 4
            head = bytes(pending[:marker])
            del pending[:marker]
            headers = {}
            for line in head[:-4].split(b"\r\n")[1:]:
                name, _, value = line.partition(b":")
                headers.setdefault(name.strip().lower(), []).append(value.strip())
            content_length = int(headers.get(b"content-length", [b"0"])[0])
            while len(pending) < content_length:
                chunk = self.request.recv(4096)
                if not chunk:
                    return
                pending.extend(chunk)
            body = bytes(pending[:content_length])
            del pending[:content_length]
            with self.server.record_lock:
                self.server.requests.append((head, body))
                request_number = len(self.server.requests)
            payload = f"ok-{request_number}".encode("ascii")
            self.request.sendall(
                b"HTTP/1.1 200 OK\r\nContent-Length: "
                + str(len(payload)).encode("ascii")
                + b"\r\nConnection: keep-alive\r\n\r\n"
                + payload
            )


class EarlyDataProxyHandler(socketserver.BaseRequestHandler):
    def handle(self):
        head, tail = adaptive_proxy.recv_headers(self.request, timeout=2)
        self.server.connect_head = head
        self.server.connect_tail = tail
        self.request.sendall(
            b"HTTP/1.1 200 Connection Established\r\nX-Test: yes\r\n\r\n"
            b"SERVER_EARLY"
        )
        data = receive_all(self.request, timeout=3)
        self.server.tunnel_data = data
        self.request.sendall(b"ECHO:" + data)
        self.request.shutdown(socket.SHUT_WR)
        self.server.completed.set()


class HoldingProxyHandler(socketserver.BaseRequestHandler):
    def handle(self):
        adaptive_proxy.recv_headers(self.request, timeout=2)
        self.request.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        self.server.connected.set()
        self.server.release.wait(3)


class HoldingHttpHandler(socketserver.BaseRequestHandler):
    def handle(self):
        receive_until(self.request, b"\r\n\r\n", timeout=2)
        self.server.connected.set()
        self.server.release.wait(3)


def make_threading_server(handler):
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), handler)
    server.daemon_threads = True
    server.allow_reuse_address = True
    return server


class AuthAdapterProtocolTests(unittest.TestCase):
    def test_rewrites_every_keep_alive_request_on_one_upstream_connection(self):
        upstream = make_threading_server(KeepAliveUpstreamHandler)
        upstream.requests = []
        upstream.connection_count = 0
        upstream.record_lock = threading.Lock()
        with RunningServer(upstream):
            adapter = codex_auth_adapter.CodexAuthAdapterServer(
                ("127.0.0.1", 0),
                "127.0.0.1",
                upstream.server_address[1],
                api_key="router-test-key",
            )
            with RunningServer(adapter):
                client = socket.create_connection(adapter.server_address, timeout=2)
                try:
                    client.sendall(
                        b"GET /one HTTP/1.1\r\nHost: local\r\n"
                        b"Authorization: Bearer client-secret-one\r\n"
                        b"Connection: keep-alive, X-Hop\r\n"
                        b"X-Hop: remove-me\r\n\r\n"
                    )
                    first = receive_until(client, b"ok-1")
                    self.assertIn(b"ok-1", first)
                    client.sendall(
                        b"POST /two HTTP/1.1\r\nHost: local\r\n"
                        b"Authorization: Basic client-secret-two\r\n"
                        b"Content-Length: 5\r\nConnection: close\r\n\r\nhello"
                    )
                    second = receive_until(client, b"ok-2")
                    self.assertIn(b"ok-2", second)
                finally:
                    client.close()

        self.assertEqual(upstream.connection_count, 1)
        self.assertEqual(len(upstream.requests), 2)
        for head, _ in upstream.requests:
            self.assertEqual(head.count(b"Authorization: Bearer router-test-key"), 1)
            self.assertNotIn(b"client-secret", head)
        self.assertNotIn(b"X-Hop:", upstream.requests[0][0])
        self.assertNotIn(b"Connection: keep-alive", upstream.requests[0][0])
        self.assertEqual(upstream.requests[1][1], b"hello")

    def test_fixed_and_chunked_bodies_preserve_framing_across_pipelining(self):
        rewriter = codex_auth_adapter.RequestStreamRewriter("test-key")
        source = (
            b"POST /fixed HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello"
            b"POST /chunked HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n"
            b"3\r\nabc\r\n0\r\nX-End: yes\r\n\r\n"
            b"GET /last HTTP/1.1\r\nHost: h\r\n\r\n"
        )
        pieces = [rewriter.feed(source[index : index + 7]) for index in range(0, len(source), 7)]
        rewriter.finish()
        rewritten = b"".join(pieces)
        self.assertEqual(rewritten.count(b"Authorization: Bearer test-key"), 3)
        self.assertIn(b"\r\n\r\nhelloPOST /chunked", rewritten)
        self.assertIn(b"3\r\nabc\r\n0\r\nX-End: yes\r\n\r\nGET /last", rewritten)

    def test_rejects_request_smuggling_framing(self):
        cases = (
            b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: h\r\nConnection: Content-Length\r\nContent-Length: 1\r\n\r\n",
        )
        for request in cases:
            with self.subTest(request=request):
                with self.assertRaises(codex_auth_adapter.AdapterError):
                    codex_auth_adapter.rewrite_request_head(request, "test-key")

    def test_rejects_data_after_a_connection_close_request(self):
        rewriter = codex_auth_adapter.RequestStreamRewriter("test-key")
        with self.assertRaises(codex_auth_adapter.AdapterError):
            rewriter.feed(
                b"GET /last HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n"
                b"GET /must-not-pass HTTP/1.1\r\nHost: h\r\n\r\n"
            )

    def test_credential_script_is_loaded_once_when_server_starts(self):
        with mock.patch.object(
            codex_auth_adapter,
            "read_local_api_key",
            return_value="loaded-once-key",
        ) as loader:
            server = codex_auth_adapter.CodexAuthAdapterServer(
                ("127.0.0.1", 0),
                "127.0.0.1",
                1,
                Path("powershell.exe"),
                Path("credential.ps1"),
            )
            server.server_close()
        loader.assert_called_once_with(Path("powershell.exe"), Path("credential.ps1"))

    def test_auth_adapter_max_connection_gate_returns_503(self):
        upstream = make_threading_server(HoldingHttpHandler)
        upstream.connected = threading.Event()
        upstream.release = threading.Event()
        with RunningServer(upstream):
            adapter = codex_auth_adapter.CodexAuthAdapterServer(
                ("127.0.0.1", 0),
                "127.0.0.1",
                upstream.server_address[1],
                api_key="router-test-key",
                max_connections=1,
            )
            with RunningServer(adapter):
                first = socket.create_connection(adapter.server_address, timeout=2)
                first.sendall(b"GET /hold HTTP/1.1\r\nHost: local\r\n\r\n")
                self.assertTrue(upstream.connected.wait(2))
                second = socket.create_connection(adapter.server_address, timeout=2)
                response = receive_until(second, b"\r\n\r\n")
                self.assertIn(b"503 Service Unavailable", response)
                second.close()
                first.close()
                upstream.release.set()


class AdaptiveProxyProtocolTests(unittest.TestCase):
    def test_ipv6_and_hostname_authority_validation(self):
        self.assertEqual(
            adaptive_proxy.parse_connect_authority("[2001:db8::1]:443"),
            ("2001:db8::1", 443),
        )
        self.assertEqual(
            adaptive_proxy.parse_connect_authority("api.example.test:8443"),
            ("api.example.test", 8443),
        )
        invalid = (
            "2001:db8::1:443",
            "[2001:db8::1]",
            "[not-ipv6]:443",
            "[fe80::1%bad/scope]:443",
            "host",
            "host:0",
            "host:65536",
            "user@host:443",
            "host name:443",
            "-bad-host:443",
            "bad-host-:443",
        )
        for authority in invalid:
            with self.subTest(authority=authority):
                with self.assertRaises(ValueError):
                    adaptive_proxy.parse_connect_authority(authority)

    def test_header_reader_preserves_bytes_after_connect_headers(self):
        reader, writer = socket.socketpair()
        try:
            writer.sendall(b"CONNECT h:443 HTTP/1.1\r\nHost: h\r\n\r\nEARLY")
            head, tail = adaptive_proxy.recv_headers(reader, timeout=1)
            self.assertTrue(head.endswith(b"\r\n\r\n"))
            self.assertEqual(tail, b"EARLY")
        finally:
            reader.close()
            writer.close()

    def test_proxy_policy_does_not_bypass_an_explicit_rejection(self):
        fake_socket = object()
        with mock.patch.object(
            adaptive_proxy,
            "connect_through_clash",
            side_effect=adaptive_proxy.ProxyUnavailableError("down"),
        ) as through_proxy, mock.patch.object(
            adaptive_proxy, "connect_direct", return_value=fake_socket
        ) as direct:
            result, tail = adaptive_proxy.connect_with_policy(
                "host",
                443,
                policy="prefer",
                clash_port=1,
                proxy_timeout=1,
                direct_timeout=1,
            )
            self.assertIs(result, fake_socket)
            self.assertEqual(tail, b"")
            through_proxy.assert_called_once()
            direct.assert_called_once()

        with mock.patch.object(
            adaptive_proxy,
            "connect_through_clash",
            side_effect=adaptive_proxy.ProxyRejectedError("policy rejected"),
        ), mock.patch.object(adaptive_proxy, "connect_direct") as direct:
            with self.assertRaises(adaptive_proxy.ProxyRejectedError):
                adaptive_proxy.connect_with_policy(
                    "host",
                    443,
                    policy="prefer",
                    clash_port=1,
                    proxy_timeout=1,
                    direct_timeout=1,
                )
            direct.assert_not_called()

        with mock.patch.object(
            adaptive_proxy, "connect_through_clash"
        ) as through_proxy, mock.patch.object(
            adaptive_proxy, "connect_direct", return_value=fake_socket
        ) as direct:
            result, tail = adaptive_proxy.connect_with_policy(
                "host",
                443,
                policy="direct",
                clash_port=1,
                proxy_timeout=1,
                direct_timeout=1,
            )
            self.assertIs(result, fake_socket)
            self.assertEqual(tail, b"")
            through_proxy.assert_not_called()
            direct.assert_called_once()

        with mock.patch.object(
            adaptive_proxy,
            "connect_through_clash",
            side_effect=adaptive_proxy.ProxyUnavailableError("down"),
        ), mock.patch.object(adaptive_proxy, "connect_direct") as direct:
            with self.assertRaises(adaptive_proxy.ProxyUnavailableError):
                adaptive_proxy.connect_with_policy(
                    "host",
                    443,
                    policy="required",
                    clash_port=1,
                    proxy_timeout=1,
                    direct_timeout=1,
                )
            direct.assert_not_called()

    def test_connect_preserves_client_and_upstream_early_data(self):
        clash = make_threading_server(EarlyDataProxyHandler)
        clash.completed = threading.Event()
        clash.connect_head = b""
        clash.connect_tail = b""
        clash.tunnel_data = b""
        with RunningServer(clash):
            proxy = adaptive_proxy.AdaptiveProxyServer(
                ("127.0.0.1", 0),
                clash.server_address[1],
                proxy_policy="required",
            )
            with RunningServer(proxy):
                client = socket.create_connection(proxy.server_address, timeout=2)
                client.sendall(
                    b"CONNECT example.test:443 HTTP/1.1\r\n"
                    b"Host: example.test:443\r\n\r\nCLIENT_EARLY"
                )
                client.shutdown(socket.SHUT_WR)
                response = receive_all(client, timeout=3)
                client.close()
        self.assertTrue(clash.completed.is_set())
        self.assertIn(b"200 Connection Established", response)
        self.assertIn(b"SERVER_EARLY", response)
        self.assertIn(b"ECHO:CLIENT_EARLY", response)
        self.assertEqual(clash.tunnel_data, b"CLIENT_EARLY")

    def test_max_connection_gate_returns_503_without_spawning_another_handler(self):
        clash = make_threading_server(HoldingProxyHandler)
        clash.connected = threading.Event()
        clash.release = threading.Event()
        with RunningServer(clash):
            proxy = adaptive_proxy.AdaptiveProxyServer(
                ("127.0.0.1", 0),
                clash.server_address[1],
                proxy_policy="required",
                max_connections=1,
            )
            with RunningServer(proxy):
                first = socket.create_connection(proxy.server_address, timeout=2)
                first.sendall(b"CONNECT hold.test:443 HTTP/1.1\r\nHost: hold.test\r\n\r\n")
                self.assertIn(b"200 Connection Established", receive_until(first, b"\r\n\r\n"))
                self.assertTrue(clash.connected.wait(2))
                second = socket.create_connection(proxy.server_address, timeout=2)
                response = receive_until(second, b"\r\n\r\n")
                self.assertIn(b"503 Service Unavailable", response)
                second.close()
                first.close()
                clash.release.set()


class RelayProtocolTests(unittest.TestCase):
    def test_half_close_drains_the_other_direction(self):
        for module in (codex_auth_adapter, adaptive_proxy):
            with self.subTest(module=module.__name__):
                client, relay_left = socket.socketpair()
                relay_right, upstream = socket.socketpair()
                thread = threading.Thread(
                    target=module.relay,
                    args=(relay_left, relay_right),
                    kwargs={"max_buffer_bytes": 65536},
                    daemon=True,
                )
                thread.start()
                try:
                    client.sendall(b"request")
                    client.shutdown(socket.SHUT_WR)
                    self.assertEqual(receive_all(upstream), b"request")
                    upstream.sendall(b"response-after-client-fin")
                    upstream.shutdown(socket.SHUT_WR)
                    self.assertEqual(receive_all(client), b"response-after-client-fin")
                    thread.join(3)
                    self.assertFalse(thread.is_alive())
                finally:
                    client.close()
                    relay_left.close()
                    relay_right.close()
                    upstream.close()

    def test_bounded_relay_applies_backpressure_without_data_loss(self):
        payload = os.urandom(2 * 1024 * 1024)
        for module in (codex_auth_adapter, adaptive_proxy):
            with self.subTest(module=module.__name__):
                client, relay_left = socket.socketpair()
                relay_right, upstream = socket.socketpair()
                relay_thread = threading.Thread(
                    target=module.relay,
                    args=(relay_left, relay_right),
                    kwargs={"max_buffer_bytes": 65536},
                    daemon=True,
                )
                relay_thread.start()
                sender = threading.Thread(
                    target=lambda: (
                        client.sendall(payload),
                        client.shutdown(socket.SHUT_WR),
                    ),
                    daemon=True,
                )
                sender.start()
                received = bytearray()
                upstream.settimeout(5)
                while True:
                    chunk = upstream.recv(4096)
                    if not chunk:
                        break
                    received.extend(chunk)
                    if len(received) % (256 * 1024) == 0:
                        time.sleep(0.002)
                upstream.shutdown(socket.SHUT_WR)
                receive_all(client)
                sender.join(3)
                relay_thread.join(3)
                try:
                    self.assertEqual(bytes(received), payload)
                    self.assertFalse(sender.is_alive())
                    self.assertFalse(relay_thread.is_alive())
                finally:
                    client.close()
                    relay_left.close()
                    relay_right.close()
                    upstream.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)

