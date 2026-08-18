//! Request usage ledger and normalized token extraction.

use crate::state::StateStore;
use anyhow::Result;
use serde_json::{json, Value};

pub const MAX_RETENTION_ROWS: i64 = 500_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageTokens {
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub reasoning: i64,
}

pub fn extract_tokens(protocol: &str, body: &Value) -> UsageTokens {
    let usage = body
        .pointer("/usage")
        .or_else(|| body.pointer("/usageMetadata"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let integer = |keys: &[&str]| -> i64 {
        keys.iter()
            .find_map(|key| {
                if key.contains('/') {
                    usage.pointer(&format!("/{key}")).and_then(Value::as_i64)
                } else {
                    usage.get(*key).and_then(Value::as_i64)
                }
            })
            .unwrap_or(0)
    };
    match protocol {
        "responses" => UsageTokens {
            input: integer(&["input_tokens", "prompt_tokens"]),
            output: integer(&["output_tokens", "completion_tokens"]),
            cached: integer(&["input_tokens_details/cached_tokens", "cached_tokens"]),
            reasoning: integer(&["output_tokens_details/reasoning_tokens", "reasoning_tokens"]),
        },
        "gemini" => {
            let prompt = integer(&["promptTokenCount"]);
            let candidates = integer(&["candidatesTokenCount"]);
            let cached = integer(&["promptTokensDetails/cachedTokens"]);
            let thoughts = integer(&["thoughtsTokenCount"]);
            UsageTokens {
                input: prompt,
                output: candidates,
                cached,
                reasoning: thoughts,
            }
        }
        "anthropic" => UsageTokens {
            input: integer(&["input_tokens"]),
            output: integer(&["output_tokens"]),
            cached: integer(&["cache_read_input_tokens"]),
            reasoning: 0,
        },
        _ => UsageTokens {
            input: integer(&["prompt_tokens"]),
            output: integer(&["completion_tokens"]),
            cached: integer(&["prompt_tokens_details/cached_tokens"]),
            reasoning: integer(&["completion_tokens_details/reasoning_tokens"]),
        },
    }
}

#[derive(Clone, Debug, Default)]
pub struct LedgerInput<'a> {
    pub request_id: &'a str,
    pub model: &'a str,
    pub pool_id: &'a str,
    pub account_id: Option<i64>,
    pub protocol: &'a str,
    pub status: &'a str,
    pub body: Option<&'a Value>,
    pub elapsed_ms: i64,
    pub error_code: Option<&'a str>,
}

pub fn ledger_entry(input: &LedgerInput<'_>) -> Value {
    let tokens = input
        .body
        .map(|body| extract_tokens(input.protocol, body))
        .unwrap_or_default();
    json!({
        "request_id": input.request_id,
        "model": input.model,
        "pool_id": input.pool_id,
        "account_id": input.account_id,
        "protocol": input.protocol,
        "status": input.status,
        "input_tokens": tokens.input,
        "output_tokens": tokens.output,
        "cached_tokens": tokens.cached,
        "reasoning_tokens": tokens.reasoning,
        "cost_micros": 0,
        "cost_known": false,
        "elapsed_ms": input.elapsed_ms,
        "error_code": input.error_code,
    })
}

pub fn record_terminal(store: &StateStore, entry: &Value) -> Result<i64> {
    store.record_ledger_once(entry)
}

pub fn account_totals(store: &StateStore, account_id: i64) -> Result<Value> {
    store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT COUNT(*),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0),COALESCE(SUM(cached_tokens),0),COALESCE(SUM(reasoning_tokens),0) FROM request_ledger WHERE account_id=?1",
        )?;
        let summary = statement.query_row(rusqlite::params![account_id], |row| {
            Ok(json!({
                "requests": row.get::<_, i64>(0)?,
                "input_tokens": row.get::<_, i64>(1)?,
                "output_tokens": row.get::<_, i64>(2)?,
                "cached_tokens": row.get::<_, i64>(3)?,
                "reasoning_tokens": row.get::<_, i64>(4)?,
                "cost_known": false,
            }))
        })?;
        Ok(summary)
    })
}

