//! Data-plane helpers owned by the Router Host: ledger-backed usage/billing
//! aggregation, direct API-key channel resolution for legacy adapters
//! (embeddings, async images) that CLIProxyAPI does not serve, and the async
//! image task/batch state machine persisted in SQLite.

use crate::state::StateStore;
use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Aggregate the request ledger per public model for the old `/v1/usage`
/// contract. Costs stay zero with `cost_known=false` until prices exist.
pub fn usage_by_model(store: &StateStore) -> Result<Value> {
    store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT model, COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(cached_tokens),0), COALESCE(SUM(reasoning_tokens),0)
             FROM request_ledger WHERE status='completed'
             GROUP BY model ORDER BY model",
        )?;
        let rows = statement.query_map([], |row| {
            let input: i64 = row.get(2)?;
            let output: i64 = row.get(3)?;
            let cached: i64 = row.get(4)?;
            let reasoning: i64 = row.get(5)?;
            Ok(json!({
                "model": row.get::<_, String>(0)?,
                "requests": row.get::<_, i64>(1)?,
                "input_tokens": input,
                "output_tokens": output,
                "cache_creation_tokens": 0,
                "cache_read_tokens": cached,
                "reasoning_tokens": reasoning,
                "total_tokens": input + output,
                "cost": 0.0,
                "actual_cost": 0.0,
                "cost_known": false,
            }))
        })?;
        let mut items = Vec::new();
        let mut total_requests = 0i64;
        let mut total_tokens = 0i64;
        for row in rows {
            let row = row?;
            total_requests += row["requests"].as_i64().unwrap_or(0);
            total_tokens += row["total_tokens"].as_i64().unwrap_or(0);
            items.push(row);
        }
        Ok::<Value, anyhow::Error>(json!({
            "object": "list",
            "data": items,
            "total_requests": total_requests,
            "total_tokens": total_tokens,
            "cost_known": false,
        }))
    })
}

/// Old `/v1/sub2api/billing` envelope: local single-tenant router bills at a
/// flat multiplier of 1.0; shape kept for legacy clients.
pub fn key_billing_info() -> Value {
    json!({
        "object": "billing_info",
        "schema_version": 1,
        "billing_scope": "local",
        "group_rate_multiplier": 1.0,
        "resolved_rate_multiplier": 1.0,
        "peak_rate_enabled": false,
        "effective_rate_multiplier": 1.0,
        "observed_at": chrono::Utc::now().to_rfc3339(),
    })
}

/// Resolved direct upstream channel for a Router pool.
pub struct DirectChannel {
    pub account_id: i64,
    pub base_url: String,
    pub api_key: String,
}

/// Resolve the first schedulable API-key account backing a pool
/// (`cr/r{route_id}/{platform}`) to its direct upstream channel. Returns None
/// when the pool only has OAuth accounts or no schedulable API-key account.
pub fn resolve_pool_direct_channel(
    store: &StateStore,
    pool_id: &str,
) -> Result<Option<DirectChannel>> {
    let Some(route_id) = pool_id
        .strip_prefix("cr/r")
        .and_then(|tail| tail.split('/').next())
        .and_then(|digits| digits.parse::<i64>().ok())
    else {
        return Ok(None);
    };
    let rows = store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT a.id, a.payload FROM accounts a
             JOIN account_groups ag ON ag.account_id = a.id
             JOIN composite_routes r ON r.group_id = ag.group_id
             WHERE r.id = ?1 AND r.deleted_at IS NULL
               AND a.deleted_at IS NULL AND a.schedulable = 1 AND a.account_type = 'apikey'
             ORDER BY a.priority, a.id",
        )?;
        let rows = statement
            .query_map(rusqlite::params![route_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok::<Vec<(i64, String)>, anyhow::Error>(rows)
    })?;
    for (account_id, payload) in rows {
        let payload: Value = serde_json::from_str(&payload).unwrap_or_else(|_| json!({}));
        let base_url = payload
            .pointer("/credentials/base_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let Some(base_url) = base_url else { continue };
        let secret = crate::credentials::read_text(&format!("AccountKey-{account_id}"))
            .ok()
            .flatten()
            .map(|secret| secret.to_string())
            .filter(|secret| !secret.trim().is_empty());
        let Some(api_key) = secret else { continue };
        return Ok(Some(DirectChannel {
            account_id,
            base_url,
            api_key,
        }));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Async image tasks and batches
// ---------------------------------------------------------------------------

pub fn image_task_create(store: &StateStore, kind: &str, model: &str) -> Result<String> {
    let id = format!("imgtask_{}", uuid::Uuid::now_v7().simple());
    store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO image_tasks(id,kind,status,model) VALUES(?1,?2,'queued',?3)",
            rusqlite::params![id, kind, model],
        )?;
        Ok(())
    })?;
    Ok(id)
}

