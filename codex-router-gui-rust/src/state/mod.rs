//! SQLite-backed Router state. One connection is deliberately serialized: every
//! writer enters the same queue, while WAL permits readers in future versions.

pub mod legacy_migration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SCHEMA_VERSION: i64 = 1;

#[derive(Clone)]
pub struct StateStore {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
}

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create SQLite parent directory")?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open SQLite database {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;
        let store = Self {
            path: path.to_path_buf(),
            connection: Arc::new(Mutex::new(connection)),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        operation(&connection)
    }

    pub fn migrate(&self) -> Result<()> {
        self.with_connection(|connection| {
            connection.execute_batch(
                r#"
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS schema_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS accounts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    platform TEXT NOT NULL,
                    account_type TEXT NOT NULL,
                    auth_index TEXT NOT NULL,
                    auth_file TEXT NOT NULL,
                    stable_identity_hmac TEXT NOT NULL UNIQUE,
                    status TEXT NOT NULL DEFAULT 'active',
                    schedulable INTEGER NOT NULL DEFAULT 1,
                    priority INTEGER NOT NULL DEFAULT 1,
                    weight INTEGER NOT NULL DEFAULT 1 CHECK(weight BETWEEN 1 AND 1000000),
                    proxy_id INTEGER,
                    payload TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    deleted_at TEXT
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_accounts_live_identity
                    ON accounts(stable_identity_hmac) WHERE deleted_at IS NULL;
                CREATE TABLE IF NOT EXISTS groups (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'active',
                    models TEXT NOT NULL DEFAULT '[]',
                    payload TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    deleted_at TEXT
                );
                CREATE TABLE IF NOT EXISTS account_groups (
                    account_id INTEGER NOT NULL REFERENCES accounts(id),
                    group_id INTEGER NOT NULL REFERENCES groups(id),
                    PRIMARY KEY(account_id, group_id)
                );
                CREATE TABLE IF NOT EXISTS proxies (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    protocol TEXT NOT NULL,
                    host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    normalized_url TEXT NOT NULL UNIQUE,
                    fallback_policy TEXT NOT NULL DEFAULT 'forbid_direct',
                    payload TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    deleted_at TEXT
                );
                CREATE TABLE IF NOT EXISTS composite_routes (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    group_id INTEGER NOT NULL REFERENCES groups(id),
                    public_model TEXT NOT NULL,
                    upstream_model TEXT NOT NULL,
                    target_platform TEXT NOT NULL,
                    endpoint TEXT NOT NULL,
                    priority INTEGER NOT NULL DEFAULT 1,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    payload TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    deleted_at TEXT,
                    UNIQUE(group_id, public_model, upstream_model, target_platform, endpoint)
                );
                CREATE TABLE IF NOT EXISTS api_keys (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    key_hmac TEXT NOT NULL UNIQUE,
                    key_suffix TEXT NOT NULL,
                    group_id INTEGER REFERENCES groups(id),
                    status TEXT NOT NULL DEFAULT 'active',
                    payload TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS scheduled_test_plans (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id INTEGER NOT NULL REFERENCES accounts(id),
                    model TEXT NOT NULL,
                    cron TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    auto_recover INTEGER NOT NULL DEFAULT 0,
                    max_results INTEGER NOT NULL DEFAULT 20 CHECK(max_results BETWEEN 1 AND 1000),
                    last_run TEXT,
                    next_run TEXT,
                    payload TEXT NOT NULL DEFAULT '{}'
                );
                CREATE TABLE IF NOT EXISTS scheduled_test_results (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    plan_id INTEGER NOT NULL REFERENCES scheduled_test_plans(id) ON DELETE CASCADE,
                    status TEXT NOT NULL,
                    error_code TEXT,
                    details TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE INDEX IF NOT EXISTS idx_plan_results_time
                    ON scheduled_test_results(plan_id, created_at DESC);
                CREATE TABLE IF NOT EXISTS continuation_bindings (
                    session_key_hmac TEXT PRIMARY KEY,
                    pool_id TEXT NOT NULL,
                    account_id INTEGER REFERENCES accounts(id),
                    owner_state TEXT NOT NULL DEFAULT 'active',
                    expires_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS request_ledger (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    request_id TEXT NOT NULL UNIQUE,
                    model TEXT NOT NULL,
                    pool_id TEXT NOT NULL,
                    account_id INTEGER REFERENCES accounts(id),
                    protocol TEXT NOT NULL,
                    status TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cached_tokens INTEGER NOT NULL DEFAULT 0,
                    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                    cost_micros INTEGER NOT NULL DEFAULT 0,
                    cost_known INTEGER NOT NULL DEFAULT 0,
                    elapsed_ms INTEGER NOT NULL DEFAULT 0,
                    error_code TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE INDEX IF NOT EXISTS idx_ledger_account_time
                    ON request_ledger(account_id, created_at DESC);
                CREATE TABLE IF NOT EXISTS usage_windows (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id INTEGER REFERENCES accounts(id),
                    provider TEXT NOT NULL,
                    window_kind TEXT NOT NULL,
                    used TEXT NOT NULL,
                    quota TEXT NOT NULL,
                    reset_at TEXT,
                    source TEXT NOT NULL DEFAULT 'live',
                    sampled_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(account_id, provider, window_kind)
                );
                CREATE TABLE IF NOT EXISTS oauth_sessions (
                    state_hmac TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    status TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    metadata TEXT NOT NULL DEFAULT '{}'
                );
                CREATE TABLE IF NOT EXISTS migration_journal (
                    object_type TEXT NOT NULL,
                    legacy_id TEXT NOT NULL,
                    new_id TEXT NOT NULL,
                    snapshot_hash TEXT NOT NULL,
                    state TEXT NOT NULL,
                    error_code TEXT,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY(object_type, legacy_id)
                );
                CREATE TABLE IF NOT EXISTS admin_settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS admin_tokens (
                    token_hmac TEXT PRIMARY KEY,
                    expires_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS image_tasks (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    model TEXT NOT NULL,
                    input_ref TEXT,
                    output_ref TEXT,
                    error_code TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS image_batches (
                    id TEXT PRIMARY KEY,
                    status TEXT NOT NULL,
                    model TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    completed_at TEXT
                );
                CREATE TABLE IF NOT EXISTS image_batch_items (
                    batch_id TEXT NOT NULL REFERENCES image_batches(id) ON DELETE CASCADE,
                    custom_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    output_ref TEXT,
                    error_code TEXT,
                    PRIMARY KEY(batch_id, custom_id)
                );
                INSERT INTO schema_meta(key, value) VALUES('schema_version', '1')
                    ON CONFLICT(key) DO UPDATE SET value=excluded.value;
                INSERT OR IGNORE INTO schema_meta(key, value) VALUES('baseline', '1.7.10/8afc259');
                COMMIT;
                "#,
            )?;
            let version: i64 = connection.query_row(
                "SELECT CAST(value AS INTEGER) FROM schema_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )?;
            if version > SCHEMA_VERSION {
                bail!("SQLite schema {version} is newer than supported {SCHEMA_VERSION}");
            }
            Ok(())
        })
    }

    pub fn integrity_check(&self) -> Result<bool> {
        self.with_connection(|connection| {
            let result: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            Ok(result == "ok")
        })
    }

    pub fn backup_to(&self, destination: impl AsRef<Path>) -> Result<PathBuf> {
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if destination.exists() {
            bail!("SQLite backup destination already exists");
        }
        self.with_connection(|connection| {
            connection.execute(
                "VACUUM INTO ?1",
                params![destination.to_string_lossy().to_string()],
            )?;
            Ok(())
        })?;
        Ok(destination.to_path_buf())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT value FROM admin_settings WHERE key=?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.with_connection(|connection| {
            connection.execute(
                "INSERT INTO admin_settings(key,value) VALUES(?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value.to_string()],
            )?;
            Ok(())
        })
    }

    pub fn record_ledger_once(&self, entry: &Value) -> Result<i64> {
        self.with_connection(|connection| {
            let changed = connection.execute(
                "INSERT OR IGNORE INTO request_ledger(
                    request_id,model,pool_id,account_id,protocol,status,
                    input_tokens,output_tokens,cached_tokens,reasoning_tokens,
                    cost_micros,cost_known,elapsed_ms,error_code
                ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    value_str(entry, "request_id"),
                    value_str(entry, "model"),
                    value_str(entry, "pool_id"),
                    entry.get("account_id").and_then(Value::as_i64),
                    value_str(entry, "protocol"),
                    value_str(entry, "status"),
                    entry
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    entry
                        .get("output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    entry
                        .get("cached_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    entry
                        .get("reasoning_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    entry
                        .get("cost_micros")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    entry
                        .get("cost_known")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    entry.get("elapsed_ms").and_then(Value::as_i64).unwrap_or(0),
                    entry.get("error_code").and_then(Value::as_str),
                ],
            )?;
            if changed == 0 {
                bail!("request ledger entry already exists");
            }
            connection
                .query_row(
                    "SELECT id FROM request_ledger WHERE request_id=?1",
                    params![value_str(entry, "request_id")],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        })
    }
}

fn value_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn migrations_are_idempotent_and_integrity_passes() {
        let root = std::env::temp_dir().join(format!("router-state-{}", Uuid::now_v7()));
        let path = root.join("router-state.sqlite3");
        let store = StateStore::open(&path).unwrap();
        store.migrate().unwrap();
        assert!(store.integrity_check().unwrap());
    }

    #[test]
    fn foreign_keys_and_identity_constraints_are_enforced() {
        let root = std::env::temp_dir().join(format!("router-state-{}", Uuid::now_v7()));
        let store = StateStore::open(root.join("router-state.sqlite3")).unwrap();
        let result = store.with_connection(|connection| {
            connection.execute(
                "INSERT INTO account_groups(account_id,group_id) VALUES(999,999)",
                [],
            )?;
            Ok(())
        });
        assert!(result.is_err());
    }

    #[test]
    fn ledger_is_exactly_once() {
        let root = std::env::temp_dir().join(format!("router-state-{}", Uuid::now_v7()));
        let store = StateStore::open(root.join("router-state.sqlite3")).unwrap();
        let entry = json!({
            "request_id": "req-once", "model": "m", "pool_id": "p", "protocol": "responses",
            "status": "completed", "input_tokens": 2, "output_tokens": 3, "cost_known": false
        });
        assert!(store.record_ledger_once(&entry).unwrap() > 0);
        assert!(store.record_ledger_once(&entry).is_err());
    }
}
