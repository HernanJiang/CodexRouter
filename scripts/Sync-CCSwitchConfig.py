import argparse
import datetime as dt
import json
import pathlib
import re
import sqlite3


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", required=True)
    parser.add_argument("--provider-id", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--auth", required=True)
    parser.add_argument("--backup-dir", required=True)
    args = parser.parse_args()

    db_path = pathlib.Path(args.db)
    config_path = pathlib.Path(args.config)
    auth_path = pathlib.Path(args.auth)
    backup_dir = pathlib.Path(args.backup_dir)
    backup_dir.mkdir(parents=True, exist_ok=True)

    timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    backup_path = backup_dir / f"cc-switch-before-codex-router-{timestamp}.db"
    config_text = config_path.read_text(encoding="utf-8-sig")
    reserved_provider = re.search(
        r"(?m)^\s*\[model_providers\.(openai|ollama|lmstudio)(?:[.\]])",
        config_text,
    )
    if reserved_provider:
        raise RuntimeError(
            f"Reserved Codex provider cannot be synchronized: "
            f"{reserved_provider.group(1)}"
        )
    auth = json.loads(auth_path.read_text(encoding="utf-8-sig"))
    if auth.get("auth_mode") != "chatgpt" or not isinstance(auth.get("tokens"), dict):
        raise RuntimeError("Current Codex auth state is not a ChatGPT login")

    connection = sqlite3.connect(db_path, timeout=30)
    try:
        backup = sqlite3.connect(backup_path)
        try:
            connection.backup(backup)
        finally:
            backup.close()

        row = connection.execute(
            "SELECT settings_config FROM providers WHERE id = ? AND app_type = 'codex'",
            (args.provider_id,),
        ).fetchone()
        if row is None:
            raise RuntimeError(f"CC Switch Codex provider not found: {args.provider_id}")

        settings = json.loads(row[0])
        settings["auth"] = auth
        settings["config"] = config_text

        connection.execute("BEGIN IMMEDIATE")
        connection.execute(
            "UPDATE providers SET settings_config = ? "
            "WHERE id = ? AND app_type = 'codex'",
            (json.dumps(settings, ensure_ascii=False, separators=(",", ":")), args.provider_id),
        )
        connection.commit()
    except Exception:
        connection.rollback()
        raise
    finally:
        connection.close()

    print(f"CC Switch provider synchronized; backup: {backup_path}")


if __name__ == "__main__":
    main()
