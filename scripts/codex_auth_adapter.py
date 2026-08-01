import argparse
import select
import socket
import socketserver
import subprocess
from pathlib import Path


MAX_HEADER_BYTES = 65536
BUFFER_SIZE = 65536


class AdapterError(Exception):
    pass


def receive_request_head(connection: socket.socket) -> tuple[bytes, bytes]:
    data = bytearray()
    while b"\r\n\r\n" not in data:
        chunk = connection.recv(4096)
        if not chunk:
            break
        data.extend(chunk)
        if len(data) > MAX_HEADER_BYTES:
            raise AdapterError("request headers are too large")

    marker = data.find(b"\r\n\r\n")
    if marker < 0:
        raise AdapterError("incomplete request headers")
    return bytes(data[: marker + 4]), bytes(data[marker + 4 :])


def rewrite_request_head(request_head: bytes, api_key: str) -> bytes:
    try:
        api_key_bytes = api_key.encode("ascii")
    except UnicodeEncodeError as exc:
        raise AdapterError("local API key must be ASCII") from exc
    if not api_key_bytes or b"\r" in api_key_bytes or b"\n" in api_key_bytes:
        raise AdapterError("local API key is invalid")

    lines = request_head[:-4].split(b"\r\n")
    if not lines or len(lines[0].split(b" ", 2)) != 3:
        raise AdapterError("invalid HTTP request line")

    has_websocket_upgrade = any(
        line.partition(b":")[0].strip().lower() == b"upgrade"
        and line.partition(b":")[2].strip().lower() == b"websocket"
        for line in lines[1:]
    )
    rewritten = [lines[0]]
    has_authorization = False
    has_connection = False
    for line in lines[1:]:
        name, separator, _ = line.partition(b":")
        if not separator:
            raise AdapterError("invalid HTTP header")
        lower_name = name.strip().lower()
        if lower_name == b"authorization":
            rewritten.append(b"Authorization: Bearer " + api_key_bytes)
            has_authorization = True
        elif lower_name == b"proxy-authorization":
            continue
        elif lower_name == b"connection":
            rewritten.append(
                line if has_websocket_upgrade else b"Connection: close"
            )
            has_connection = True
        else:
            rewritten.append(line)

    if not has_authorization:
        rewritten.append(b"Authorization: Bearer " + api_key_bytes)
    if not has_connection:
        rewritten.append(
            b"Connection: Upgrade"
            if has_websocket_upgrade
            else b"Connection: close"
        )
    return b"\r\n".join(rewritten) + b"\r\n\r\n"


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
    if not api_key or "\r" in api_key or "\n" in api_key:
        raise AdapterError("local API key output is invalid")
    return api_key


def relay(left: socket.socket, right: socket.socket) -> None:
    sockets = [left, right]
    while True:
        readable, _, exceptional = select.select(sockets, [], sockets, 300)
        if exceptional or not readable:
            return
        for source in readable:
            destination = right if source is left else left
            data = source.recv(BUFFER_SIZE)
            if not data:
                return
            destination.sendall(data)


class CodexAuthAdapterHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        if self.client_address[0] != "127.0.0.1":
            return

        upstream = None
        self.request.settimeout(15)
        try:
            request_head, buffered_body = receive_request_head(self.request)
            api_key = read_local_api_key(
                self.server.powershell,
                self.server.credential_script,
            )
            rewritten_head = rewrite_request_head(request_head, api_key)
            upstream = socket.create_connection(
                (self.server.upstream_host, self.server.upstream_port),
                timeout=5,
            )
            upstream.sendall(rewritten_head)
            if buffered_body:
                upstream.sendall(buffered_body)
            self.request.settimeout(None)
            upstream.settimeout(None)
            relay(self.request, upstream)
        except AdapterError:
            self._send_error(b"503 Service Unavailable")
        except OSError:
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


class CodexAuthAdapterServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        upstream_host: str,
        upstream_port: int,
        powershell: Path,
        credential_script: Path,
    ):
        super().__init__(address, CodexAuthAdapterHandler)
        self.upstream_host = upstream_host
        self.upstream_port = upstream_port
        self.powershell = powershell
        self.credential_script = credential_script


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
    args = parser.parse_args()

    with CodexAuthAdapterServer(
        ("127.0.0.1", args.listen_port),
        "127.0.0.1",
        args.upstream_port,
        args.powershell,
        args.credential_script,
    ) as server:
        server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()
