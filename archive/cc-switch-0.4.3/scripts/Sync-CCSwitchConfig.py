import argparse
import ctypes
import datetime as dt
import json
import os
import pathlib
import re
import sqlite3
import struct
import uuid
import zlib
from ctypes import wintypes
from urllib.parse import urlsplit


DESKTOP_REASONING_EFFORTS = ["low", "medium", "high", "xhigh", "ultra", "max"]
BACKUP_MAGIC = b"CRCCBKP1"
BACKUP_CHUNK_BYTES = 1024 * 1024
BACKUP_MAX_FILES = 2
BACKUP_MAX_TOTAL_BYTES = 128 * 1024 * 1024


class DataBlob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]


def dpapi_protect(data: bytes) -> bytes:
    if os.name != "nt":
        raise RuntimeError("CC Switch backups require Windows DPAPI")
    source = (ctypes.c_ubyte * len(data)).from_buffer_copy(data)
    source_blob = DataBlob(len(data), ctypes.cast(source, ctypes.POINTER(ctypes.c_ubyte)))
    protected_blob = DataBlob()
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    crypt32.CryptProtectData.argtypes = [
        ctypes.POINTER(DataBlob),
        wintypes.LPCWSTR,
        ctypes.POINTER(DataBlob),
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(DataBlob),
    ]
    crypt32.CryptProtectData.restype = wintypes.BOOL
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    if not crypt32.CryptProtectData(
        ctypes.byref(source_blob), None, None, None, None, 1, ctypes.byref(protected_blob)
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return ctypes.string_at(protected_blob.pbData, protected_blob.cbData)
    finally:
        kernel32.LocalFree(protected_blob.pbData)


class DpapiChunkWriter:
    def __init__(self, target) -> None:
        self.target = target
        self.pending = bytearray()
        self.target.write(BACKUP_MAGIC)

    def write(self, data: bytes) -> None:
        self.pending.extend(data)
        while len(self.pending) >= BACKUP_CHUNK_BYTES:
            self._write_chunk(bytes(self.pending[:BACKUP_CHUNK_BYTES]))
            del self.pending[:BACKUP_CHUNK_BYTES]

    def _write_chunk(self, data: bytes) -> None:
        protected = dpapi_protect(data)
        self.target.write(struct.pack("<I", len(protected)))
        self.target.write(protected)

    def finish(self) -> None:
        if self.pending:
            self._write_chunk(bytes(self.pending))
            self.pending.clear()
        self.target.write(struct.pack("<I", 0))
        self.target.flush()
        os.fsync(self.target.fileno())


def protect_compressed_backup(source: pathlib.Path, destination: pathlib.Path) -> None:
    temporary = destination.with_name(f".{destination.name}.{uuid.uuid4().hex}.tmp")
    try:
        with source.open("rb") as input_file, temporary.open("xb") as output_file:
            writer = DpapiChunkWriter(output_file)
            compressor = zlib.compressobj(level=6, wbits=31)
            while chunk := input_file.read(BACKUP_CHUNK_BYTES):
                compressed = compressor.compress(chunk)
                if compressed:
                    writer.write(compressed)
            writer.write(compressor.flush())
            writer.finish()
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def prune_backups(backup_dir: pathlib.Path) -> None:
    backups = sorted(
        backup_dir.glob("cc-switch-before-codex-router-*.db.gz.dpapi"),
        key=lambda path: (path.stat().st_mtime_ns, path.name),
        reverse=True,
    )
    retained_bytes = 0
    for index, path in enumerate(backups):
        size = path.stat().st_size
        if index >= BACKUP_MAX_FILES or retained_bytes + size > BACKUP_MAX_TOTAL_BYTES:
            path.unlink()
        else:
            retained_bytes += size


def normalize_windows_sandbox(config_text: str) -> str:
    section_pattern = re.compile(r"(?ms)^\[windows\]\s*\r?\n.*?(?=^\[|\Z)")
    section_match = section_pattern.search(config_text)
    sandbox_pattern = re.compile(r"(?m)^sandbox\s*=.*$")
    sandbox_line = 'sandbox = "unelevated"'
    if section_match:
        section = section_match.group(0)
        if sandbox_pattern.search(section):
            section = sandbox_pattern.sub(sandbox_line, section, count=1)
        else:
            header_end = section.find("\n") + 1
            section = section[:header_end] + sandbox_line + "\n" + section[header_end:]
        return config_text[: section_match.start()] + section + config_text[section_match.end() :]

    separator = "" if not config_text.strip() else "\n\n"
    return config_text.rstrip() + separator + "[windows]\n" + sandbox_line + "\n"


def ensure_desktop_reasoning_efforts(config_text: str) -> str:
    section_pattern = re.compile(r"(?ms)^\[desktop\]\s*\r?\n.*?(?=^\[|\Z)")
    section_match = section_pattern.search(config_text)
    efforts_pattern = re.compile(r"(?m)^enabled-reasoning-efforts\s*=.*$")
    efforts = json.dumps(DESKTOP_REASONING_EFFORTS, separators=(",", ":"))
    efforts_line = f"enabled-reasoning-efforts = {efforts}"
    if section_match:
        section = section_match.group(0)
        if efforts_pattern.search(section):
            section = efforts_pattern.sub(efforts_line, section, count=1)
        else:
            header_end = section.find("\n") + 1
            section = section[:header_end] + efforts_line + "\n" + section[header_end:]
        return config_text[: section_match.start()] + section + config_text[section_match.end() :]

    separator = "" if not config_text.strip() else "\n\n"
    return config_text.rstrip() + separator + "[desktop]\n" + efforts_line + "\n"


def local_router_port(value: str, *, allow_root_path: bool) -> int:
    parsed = urlsplit(value)
    allowed_paths = {"", "/", "/v1", "/v1/"} if allow_root_path else {"/v1", "/v1/"}
    if (
        parsed.scheme != "http"
        or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in allowed_paths
    ):
        raise RuntimeError("Codex Router provider must use a local HTTP /v1 endpoint")
    try:
        return parsed.port or 80
    except ValueError as exc:
        raise RuntimeError("Codex Router provider has an invalid local port") from exc


def decode_toml_basic_string(value: str) -> str:
    try:
        return json.loads(f'"{value}"')
    except json.JSONDecodeError as exc:
        raise RuntimeError("Codex Router model catalog path is not a valid string") from exc


def validate_router_provider(config_text: str, expected_base_url: str) -> None:
    if not re.search(r'(?m)^model_provider\s*=\s*"custom"\s*$', config_text):
        raise RuntimeError("Codex Router provider must be selected as custom")
    section = re.search(
        r"(?ms)^\[model_providers\.custom\]\s*\r?\n.*?(?=^\[|\Z)",
        config_text,
    )
    if section is None:
        raise RuntimeError("Codex Router provider section is missing")
    provider = section.group(0)
    if not re.search(r'(?m)^name\s*=\s*"Codex-Router"\s*$', provider):
        raise RuntimeError("The custom provider is not managed by Codex-Router")
    base_match = re.search(r'(?m)^base_url\s*=\s*"([^"]+)"\s*$', provider)
    if base_match is None:
        raise RuntimeError("Codex Router provider base URL is missing")
    actual_port = local_router_port(base_match.group(1), allow_root_path=False)
    expected_port = local_router_port(expected_base_url, allow_root_path=True)
    if actual_port != expected_port:
        raise RuntimeError(f"Codex Router provider must use the configured local port {expected_port}")
    if not re.search(r"(?m)^requires_openai_auth\s*=\s*true\s*$", provider):
        raise RuntimeError("Codex Router provider must preserve OpenAI authentication")
    if not re.search(r'(?m)^experimental_bearer_token\s*=\s*"[^"\r\n]+"\s*$', provider):
        raise RuntimeError("Codex Router provider is missing its local bearer token")

    catalog_match = re.search(
        r'(?m)^model_catalog_json\s*=\s*"((?:\\.|[^"\\])+)"\s*$',
        config_text,
    )
    if catalog_match is None:
        raise RuntimeError("Codex Router model catalog is missing")
    catalog_path = pathlib.Path(decode_toml_basic_string(catalog_match.group(1)))
    if not catalog_path.is_file():
        raise RuntimeError(f"Codex Router model catalog does not exist: {catalog_path}")


def atomic_write(path: pathlib.Path, data: bytes) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    try:
        with temporary.open("xb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", required=True)
    parser.add_argument("--provider-id", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--auth", required=True)
    parser.add_argument("--backup-dir", required=True)
    parser.add_argument("--settings")
    parser.add_argument("--base-url", default="http://127.0.0.1:18080")
    parser.add_argument(
        "--require-inactive",
        action="store_true",
        help="Abort unless the target provider is currently inactive",
    )
    parser.add_argument(
        "--activate",
        action="store_true",
        help="Mark this provider active after the same config was applied locally",
    )
    args = parser.parse_args()

    db_path = pathlib.Path(args.db)
    config_path = pathlib.Path(args.config)
    auth_path = pathlib.Path(args.auth)
    backup_dir = pathlib.Path(args.backup_dir)
    settings_path = pathlib.Path(args.settings) if args.settings else db_path.parent / "settings.json"
    backup_dir.mkdir(parents=True, exist_ok=True)

    timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    backup_path = backup_dir / f"cc-switch-before-codex-router-{timestamp}.db.gz.dpapi"
    plain_backup_path = backup_dir / f".cc-switch-before-codex-router-{uuid.uuid4().hex}.db.tmp"
    original_config_text = config_path.read_text(encoding="utf-8-sig")
    config_text = ensure_desktop_reasoning_efforts(
        normalize_windows_sandbox(original_config_text)
    )
    reserved_provider = re.search(
        r"(?m)^\s*\[model_providers\.(openai|ollama|lmstudio)(?:[.\]])",
        config_text,
    )
    if reserved_provider:
        raise RuntimeError(
            f"Reserved Codex provider cannot be synchronized: "
            f"{reserved_provider.group(1)}"
        )
    validate_router_provider(config_text, args.base_url)
    auth = json.loads(auth_path.read_text(encoding="utf-8-sig"))
    if auth.get("auth_mode") != "chatgpt" or not isinstance(auth.get("tokens"), dict):
        raise RuntimeError("Current Codex auth state is not a ChatGPT login")
    original_cc_settings = settings_path.read_bytes()
    original_cc_settings_object = json.loads(original_cc_settings.decode("utf-8-sig"))
    if not isinstance(original_cc_settings_object, dict):
        raise RuntimeError("CC Switch settings.json must contain an object")
    cc_settings = dict(original_cc_settings_object)
    cc_settings["preserveCodexOfficialAuthOnSwitch"] = True
    cc_settings["unifyCodexSessionHistory"] = True
    cc_settings["unifyCodexMigrateExisting"] = True
    if args.activate:
        cc_settings["currentProviderCodex"] = args.provider_id
    updated_cc_settings = (
        json.dumps(cc_settings, ensure_ascii=False, indent=2) + "\n"
    ).encode("utf-8")

    connection = sqlite3.connect(db_path, timeout=30)
    settings_written = False
    try:
        row = connection.execute(
            "SELECT settings_config, is_current FROM providers "
            "WHERE id = ? AND app_type = 'codex'",
            (args.provider_id,),
        ).fetchone()
        if row is None:
            raise RuntimeError(f"CC Switch Codex provider not found: {args.provider_id}")
        if args.require_inactive and bool(row[1]):
            raise RuntimeError("CC Switch target provider is active; offline update was aborted")

        backup = sqlite3.connect(plain_backup_path)
        try:
            connection.backup(backup)
        finally:
            backup.close()
        protect_compressed_backup(plain_backup_path, backup_path)
        plain_backup_path.unlink(missing_ok=True)
        prune_backups(backup_dir)

        settings = json.loads(row[0])
        settings["auth"] = auth
        settings["config"] = config_text
        settings["codexRouter"] = {
            "managed": True,
            "profileId": args.provider_id,
            "version": 1,
        }

        connection.execute("BEGIN IMMEDIATE")
        has_category = any(
            column[1] == "category"
            for column in connection.execute("PRAGMA table_info(providers)")
        )
        if has_category:
            connection.execute(
                "UPDATE providers SET settings_config = ?, category = 'third_party' "
                "WHERE id = ? AND app_type = 'codex'",
                (json.dumps(settings, ensure_ascii=False, separators=(",", ":")), args.provider_id),
            )
        else:
            connection.execute(
                "UPDATE providers SET settings_config = ? "
                "WHERE id = ? AND app_type = 'codex'",
                (json.dumps(settings, ensure_ascii=False, separators=(",", ":")), args.provider_id),
            )
        if args.activate:
            connection.execute(
                "UPDATE providers SET is_current = CASE WHEN id = ? THEN 1 ELSE 0 END "
                "WHERE app_type = 'codex'",
                (args.provider_id,),
            )
            active_group = connection.execute(
                "SELECT value FROM settings WHERE key = 'current_profile_id_codex' LIMIT 1"
            ).fetchone()
            if active_group:
                group = connection.execute(
                    "SELECT payload FROM profiles WHERE id = ? LIMIT 1",
                    (active_group[0],),
                ).fetchone()
                if group:
                    payload = json.loads(group[0])
                    providers = payload.setdefault("providers", {})
                    if not isinstance(providers, dict):
                        raise RuntimeError("CC Switch profile providers must be an object")
                    providers["codex"] = args.provider_id
                    connection.execute(
                        "UPDATE profiles SET payload = ? WHERE id = ?",
                        (
                            json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
                            active_group[0],
                        ),
                    )
        if cc_settings != original_cc_settings_object:
            atomic_write(settings_path, updated_cc_settings)
            settings_written = True
        connection.commit()
    except Exception:
        connection.rollback()
        if settings_written:
            atomic_write(settings_path, original_cc_settings)
        raise
    finally:
        connection.close()
        plain_backup_path.unlink(missing_ok=True)

    print(f"CC Switch provider synchronized; backup: {backup_path}")


if __name__ == "__main__":
    main()