pub fn image_task_status(
    store: &StateStore,
    id: &str,
    status: &str,
    output_ref: Option<&str>,
    error_code: Option<&str>,
) -> Result<()> {
    store.with_connection(|connection| {
        connection.execute(
            "UPDATE image_tasks SET status=?2,output_ref=COALESCE(?3,output_ref),error_code=?4,updated_at=CURRENT_TIMESTAMP WHERE id=?1",
            rusqlite::params![id, status, output_ref, error_code],
        )?;
        Ok(())
    })
}

pub fn image_task_get(store: &StateStore, id: &str) -> Result<Option<Value>> {
    store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,kind,status,model,output_ref,error_code,created_at,updated_at FROM image_tasks WHERE id=?1",
        )?;
        let mut rows = statement.query(rusqlite::params![id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(json!({
                "id": row.get::<_, String>(0)?,
                "kind": row.get::<_, String>(1)?,
                "status": row.get::<_, String>(2)?,
                "model": row.get::<_, String>(3)?,
                "output_ref": row.get::<_, Option<String>>(4)?,
                "error_code": row.get::<_, Option<String>>(5)?,
                "created_at": row.get::<_, String>(6)?,
                "updated_at": row.get::<_, String>(7)?,
            })));
        }
        Ok(None)
    })
}

pub fn image_batch_create(
    store: &StateStore,
    model: &str,
    custom_ids: &[String],
) -> Result<String> {
    let id = format!("imgbatch_{}", uuid::Uuid::now_v7().simple());
    store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO image_batches(id,status,model) VALUES(?1,'queued',?2)",
            rusqlite::params![id, model],
        )?;
        for custom_id in custom_ids {
            connection.execute(
                "INSERT INTO image_batch_items(batch_id,custom_id,status) VALUES(?1,?2,'queued')",
                rusqlite::params![id, custom_id],
            )?;
        }
        Ok(())
    })?;
    Ok(id)
}

pub fn image_batch_get(store: &StateStore, id: &str) -> Result<Option<Value>> {
    store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,status,model,created_at,completed_at FROM image_batches WHERE id=?1",
        )?;
        let mut rows = statement.query(rusqlite::params![id])?;
        let Some(row) = rows.next()? else { return Ok(None) };
        let mut item_stmt = connection.prepare(
            "SELECT custom_id,status,output_ref,error_code FROM image_batch_items WHERE batch_id=?1 ORDER BY custom_id",
        )?;
        let items = item_stmt
            .query_map(rusqlite::params![id], |item| {
                Ok(json!({
                    "custom_id": item.get::<_, String>(0)?,
                    "status": item.get::<_, String>(1)?,
                    "output_ref": item.get::<_, Option<String>>(2)?,
                    "error_code": item.get::<_, Option<String>>(3)?,
                }))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Some(json!({
            "id": row.get::<_, String>(0)?,
            "status": row.get::<_, String>(1)?,
            "model": row.get::<_, String>(2)?,
            "created_at": row.get::<_, String>(3)?,
            "completed_at": row.get::<_, Option<String>>(4)?,
            "items": items,
        })))
    })
}

pub fn image_batch_list(store: &StateStore) -> Result<Value> {
    store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,status,model,created_at,completed_at FROM image_batches ORDER BY created_at DESC LIMIT 100",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "status": row.get::<_, String>(1)?,
                    "model": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, String>(3)?,
                    "completed_at": row.get::<_, Option<String>>(4)?,
                }))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!({"object": "list", "data": rows}))
    })
}

pub fn image_batch_item_update(
    store: &StateStore,
    batch_id: &str,
    custom_id: &str,
    status: &str,
    output_ref: Option<&str>,
    error_code: Option<&str>,
) -> Result<()> {
    store.with_connection(|connection| {
        connection.execute(
            "UPDATE image_batch_items SET status=?3,output_ref=COALESCE(?4,output_ref),error_code=?5 WHERE batch_id=?1 AND custom_id=?2",
            rusqlite::params![batch_id, custom_id, status, output_ref, error_code],
        )?;
        Ok(())
    })
}

