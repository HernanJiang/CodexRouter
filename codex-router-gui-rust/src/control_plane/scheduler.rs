//! Persistent scheduled account probes.

use super::account_probe::{self, ProbeFailure, ProbeSuccess};
use super::http_compat::{sync_backend, ControlState};
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use cron::Schedule;
use serde_json::{json, Value};
use std::str::FromStr;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct ScheduledPlan {
    id: i64,
    account_id: i64,
    model: String,
    cron: String,
    auto_recover: bool,
    max_results: i64,
}

fn normalized_cron(expression: &str) -> Result<String> {
    let expression = expression.trim();
    let fields = expression.split_whitespace().count();
    let normalized = match fields {
        5 => format!("0 {expression}"),
        6 | 7 => expression.to_owned(),
        _ => anyhow::bail!("cron expression must contain 5, 6 or 7 fields"),
    };
    Schedule::from_str(&normalized).context("parse cron expression")?;
    Ok(normalized)
}

pub fn next_run(expression: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let schedule = Schedule::from_str(&normalized_cron(expression)?)?;
    schedule
        .after(&after)
        .next()
        .context("cron expression has no future occurrence")
}

pub fn next_run_text(expression: &str, after: DateTime<Utc>) -> Result<String> {
    Ok(next_run(expression, after)?.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub fn validate_cron(expression: &str) -> Result<()> {
    let _ = next_run(expression, Utc::now())?;
    Ok(())
}

pub fn initialize(store: &crate::state::StateStore) -> Result<()> {
    let now = Utc::now();
    store.with_connection(|connection| {
        connection.execute(
            "UPDATE scheduled_test_results
             SET status='failed',error_code='CR-SYS-0002',
                 details=json_set(details,'$.reason','host_restarted_before_completion')
             WHERE status='running'",
            [],
        )?;
        let mut statement = connection.prepare(
            "SELECT id,cron FROM scheduled_test_plans
             WHERE enabled=1 AND (next_run IS NULL OR trim(next_run)='')",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for (id, expression) in rows {
            match next_run_text(&expression, now) {
                Ok(next) => {
                    connection.execute(
                        "UPDATE scheduled_test_plans SET next_run=?2 WHERE id=?1",
                        rusqlite::params![id, next],
                    )?;
                }
                Err(_) => {
                    connection.execute(
                        "UPDATE scheduled_test_plans SET enabled=0,next_run=NULL WHERE id=?1",
                        rusqlite::params![id],
                    )?;
                }
            }
        }
        Ok(())
    })
}

fn due_plans(state: &ControlState, now: DateTime<Utc>) -> Result<Vec<ScheduledPlan>> {
    let now_text = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    state.store.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id,account_id,model,cron,auto_recover,max_results
             FROM scheduled_test_plans
             WHERE enabled=1 AND next_run IS NOT NULL AND next_run<=?1
             ORDER BY next_run,id",
        )?;
        let rows = statement.query_map(rusqlite::params![now_text], |row| {
            Ok(ScheduledPlan {
                id: row.get(0)?,
                account_id: row.get(1)?,
                model: row.get(2)?,
                cron: row.get(3)?,
                auto_recover: row.get::<_, i64>(4)? != 0,
                max_results: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    })
}

fn claim_plan(
    state: &ControlState,
    plan: &ScheduledPlan,
    started_at: DateTime<Utc>,
) -> Result<Option<i64>> {
    let started_text = started_at.to_rfc3339_opts(SecondsFormat::Millis, true);
    let next_text = next_run_text(&plan.cron, started_at)?;
    state.store.with_connection(|connection| {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<Option<i64>> {
            connection.execute(
                "UPDATE scheduled_test_plans
                 SET last_run=?2,next_run=?3
                 WHERE id=?1 AND enabled=1 AND next_run IS NOT NULL AND next_run<=?2",
                rusqlite::params![plan.id, started_text, next_text],
            )?;
            if connection.changes() != 1 {
                return Ok(None);
            }
            connection.execute(
                "INSERT INTO scheduled_test_results(plan_id,status,details,created_at)
                 VALUES(?1,'running',?2,?3)",
                rusqlite::params![
                    plan.id,
                    json!({
                        "account_id":plan.account_id,
                        "model":plan.model,
                        "started_at":started_text,
                    })
                    .to_string(),
                    started_text,
                ],
            )?;
            Ok(Some(connection.last_insert_rowid()))
        })();
        if result.is_ok() {
            connection.execute_batch("COMMIT")?;
        } else {
            let _ = connection.execute_batch("ROLLBACK");
        }
        result
    })
}

fn result_details_success(plan: &ScheduledPlan, probe: &ProbeSuccess) -> Value {
    json!({
        "account_id":plan.account_id,
        "model":probe.model,
        "latency_ms":probe.latency_ms,
        "upstream_status":probe.upstream_status,
        "finished_at":Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    })
}

fn result_details_failure(plan: &ScheduledPlan, failure: &ProbeFailure) -> Value {
    json!({
        "account_id":plan.account_id,
        "model":plan.model,
        "latency_ms":failure.latency_ms,
        "upstream_status":failure.upstream_status,
        "finished_at":Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    })
}

fn finish_result(
    state: &ControlState,
    plan: &ScheduledPlan,
    result_id: i64,
    status: &str,
    error_code: Option<&str>,
    details: &Value,
) -> Result<()> {
    state.store.with_connection(|connection| {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            connection.execute(
                "UPDATE scheduled_test_results
                 SET status=?2,error_code=?3,details=?4 WHERE id=?1",
                rusqlite::params![result_id, status, error_code, details.to_string()],
            )?;
            connection.execute(
                "DELETE FROM scheduled_test_results
                 WHERE plan_id=?1 AND id NOT IN (
                    SELECT id FROM scheduled_test_results
                    WHERE plan_id=?1 ORDER BY id DESC LIMIT ?2
                 )",
                rusqlite::params![plan.id, plan.max_results.clamp(1, 1000)],
            )?;
            Ok(())
        })();
        if result.is_ok() {
            connection.execute_batch("COMMIT")?;
        } else {
            let _ = connection.execute_batch("ROLLBACK");
        }
        result
    })
}

