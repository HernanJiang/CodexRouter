import unittest

from scripts.codex_auth_adapter import AdapterError, rewrite_request_head


class RewriteRequestHeadTests(unittest.TestCase):
    def test_replaces_oauth_bearer_and_forces_connection_close(self) -> None:
        request = (
            b"POST /v1/responses HTTP/1.1\r\n"
            b"Host: 127.0.0.1:18081\r\n"
            b"Authorization: Bearer chatgpt-oauth-token\r\n"
            b"Connection: keep-alive\r\n\r\n"
        )

        rewritten = rewrite_request_head(request, "local-sub2api-key")

        self.assertIn(
            b"Authorization: Bearer local-sub2api-key\r\n",
            rewritten,
        )
        self.assertNotIn(b"chatgpt-oauth-token", rewritten)
        self.assertIn(b"Connection: close\r\n", rewritten)

    def test_adds_authorization_when_missing(self) -> None:
        request = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n"

        rewritten = rewrite_request_head(request, "local-sub2api-key")

        self.assertEqual(
            rewritten.count(b"Authorization: Bearer local-sub2api-key"),
            1,
        )

    def test_rejects_header_injection_in_key(self) -> None:
        request = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n"

        with self.assertRaises(AdapterError):
            rewrite_request_head(request, "key\r\nX-Injected: yes")

    def test_preserves_websocket_upgrade(self) -> None:
        request = (
            b"GET /v1/responses HTTP/1.1\r\n"
            b"Host: 127.0.0.1:18081\r\n"
            b"Connection: Upgrade\r\n"
            b"Upgrade: websocket\r\n"
            b"Authorization: Bearer chatgpt-oauth-token\r\n\r\n"
        )

        rewritten = rewrite_request_head(request, "local-sub2api-key")

        self.assertIn(b"Connection: Upgrade\r\n", rewritten)
        self.assertIn(b"Upgrade: websocket\r\n", rewritten)
        self.assertNotIn(b"chatgpt-oauth-token", rewritten)


if __name__ == "__main__":
    unittest.main()