pub fn image_batch_finish(store: &StateStore, id: &str, status: &str) -> Result<()> {
    store.with_connection(|connection| {
        connection.execute(
            "UPDATE image_batches SET status=?2,completed_at=CURRENT_TIMESTAMP WHERE id=?1",
            rusqlite::params![id, status],
        )?;
        Ok(())
    })
}

pub fn image_batch_start(store: &StateStore, id: &str) -> Result<()> {
    store.with_connection(|connection| {
        connection.execute(
            "UPDATE image_batches SET status='running' WHERE id=?1 AND status='queued'",
            rusqlite::params![id],
        )?;
        Ok(())
    })
}

/// Clear stored output references after the caller deleted the files on disk.
pub fn image_batch_clear_outputs(store: &StateStore, id: &str) -> Result<u64> {
    store.with_connection(|connection| {
        let changed = connection.execute(
            "UPDATE image_batch_items SET output_ref=NULL WHERE batch_id=?1 AND output_ref IS NOT NULL",
            rusqlite::params![id],
        )?;
        Ok(changed as u64)
    })
}

pub fn image_batch_cancel_pending(store: &StateStore, id: &str) -> Result<u64> {
    store.with_connection(|connection| {
        let changed = connection.execute(
            "UPDATE image_batch_items SET status='cancelled',error_code='CR-REQ-0008' WHERE batch_id=?1 AND status='queued'",
            rusqlite::params![id],
        )?;
        Ok(changed as u64)
    })
}

pub fn image_batch_delete(store: &StateStore, id: &str) -> Result<u64> {
    store.with_connection(|connection| {
        let changed = connection.execute(
            "DELETE FROM image_batches WHERE id=?1",
            rusqlite::params![id],
        )?;
        Ok(changed as u64)
    })
}

/// Validate a task/batch identifier: generated ids are alphanumeric plus a
/// single underscore prefix; anything else is a path-escape attempt.
pub fn safe_task_id(id: &str) -> Result<&str> {
    let ok = !id.is_empty()
        && id.len() <= 96
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        });
    if ok {
        Ok(id)
    } else {
        anyhow::bail!("CR-VAL-0006: unsafe task id")
    }
}

/// Resolve an output reference to an on-disk path, refusing escapes outside
/// the task output directory.
pub fn task_output_path(root: &std::path::Path, output_ref: &str) -> Result<std::path::PathBuf> {
    let cleaned = output_ref.replace('/', "\\");
    if cleaned.contains("..") || cleaned.starts_with('\\') || cleaned.contains(':') {
        anyhow::bail!("CR-VAL-0006: unsafe output reference");
    }
    Ok(root.join("data").join("image-tasks").join(cleaned))
}

pub fn ensure_task_dir(root: &std::path::Path) -> Result<std::path::PathBuf> {
    let dir = root.join("data").join("image-tasks");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn usage_by_model_aggregates_completed_rows() {
        let root = std::env::temp_dir().join(format!("router-dataops-{}", Uuid::now_v7()));
        let store = StateStore::open(root.join("state.sqlite3")).unwrap();
        let record = |request_id: &str, model: &str, status: &str, input: i64, output: i64| {
            crate::telemetry::ledger::record_terminal(
                &store,
                &crate::telemetry::ledger::ledger_entry(&crate::telemetry::ledger::LedgerInput {
                    request_id,
                    model,
                    pool_id: "p",
                    account_id: None,
                    protocol: "responses",
                    status,
                    body: Some(&json!({"usage":{"input_tokens":input,"output_tokens":output}})),
                    elapsed_ms: 1,
                    error_code: None,
                }),
            )
            .unwrap();
        };
        record("r1", "m-a", "completed", 3, 4);
        record("r2", "m-a", "completed", 5, 6);
        record("r3", "m-a", "failed", 9, 9);
        record("r4", "m-b", "completed", 1, 2);
        let usage = usage_by_model(&store).unwrap();
        let rows = usage["data"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["model"], "m-a");
        assert_eq!(rows[0]["requests"], 2);
        assert_eq!(rows[0]["input_tokens"], 8);
        assert_eq!(rows[0]["output_tokens"], 10);
        assert_eq!(rows[1]["model"], "m-b");
        assert_eq!(usage["total_requests"], 3);
        let _ = std::fs::remove_dir_all(root);
    }
}