async fn auto_recover(state: &ControlState, plan: &ScheduledPlan) -> Result<bool> {
    if !plan.auto_recover {
        return Ok(false);
    }
    let changed = state.store.with_connection(|connection| {
        connection.execute(
            "UPDATE accounts
             SET status='active',schedulable=1,updated_at=CURRENT_TIMESTAMP
             WHERE id=?1 AND deleted_at IS NULL
               AND (status<>'active' OR schedulable<>1)",
            rusqlite::params![plan.account_id],
        )?;
        Ok(connection.changes() == 1)
    })?;
    if changed {
        sync_backend(state).await?;
    }
    Ok(changed)
}

async fn execute_claimed(state: &ControlState, plan: &ScheduledPlan, result_id: i64) {
    let result = account_probe::probe_account(
        &state.store,
        &state.cli,
        &state.cli_index_map,
        plan.account_id,
        &plan.model,
    )
    .await;
    match result {
        Ok(probe) => {
            let recovered = match auto_recover(state, plan).await {
                Ok(recovered) => recovered,
                Err(error) => {
                    let details = json!({
                        "account_id":plan.account_id,
                        "model":plan.model,
                        "latency_ms":probe.latency_ms,
                        "upstream_status":probe.upstream_status,
                        "recovery_error":"backend_sync_failed",
                    });
                    let _ = finish_result(
                        state,
                        plan,
                        result_id,
                        "failed",
                        Some("CR-CFG-0005"),
                        &details,
                    );
                    let _ = state.logger.write(json!({
                        "level":"ERROR",
                        "event":"scheduler.auto_recover_failed",
                        "plan_id":plan.id,
                        "account_id":plan.account_id,
                        "error_description":error.to_string(),
                    }));
                    return;
                }
            };
            let mut details = result_details_success(plan, &probe);
            details["recovered"] = Value::Bool(recovered);
            let _ = finish_result(state, plan, result_id, "success", None, &details);
            let _ = state.logger.write(json!({
                "level":"INFO",
                "event":"scheduler.probe_completed",
                "plan_id":plan.id,
                "account_id":plan.account_id,
                "latency_ms":probe.latency_ms,
                "recovered":recovered,
            }));
        }
        Err(failure) => {
            let details = result_details_failure(plan, &failure);
            let _ = finish_result(
                state,
                plan,
                result_id,
                "failed",
                Some(failure.error_code),
                &details,
            );
            let _ = state.logger.write(json!({
                "level":"INFO",
                "event":"scheduler.probe_failed",
                "plan_id":plan.id,
                "account_id":plan.account_id,
                "error_code":failure.error_code,
                "upstream_status":failure.upstream_status,
                "latency_ms":failure.latency_ms,
            }));
        }
    }
}