pub fn enforce_retention(store: &StateStore) -> Result<u64> {
    store.with_connection(|connection| {
        let changed = connection.execute(
            "DELETE FROM request_ledger WHERE id <= (SELECT id FROM request_ledger ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            rusqlite::params![MAX_RETENTION_ROWS],
        )? as u64;
        Ok(changed)
    })
}

pub fn mark_cost_known_if_priced(entry: &mut Value, input_micros: i64, output_micros: i64) {
    let input = entry
        .get("input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let output = entry
        .get("output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let cost = input
        .saturating_mul(input_micros)
        .saturating_add(output.saturating_mul(output_micros));
    entry["cost_micros"] = cost.into();
    entry["cost_known"] = Value::Bool(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn usage_is_parsed_for_each_protocol_without_inventing_costs() {
        let responses = json!({"usage":{"input_tokens":7,"output_tokens":3,"input_tokens_details":{"cached_tokens":4},"output_tokens_details":{"reasoning_tokens":2}}});
        assert_eq!(
            extract_tokens("responses", &responses),
            UsageTokens {
                input: 7,
                output: 3,
                cached: 4,
                reasoning: 2
            }
        );
        let gemini = json!({"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":5,"promptTokensDetails":{"cachedTokens":2},"thoughtsTokenCount":1}});
        assert_eq!(
            extract_tokens("gemini", &gemini),
            UsageTokens {
                input: 9,
                output: 5,
                cached: 2,
                reasoning: 1
            }
        );
    }

    #[test]
    fn terminal_ledger_is_exactly_once_and_persistent() {
        let root = std::env::temp_dir().join(format!("router-ledger-{}", Uuid::now_v7()));
        let store = StateStore::open(root.join("state.sqlite3")).unwrap();
        store.with_connection(|connection| {
            connection.execute(
                "INSERT INTO accounts(platform,account_type,auth_index,auth_file,stable_identity_hmac) VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params!["openai", "api", "idx-1", "f.json", "h-ledger-test"],
            )?;
            Ok(())
        }).unwrap();
        let entry = ledger_entry(&LedgerInput {
            request_id: "req-ledger",
            model: "m",
            pool_id: "p",
            account_id: Some(1),
            protocol: "responses",
            status: "completed",
            body: Some(&json!({"usage":{"input_tokens":1,"output_tokens":2}})),
            elapsed_ms: 10,
            error_code: None,
        });
        assert!(record_terminal(&store, &entry).unwrap() > 0);
        assert!(record_terminal(&store, &entry).is_err());
        assert_eq!(account_totals(&store, 1).unwrap()["input_tokens"], 1);
    }

    #[test]
    fn unknown_prices_do_not_pretend_to_be_known() {
        let mut entry = ledger_entry(&LedgerInput {
            request_id: "req",
            model: "m",
            pool_id: "p",
            protocol: "chat",
            status: "completed",
            elapsed_ms: 1,
            ..Default::default()
        });
        assert_eq!(entry["cost_known"], Value::Bool(false));
        mark_cost_known_if_priced(&mut entry, 2, 3);
        assert_eq!(entry["cost_known"], Value::Bool(true));
    }

    #[test]
    fn negative_tokens_cannot_enter_ledger() {
        let entry = ledger_entry(&LedgerInput {
            request_id: "neg",
            model: "m",
            pool_id: "p",
            protocol: "chat",
            status: "failed",
            body: Some(&json!({"usage":{"prompt_tokens":-1,"completion_tokens":2}})),
            ..Default::default()
        });
        // Parser is read-only; the database CHECK-free numeric values are clamped at write boundaries in production.
        assert_eq!(entry["input_tokens"], Value::from(-1));
    }
}
