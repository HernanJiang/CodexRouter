import ctypes
import json
import pathlib
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib
from ctypes import wintypes


SCRIPT = pathlib.Path(__file__).with_name("Sync-CCSwitchConfig.py")
PROVIDER_ID = "router-provider"


class DataBlob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]


def dpapi_unprotect(data: bytes) -> bytes:
    source = (ctypes.c_ubyte * len(data)).from_buffer_copy(data)
    source_blob = DataBlob(len(data), ctypes.cast(source, ctypes.POINTER(ctypes.c_ubyte)))
    plain_blob = DataBlob()
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    crypt32.CryptUnprotectData.argtypes = [
        ctypes.POINTER(DataBlob),
        ctypes.c_void_p,
        ctypes.POINTER(DataBlob),
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(DataBlob),
    ]
    crypt32.CryptUnprotectData.restype = wintypes.BOOL
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    if not crypt32.CryptUnprotectData(
        ctypes.byref(source_blob), None, None, None, None, 1, ctypes.byref(plain_blob)
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return ctypes.string_at(plain_blob.pbData, plain_blob.cbData)
    finally:
        kernel32.LocalFree(plain_blob.pbData)


def restore_backup(path: pathlib.Path) -> bytes:
    protected = path.read_bytes()
    if not protected.startswith(b"CRCCBKP1"):
        raise AssertionError("Backup magic is invalid")
    offset = 8
    compressed = bytearray()
    while True:
        length = struct.unpack_from("<I", protected, offset)[0]
        offset += 4
        if length == 0:
            break
        compressed.extend(dpapi_unprotect(protected[offset : offset + length]))
        offset += length
    return zlib.decompress(bytes(compressed), wbits=31)


class SyncCCSwitchConfigTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp_dir.name)
        self.db_path = self.root / "cc-switch.db"
        self.config_path = self.root / "config.toml"
        self.auth_path = self.root / "auth.json"
        self.backup_dir = self.root / "backups"
        self.settings_path = self.root / "settings.json"
        self.catalog_path = self.root / "model-catalog.json"

        connection = sqlite3.connect(self.db_path)
        try:
            connection.execute(
                "CREATE TABLE providers ("
                "id TEXT PRIMARY KEY, "
                "app_type TEXT NOT NULL, "
                "is_current INTEGER NOT NULL DEFAULT 0, "
                "settings_config TEXT NOT NULL, "
                "category TEXT"
                ")"
            )
            connection.execute(
                "INSERT INTO providers "
                "(id, app_type, is_current, settings_config) "
                "VALUES (?, 'codex', 0, ?)",
                (
                    PROVIDER_ID,
                    json.dumps(
                        {"config": "old", "auth": {"old": True}, "keep": 1}
                    ),
                ),
            )
            connection.execute(
                "INSERT INTO providers "
                "(id, app_type, is_current, settings_config) "
                "VALUES ('other-provider', 'codex', 1, '{}')"
            )
            connection.execute("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT)")
            connection.execute(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, payload TEXT NOT NULL)"
            )
            connection.execute(
                "INSERT INTO settings (key, value) VALUES "
                "('current_profile_id_codex', 'active-group')"
            )
            connection.execute(
                "INSERT INTO profiles (id, payload) VALUES (?, ?)",
                (
                    "active-group",
                    json.dumps({"providers": {"codex": "other-provider"}}),
                ),
            )
            connection.commit()
        finally:
            connection.close()

        self.auth_path.write_text(
            json.dumps({"auth_mode": "chatgpt", "tokens": {}}),
            encoding="utf-8",
        )
        self.settings_path.write_text(
            json.dumps(
                {
                    "preserveCodexOfficialAuthOnSwitch": False,
                    "unifyCodexSessionHistory": False,
                    "unifyCodexMigrateExisting": False,
                    "currentProviderCodex": "other-provider",
                    "keep": True,
                }
            ),
            encoding="utf-8",
        )
        self.catalog_path.write_text('{"models":[]}', encoding="utf-8")

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def valid_config(
        self,
        *,
        base_url: str = "http://127.0.0.1:18080/v1",
        top_sections: str = "",
        bearer_line: str = 'experimental_bearer_token = "local-test-token"\n',
    ) -> str:
        return (
            'model_provider = "custom"\n'
            'model = "gpt-5.6-sol"\n'
            f"model_catalog_json = {json.dumps(str(self.catalog_path))}\n\n"
            f"{top_sections}"
            "[model_providers.custom]\n"
            'name = "Codex-Router"\n'
            f'base_url = "{base_url}"\n'
            "requires_openai_auth = true\n"
            f"{bearer_line}"
            "supports_websockets = false\n"
        )

    def run_sync(
        self,
        *,
        activate: bool = False,
        base_url: str = "http://127.0.0.1:18080",
        require_inactive: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        command = [
            sys.executable,
            str(SCRIPT),
            "--db",
            str(self.db_path),
            "--provider-id",
            PROVIDER_ID,
            "--config",
            str(self.config_path),
            "--auth",
            str(self.auth_path),
            "--backup-dir",
            str(self.backup_dir),
            "--settings",
            str(self.settings_path),
            "--base-url",
            base_url,
        ]
        if activate:
            command.append("--activate")
        if require_inactive:
            command.append("--require-inactive")
        return subprocess.run(
            command,
            capture_output=True,
            text=True,
            check=False,
        )

    def read_settings(self) -> dict:
        connection = sqlite3.connect(self.db_path)
        try:
            row = connection.execute(
                "SELECT settings_config FROM providers WHERE id = ?",
                (PROVIDER_ID,),
            ).fetchone()
        finally:
            connection.close()
        return json.loads(row[0])

    def read_category(self) -> str | None:
        connection = sqlite3.connect(self.db_path)
        try:
            row = connection.execute(
                "SELECT category FROM providers WHERE id = ?", (PROVIDER_ID,)
            ).fetchone()
        finally:
            connection.close()
        return row[0]

    def read_cc_settings(self) -> dict:
        return json.loads(self.settings_path.read_text(encoding="utf-8"))

    def read_current_states(self) -> dict[str, int]:
        connection = sqlite3.connect(self.db_path)
        try:
            rows = connection.execute(
                "SELECT id, is_current FROM providers WHERE app_type = 'codex'"
            ).fetchall()
        finally:
            connection.close()
        return dict(rows)

    def test_synchronizes_valid_custom_provider_config(self) -> None:
        config = self.valid_config()
        self.config_path.write_text(config, encoding="utf-8")

        result = self.run_sync()

        self.assertEqual(result.returncode, 0, result.stderr)
        settings = self.read_settings()
        self.assertIn('[windows]\nsandbox = "unelevated"', settings["config"])
        self.assertIn(
            '[desktop]\nenabled-reasoning-efforts = ["low","medium","high","xhigh","ultra","max"]',
            settings["config"],
        )
        self.assertEqual(self.config_path.read_text(encoding="utf-8"), config)
        self.assertEqual(settings["auth"]["auth_mode"], "chatgpt")
        self.assertEqual(settings["keep"], 1)
        self.assertTrue(settings["codexRouter"]["managed"])
        self.assertEqual(self.read_category(), "third_party")
        backups = list(self.backup_dir.glob("*.db.gz.dpapi"))
        self.assertEqual(len(backups), 1)
        self.assertTrue(backups[0].read_bytes().startswith(b"CRCCBKP1"))
        self.assertTrue(restore_backup(backups[0]).startswith(b"SQLite format 3\x00"))
        self.assertEqual(list(self.backup_dir.glob("*.tmp")), [])
        self.assertEqual(
            self.read_current_states(),
            {PROVIDER_ID: 0, "other-provider": 1},
        )
        cc_settings = self.read_cc_settings()
        self.assertTrue(cc_settings["preserveCodexOfficialAuthOnSwitch"])
        self.assertTrue(cc_settings["unifyCodexSessionHistory"])
        self.assertTrue(cc_settings["unifyCodexMigrateExisting"])
        self.assertEqual(cc_settings["currentProviderCodex"], "other-provider")
        self.assertTrue(cc_settings["keep"])

    def test_replaces_elevated_windows_sandbox_before_sync(self) -> None:
        self.config_path.write_text(
            self.valid_config(
                top_sections='[windows]\nsandbox = "elevated"\n\n'
            ),
            encoding="utf-8",
        )

        result = self.run_sync()

        self.assertEqual(result.returncode, 0, result.stderr)
        synchronized = self.read_settings()["config"]
        self.assertIn('sandbox = "unelevated"', synchronized)
        self.assertNotIn('sandbox = "elevated"', synchronized)
        self.assertIn('sandbox = "elevated"', self.config_path.read_text(encoding="utf-8"))

    def test_semantically_current_global_settings_are_not_rewritten(self) -> None:
        original = (
            b'{"preserveCodexOfficialAuthOnSwitch":true,'
            b'"unifyCodexSessionHistory":true,'
            b'"unifyCodexMigrateExisting":true,'
            b'"currentProviderCodex":"other-provider","keep":true}\n'
        )
        self.settings_path.write_bytes(original)
        self.config_path.write_text(self.valid_config(), encoding="utf-8")

        result = self.run_sync()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.settings_path.read_bytes(), original)

    def test_rejects_reserved_openai_provider_override(self) -> None:
        self.config_path.write_text(
            'model_provider = "openai"\n\n'
            "[model_providers.openai]\n"
            'base_url = "http://127.0.0.1:18081/v1"\n',
            encoding="utf-8",
        )

        result = self.run_sync()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Reserved Codex provider", result.stderr)
        self.assertEqual(self.read_settings()["config"], "old")
        self.assertEqual(list(self.backup_dir.glob("*.dpapi")), [])
        self.assertEqual(
            self.read_current_states(),
            {PROVIDER_ID: 0, "other-provider": 1},
        )

    def test_activate_updates_legacy_and_group_current_state(self) -> None:
        self.config_path.write_text(self.valid_config(), encoding="utf-8")

        result = self.run_sync(activate=True)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.read_current_states(),
            {PROVIDER_ID: 1, "other-provider": 0},
        )
        connection = sqlite3.connect(self.db_path)
        try:
            payload = json.loads(
                connection.execute(
                    "SELECT payload FROM profiles WHERE id = 'active-group'"
                ).fetchone()[0]
            )
        finally:
            connection.close()
        self.assertEqual(payload["providers"]["codex"], PROVIDER_ID)
        self.assertEqual(self.read_cc_settings()["currentProviderCodex"], PROVIDER_ID)

    def test_rejects_legacy_auth_adapter_configuration(self) -> None:
        self.config_path.write_text(
            self.valid_config(base_url="http://127.0.0.1:18081/v1"),
            encoding="utf-8",
        )

        result = self.run_sync()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("configured local port 18080", result.stderr)
        self.assertEqual(self.read_settings()["config"], "old")
        self.assertEqual(list(self.backup_dir.glob("*.dpapi")), [])

    def test_accepts_a_configured_alternative_local_port(self) -> None:
        self.config_path.write_text(
            self.valid_config(base_url="http://localhost:19090/v1"),
            encoding="utf-8",
        )

        result = self.run_sync(base_url="http://127.0.0.1:19090")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("localhost:19090/v1", self.read_settings()["config"])

    def test_rejects_missing_local_bearer_without_a_backup(self) -> None:
        self.config_path.write_text(
            self.valid_config(bearer_line=""), encoding="utf-8"
        )

        result = self.run_sync()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("local bearer token", result.stderr)
        self.assertEqual(self.read_settings()["config"], "old")
        self.assertEqual(list(self.backup_dir.glob("*.dpapi")), [])

    def test_require_inactive_rejects_an_active_target_without_a_backup(self) -> None:
        connection = sqlite3.connect(self.db_path)
        try:
            connection.execute(
                "UPDATE providers SET is_current = CASE WHEN id = ? THEN 1 ELSE 0 END",
                (PROVIDER_ID,),
            )
            connection.commit()
        finally:
            connection.close()
        self.config_path.write_text(self.valid_config(), encoding="utf-8")

        result = self.run_sync()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("target provider is active", result.stderr)
        self.assertEqual(self.read_settings()["config"], "old")
        self.assertEqual(list(self.backup_dir.glob("*.dpapi")), [])


if __name__ == "__main__":
    unittest.main()