/// Execute every currently due plan. Work is deliberately serial: this gives
/// strict non-overlap for the same account and keeps SQLite write ordering
/// deterministic. Scheduled probes are infrequent and bounded by CLI timeout.
pub async fn run_due_once(state: &ControlState) -> Result<usize> {
    let now = Utc::now();
    let plans = due_plans(state, now)?;
    let mut ran = 0;
    let mut seen_accounts = std::collections::HashSet::new();
    for plan in plans {
        if !seen_accounts.insert(plan.account_id) {
            continue;
        }
        let Some(result_id) = claim_plan(state, &plan, now)? else {
            continue;
        };
        execute_claimed(state, &plan, result_id).await;
        ran += 1;
    }
    Ok(ran)
}

pub async fn run(state: ControlState) {
    if let Err(error) = initialize(&state.store) {
        let _ = state.logger.write(json!({
            "level":"ERROR",
            "event":"scheduler.initialize_failed",
            "error_description":error.to_string(),
        }));
    }
    loop {
        if let Err(error) = run_due_once(&state).await {
            let _ = state.logger.write(json!({
                "level":"ERROR",
                "event":"scheduler.tick_failed",
                "error_description":error.to_string(),
            }));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::cli_proxy::CliProxyManagementClient;
    use crate::routing::RouteTable;
    use crate::state::StateStore;
    use crate::telemetry::structured_log::StructuredLogger;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, RwLock};

    #[derive(Clone)]
    struct ProbeMock {
        status: u16,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn mock_probe(State(mock): State<ProbeMock>, Json(request): Json<Value>) -> Json<Value> {
        mock.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        Json(json!({
            "status_code":mock.status,
            "header":{},
            "body":"{}"
        }))
    }

    async fn test_state(
        status: u16,
    ) -> (
        ControlState,
        Arc<Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let root = std::env::temp_dir().join(format!("router-scheduler-{}", uuid::Uuid::now_v7()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn({
            let requests = requests.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/v0/management/api-call", post(mock_probe))
                        .with_state(ProbeMock { status, requests }),
                )
                .await
                .unwrap();
            }
        });
        let store = Arc::new(StateStore::open(root.join("router-state.sqlite3")).unwrap());
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO accounts(id,platform,account_type,auth_index,auth_file,
                        stable_identity_hmac,status,schedulable,priority,weight,payload)
                     VALUES(1,'openai','api','stored-index','','scheduler-account',
                        'error',0,1,1,'{}')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let state = ControlState {
            store,
            cli: CliProxyManagementClient::new(
                format!("http://{address}"),
                "test-management-secret",
            )
            .unwrap(),
            logger: Arc::new(StructuredLogger::open(root.join("router-events.jsonl")).unwrap()),
            routes: Arc::new(RwLock::new(RouteTable::new(Vec::new()).unwrap())),
            backend: None,
            cli_index_map: Arc::new(RwLock::new(HashMap::from([(
                "runtime-index".to_owned(),
                1,
            )]))),
        };
        (state, requests, server)
    }

    fn insert_due_plan(state: &ControlState, auto_recover: bool, max_results: i64) -> i64 {
        state
            .store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO scheduled_test_plans(
                        account_id,model,cron,enabled,auto_recover,max_results,next_run,payload)
                     VALUES(1,'gpt-test','* * * * * *',1,?1,?2,
                        '2000-01-01T00:00:00.000Z','{}')",
                    rusqlite::params![auto_recover, max_results],
                )?;
                Ok(connection.last_insert_rowid())
            })
            .unwrap()
    }

    #[test]
    fn standard_five_field_cron_is_supported() {
        let after = DateTime::parse_from_rfc3339("2026-08-19T10:15:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            next_run("0 * * * *", after).unwrap().to_rfc3339(),
            "2026-08-19T11:00:00+00:00"
        );
        assert!(validate_cron("bad cron").is_err());
    }

    #[test]
    fn initialize_repairs_interrupted_results_and_plan_schedules() {
        let root =
            std::env::temp_dir().join(format!("router-scheduler-init-{}", uuid::Uuid::now_v7()));
        let store = StateStore::open(root.join("router-state.sqlite3")).unwrap();
        store
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO accounts(id,platform,account_type,auth_index,auth_file,
                        stable_identity_hmac,priority,weight,payload)
                     VALUES(1,'openai','api','index','','scheduler-init',1,1,'{}')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO scheduled_test_plans(account_id,model,cron,next_run,payload)
                     VALUES(1,'valid','0 * * * *',NULL,'{}')",
                    [],
                )?;
                let valid_id = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO scheduled_test_plans(account_id,model,cron,next_run,payload)
                     VALUES(1,'invalid','invalid',NULL,'{}')",
                    [],
                )?;
                let invalid_id = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO scheduled_test_results(plan_id,status,details)
                     VALUES(?1,'running','{}')",
                    rusqlite::params![valid_id],
                )?;
                Ok((valid_id, invalid_id))
            })
            .and_then(|(valid_id, invalid_id)| {
                initialize(&store)?;
                store.with_connection(|connection| {
                    let interrupted: (String, Option<String>, String) = connection.query_row(
                        "SELECT status,error_code,details FROM scheduled_test_results",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?;
                    assert_eq!(interrupted.0, "failed");
                    assert_eq!(interrupted.1.as_deref(), Some("CR-SYS-0002"));
                    assert_eq!(
                        serde_json::from_str::<Value>(&interrupted.2)?["reason"],
                        "host_restarted_before_completion"
                    );
                    let valid: (i64, Option<String>) = connection.query_row(
                        "SELECT enabled,next_run FROM scheduled_test_plans WHERE id=?1",
                        rusqlite::params![valid_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    assert_eq!(valid.0, 1);
                    assert!(valid.1.is_some());
                    let invalid: (i64, Option<String>) = connection.query_row(
                        "SELECT enabled,next_run FROM scheduled_test_plans WHERE id=?1",
                        rusqlite::params![invalid_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    assert_eq!(invalid, (0, None));
                    Ok(())
                })
            })
            .unwrap();
    }

    #[tokio::test]
    async fn due_probe_persists_result_recovers_and_enforces_retention() {
        let (state, requests, server) = test_state(200).await;
        let plan_id = insert_due_plan(&state, true, 2);
        state
            .store
            .with_connection(|connection| {
                for _ in 0..2 {
                    connection.execute(
                        "INSERT INTO scheduled_test_results(plan_id,status,details)
                         VALUES(?1,'failed','{}')",
                        rusqlite::params![plan_id],
                    )?;
                }
                Ok(())
            })
            .unwrap();

        assert_eq!(run_due_once(&state).await.unwrap(), 1);
        let request = requests.lock().unwrap_or_else(|error| error.into_inner())[0].clone();
        assert_eq!(request["auth_index"], "runtime-index");
        state
            .store
            .with_connection(|connection| {
                let account: (String, i64) = connection.query_row(
                    "SELECT status,schedulable FROM accounts WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(account, ("active".to_owned(), 1));
                let count: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM scheduled_test_results WHERE plan_id=?1",
                    rusqlite::params![plan_id],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 2);
                let latest: (String, Option<String>, String) = connection.query_row(
                    "SELECT status,error_code,details FROM scheduled_test_results
                     WHERE plan_id=?1 ORDER BY id DESC LIMIT 1",
                    rusqlite::params![plan_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!(latest.0, "success");
                assert_eq!(latest.1, None);
                assert_eq!(serde_json::from_str::<Value>(&latest.2)?["recovered"], true);
                Ok(())
            })
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn failed_probe_keeps_isolation_and_same_account_runs_once_per_tick() {
        let (state, requests, server) = test_state(401).await;
        let first = insert_due_plan(&state, true, 20);
        let second = insert_due_plan(&state, true, 20);

        assert_eq!(run_due_once(&state).await.unwrap(), 1);
        assert_eq!(
            requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
        state
            .store
            .with_connection(|connection| {
                let account: (String, i64) = connection.query_row(
                    "SELECT status,schedulable FROM accounts WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(account, ("error".to_owned(), 0));
                let result: (i64, String, Option<String>) = connection.query_row(
                    "SELECT plan_id,status,error_code FROM scheduled_test_results",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!(
                    result,
                    (first, "failed".to_owned(), Some("CR-UP-0002".to_owned()))
                );
                let second_next: String = connection.query_row(
                    "SELECT next_run FROM scheduled_test_plans WHERE id=?1",
                    rusqlite::params![second],
                    |row| row.get(0),
                )?;
                assert_eq!(second_next, "2000-01-01T00:00:00.000Z");
                Ok(())
            })
            .unwrap();
        server.abort();
    }
}
