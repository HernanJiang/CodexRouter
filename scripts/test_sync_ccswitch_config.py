import json
import pathlib
import sqlite3
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("Sync-CCSwitchConfig.py")
PROVIDER_ID = "router-provider"


class SyncCCSwitchConfigTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp_dir.name)
        self.db_path = self.root / "cc-switch.db"
        self.config_path = self.root / "config.toml"
        self.auth_path = self.root / "auth.json"
        self.backup_dir = self.root / "backups"

        connection = sqlite3.connect(self.db_path)
        try:
            connection.execute(
                "CREATE TABLE providers ("
                "id TEXT PRIMARY KEY, "
                "app_type TEXT NOT NULL, "
                "is_current INTEGER NOT NULL DEFAULT 0, "
                "settings_config TEXT NOT NULL"
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
            connection.commit()
        finally:
            connection.close()

        self.auth_path.write_text(
            json.dumps({"auth_mode": "chatgpt", "tokens": {}}),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def run_sync(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
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
            ],
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
        config = (
            'model_provider = "sub2api"\n'
            'model = "deepseek-v4-flash"\n\n'
            "[model_providers.sub2api]\n"
            'base_url = "http://127.0.0.1:18081/v1"\n'
            "requires_openai_auth = true\n"
            "supports_websockets = false\n"
        )
        self.config_path.write_text(config, encoding="utf-8")

        result = self.run_sync()

        self.assertEqual(result.returncode, 0, result.stderr)
        settings = self.read_settings()
        self.assertEqual(settings["config"], config)
        self.assertEqual(settings["auth"]["auth_mode"], "chatgpt")
        self.assertEqual(settings["keep"], 1)
        self.assertEqual(len(list(self.backup_dir.glob("*.db"))), 1)
        self.assertEqual(
            self.read_current_states(),
            {PROVIDER_ID: 0, "other-provider": 1},
        )

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
        self.assertEqual(list(self.backup_dir.glob("*.db")), [])
        self.assertEqual(
            self.read_current_states(),
            {PROVIDER_ID: 0, "other-provider": 1},
        )


if __name__ == "__main__":
    unittest.main()
