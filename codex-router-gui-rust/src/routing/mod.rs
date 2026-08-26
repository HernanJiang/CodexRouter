//! Two-level Router scheduling: Router owns cross-provider pools and total attempts;
//! CLIProxyAPI owns credential selection inside a pool.

use crate::state::StateStore;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolRoute {
    pub pool_id: String,
    pub prefix: String,
    pub public_model: String,
    pub upstream_model: String,
    pub provider: String,
    pub priority: i32,
    pub enabled: bool,
    /// True while at least one backing account is schedulable, i.e. the pool
    /// can actually serve new requests right now. Unavailable pools stay in
    /// the table so callers can tell "no route" (CR-RTE-0001) apart from
    /// "route exists but every credential is paused" (CR-RTE-0002).
    #[serde(default)]
    pub available: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteTable {
    routes: Vec<PoolRoute>,
}

impl RouteTable {
    pub fn new(mut routes: Vec<PoolRoute>) -> Result<Self> {
        routes.retain(|route| route.enabled);
        routes.sort_by(|left, right| {
            left.public_model
                .cmp(&right.public_model)
                .then(left.priority.cmp(&right.priority))
                .then(left.pool_id.cmp(&right.pool_id))
        });
        let mut public = HashSet::new();
        for route in &routes {
            if !public.insert(route.public_model.as_str()) {
                continue;
            }
        }
        let mut prefix = HashSet::new();
        for route in &routes {
            if route.prefix.starts_with("cr/") || route.prefix.contains("..") {
                bail!("unsafe internal prefix {}", route.prefix);
            }
            if !prefix.insert(route.prefix.as_str()) {
                bail!("duplicate internal prefix {}", route.prefix);
            }
        }
        Ok(Self { routes })
    }

    pub fn routes(&self) -> &[PoolRoute] {
        &self.routes
    }

    pub fn public_models(&self) -> Vec<String> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for route in &self.routes {
            if seen.insert(route.public_model.clone()) {
                models.push(route.public_model.clone());
            }
        }
        models
    }

    /// Provider-scoped view used by forced-pool surfaces (Antigravity). The
    /// routes are already enabled and priority-ordered, so filtering keeps
    /// selection semantics identical inside the view.
    pub fn filtered_by_provider(&self, provider: &str) -> RouteTable {
        RouteTable {
            routes: self
                .routes
                .iter()
                .filter(|route| route.provider == provider)
                .cloned()
                .collect(),
        }
    }

    pub fn pools(&self, public_model: &str) -> Vec<&PoolRoute> {
        self.routes
            .iter()
            .filter(|route| route.public_model == public_model)
            .collect()
    }

    pub fn rewrite_request_model(&self, public_model: &str, selected: &PoolRoute) -> String {
        format!("{}/{}", selected.prefix, public_model)
    }

    pub fn rewrite_response_model(&self, value: &mut Value, selected: &PoolRoute) {
        let internal = format!("{}/{}", selected.prefix, selected.public_model);
        fn rewrite(value: &mut Value, internal: &str, public: &str, upstream: &str) {
            match value {
                Value::String(text) => {
                    if text == internal
                        || text == upstream
                        || text.starts_with(&format!("{internal}/"))
                    {
                        *text = public.to_owned();
                    }
                }
                Value::Array(items) => items
                    .iter_mut()
                    .for_each(|item| rewrite(item, internal, public, upstream)),
                Value::Object(object) => object
                    .values_mut()
                    .for_each(|item| rewrite(item, internal, public, upstream)),
                _ => {}
            }
        }
        rewrite(
            value,
            &internal,
            &selected.public_model,
            &selected.upstream_model,
        );
    }

    pub fn continuation_key(
        &self,
        conversation_id: Option<&str>,
        previous_response_id: Option<&str>,
        prompt_cache_key: Option<&str>,
        session_header: Option<&str>,
    ) -> Option<String> {
        let identity = previous_response_id
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| conversation_id.map(str::to_owned))
            .or_else(|| prompt_cache_key.map(str::to_owned))
            .or_else(|| session_header.map(str::to_owned))?;
        Some(hex_sha256(identity.as_bytes()))
    }
}

pub fn hex_sha256(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Copy, Debug)]
pub struct AttemptBudget {
    pub max_attempts: u32,
    pub used: u32,
}

impl AttemptBudget {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            used: 0,
        }
    }

    pub fn consume(&mut self) -> Result<()> {
        self.used += 1;
        if self.used > self.max_attempts {
            bail!("attempt budget exhausted after {}", self.max_attempts);
        }
        Ok(())
    }

    pub fn remaining(&self) -> u32 {
        self.max_attempts.saturating_sub(self.used)
    }
}

#[derive(Clone, Debug)]
pub struct ContinuationBindings {
    store: HashMap<String, (String, u64)>,
}

impl ContinuationBindings {
    pub fn new() -> Self {
        Self {
            store: HashMap::new(),
        }
    }

    pub fn bind(&mut self, key: String, pool_id: String, ttl: Duration) {
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + ttl.as_secs();
        self.store.insert(key, (pool_id, expires));
    }

    pub fn pool(&self, key: &str) -> Option<&str> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.store
            .get(key)
            .and_then(|(pool, expires)| (*expires > now).then_some(pool.as_str()))
    }

    pub fn rebind_after_owner_failure(&mut self, key: &str, pool_id: String, ttl: Duration) {
        if self.pool(key).is_some() {
            self.bind(key.to_owned(), pool_id, ttl);
        }
    }

    pub fn remove_pool(&mut self, pool_id: &str) {
        self.store.retain(|_, (bound, _)| bound != pool_id);
    }
}

impl Default for ContinuationBindings {
    fn default() -> Self {
        Self::new()
    }
}

pub fn select_pool<'a>(
    table: &'a RouteTable,
    public_model: &str,
    continuation_key: Option<&str>,
    bindings: &ContinuationBindings,
    exclude: &HashSet<String>,
) -> Result<&'a PoolRoute> {
    let pools = table.pools(public_model);
    if pools.is_empty() {
        bail!("public model {public_model} has no route");
    }
    let eligible = |route: &&PoolRoute| route.available && !exclude.contains(&route.pool_id);
    if let Some(key) = continuation_key {
        if let Some(pool_id) = bindings.pool(key) {
            if !exclude.contains(pool_id) {
                match pools.iter().find(|route| route.pool_id == pool_id) {
                    Some(route) if route.available => return Ok(route),
                    // Bound pool still exists but every credential is paused:
                    // fall through to exactly one explainable rebind below.
                    Some(_) => {}
                    // Same thread switched public model. The owner is still in
                    // the table, just not for this model — rebind instead of 409.
                    None if table.routes().iter().any(|route| route.pool_id == pool_id) => {}
                    None => bail!("continuation owner {pool_id} was removed"),
                }
            }
        }
    }
    pools
        .iter()
        .copied()
        .find(eligible)
        .ok_or_else(|| anyhow::anyhow!("no available credential pool for {public_model}"))
}

/// True when another enabled, available pool for this public model remains
/// after `exclude` (failed pools) is applied.
pub fn has_fallback_pool(
    table: &RouteTable,
    public_model: &str,
    exclude: &HashSet<String>,
) -> bool {
    table
        .pools(public_model)
        .into_iter()
        .any(|route| route.available && !exclude.contains(&route.pool_id))
}

pub fn should_drop_previous_response(
    table: &RouteTable,
    bindings: &ContinuationBindings,
    key: Option<&str>,
    selected: &PoolRoute,
) -> bool {
    let Some(key) = key else {
        return false;
    };
    let Some(previous_pool) = bindings.pool(key) else {
        return false;
    };
    table
        .routes()
        .iter()
        .find(|route| route.pool_id == previous_pool)
        .is_some_and(|previous| previous.provider != selected.provider)
}

pub fn persist_binding(store: &StateStore, key: &str, pool_id: &str, ttl: Duration) -> Result<()> {
    let expires = (chrono::Utc::now() + chrono::Duration::from_std(ttl)?).to_rfc3339();
    store.with_connection(|connection| {
        connection.execute(
            "INSERT INTO continuation_bindings(session_key_hmac,pool_id,owner_state,expires_at) VALUES(?1,?2,'active',?3)
             ON CONFLICT(session_key_hmac) DO UPDATE SET pool_id=excluded.pool_id,owner_state='active',expires_at=excluded.expires_at",
            rusqlite::params![key, pool_id, expires],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(priority: i32, provider: &str) -> PoolRoute {
        PoolRoute {
            pool_id: format!("cr/model/{provider}"),
            prefix: format!("cr_model_{provider}"),
            public_model: "public".into(),
            upstream_model: "upstream".into(),
            provider: provider.into(),
            priority,
            enabled: true,
            available: true,
        }
    }

    #[test]
    fn fallback_is_explicit_and_priority_sorted() {
        let oauth = route(1, "openai");
        let api = route(10, "anthropic");
        let table = RouteTable::new(vec![api.clone(), oauth.clone()]).unwrap();
        assert_eq!(*table.pools("public")[0], oauth);
        assert_eq!(*table.pools("public")[1], api);
    }

    #[test]
    fn model_is_rewritten_both_ways_and_prefix_never_leaks() {
        let selected = route(1, "openai");
        let table = RouteTable::new(vec![selected.clone()]).unwrap();
        assert_eq!(
            table.rewrite_request_model("public", &selected),
            "cr_model_openai/public"
        );
        let mut response =
            serde_json::json!({"model":"cr_model_openai/public","nested":{"model":"upstream"}});
        table.rewrite_response_model(&mut response, &selected);
        assert_eq!(response["model"], "public");
        assert_eq!(response["nested"]["model"], "public");
    }

    #[test]
    fn continuation_binding_is_sticky_and_expires() {
        let first = route(1, "openai");
        let second = route(10, "anthropic");
        let table = RouteTable::new(vec![first, second]).unwrap();
        let mut bindings = ContinuationBindings::new();
        let key = table
            .continuation_key(None, Some("resp-1"), None, None)
            .unwrap();
        bindings.bind(
            key.clone(),
            "cr/model/openai".into(),
            Duration::from_secs(60),
        );
        assert_eq!(
            select_pool(&table, "public", Some(&key), &bindings, &HashSet::new())
                .unwrap()
                .provider,
            "openai"
        );
        bindings.bind(key.clone(), "cr/model/openai".into(), Duration::ZERO);
        assert_eq!(
            select_pool(&table, "public", Some(&key), &bindings, &HashSet::new())
                .unwrap()
                .provider,
            "openai"
        );
    }

    fn named_route(public: &str, provider: &str) -> PoolRoute {
        PoolRoute {
            pool_id: format!("cr/{public}/{provider}"),
            prefix: format!("cr_{public}_{provider}"),
            public_model: public.into(),
            upstream_model: "upstream".into(),
            provider: provider.into(),
            priority: 1,
            enabled: true,
            available: true,
        }
    }

    #[test]
    fn continuation_rebinds_when_the_same_thread_switches_public_model() {
        let chatgpt = named_route("gpt-5.6-sol", "openai");
        let deepseek = named_route("deepseek-v4-flash", "deepseek");
        let table = RouteTable::new(vec![chatgpt, deepseek]).unwrap();
        let mut bindings = ContinuationBindings::new();
        let key = table
            .continuation_key(None, Some("resp-chatgpt"), None, None)
            .unwrap();
        bindings.bind(
            key.clone(),
            "cr/gpt-5.6-sol/openai".into(),
            Duration::from_secs(60),
        );
        let selected = select_pool(
            &table,
            "deepseek-v4-flash",
            Some(&key),
            &bindings,
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(selected.provider, "deepseek");
        assert!(should_drop_previous_response(
            &table,
            &bindings,
            Some(&key),
            selected
        ));
    }

    #[test]
    fn continuation_keeps_previous_response_for_same_provider_subagent() {
        let sol = named_route("gpt-5.6-sol", "openai");
        let luna = named_route("gpt-5.6-luna", "openai");
        let table = RouteTable::new(vec![sol, luna]).unwrap();
        let mut bindings = ContinuationBindings::new();
        let key = table
            .continuation_key(None, Some("resp-sol"), None, None)
            .unwrap();
        bindings.bind(
            key.clone(),
            "cr/gpt-5.6-sol/openai".into(),
            Duration::from_secs(60),
        );
        let selected = select_pool(
            &table,
            "gpt-5.6-luna",
            Some(&key),
            &bindings,
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(selected.provider, "openai");
        assert!(!should_drop_previous_response(
            &table,
            &bindings,
            Some(&key),
            selected
        ));
    }

    #[test]
    fn continuation_still_errors_when_the_owner_pool_is_gone() {
        let deepseek = named_route("deepseek-v4-flash", "deepseek");
        let table = RouteTable::new(vec![deepseek]).unwrap();
        let mut bindings = ContinuationBindings::new();
        let key = table
            .continuation_key(None, Some("resp-gone"), None, None)
            .unwrap();
        bindings.bind(
            key.clone(),
            "cr/gone/openai".into(),
            Duration::from_secs(60),
        );
        let error = select_pool(
            &table,
            "deepseek-v4-flash",
            Some(&key),
            &bindings,
            &HashSet::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("continuation owner"), "{error}");
    }

    #[test]
    fn total_attempt_budget_is_bounded() {
        let mut budget = AttemptBudget::new(2);
        assert!(budget.consume().is_ok());
        assert!(budget.consume().is_ok());
        assert!(budget.consume().is_err());
        assert_eq!(budget.remaining(), 0);
    }

    fn openai_pools() -> (PoolRoute, PoolRoute) {
        let oauth = PoolRoute {
            pool_id: "cr/r1/openai".into(),
            prefix: "cr_r1_openai".into(),
            public_model: "gpt-5.6-sol".into(),
            upstream_model: "gpt-5.6-sol".into(),
            provider: "openai".into(),
            priority: 1,
            enabled: true,
            available: true,
        };
        let relay = PoolRoute {
            pool_id: "cr/r1f/openai".into(),
            prefix: "cr_r1f_openai".into(),
            public_model: "gpt-5.6-sol".into(),
            upstream_model: "gpt-5.6-sol".into(),
            provider: "openai".into(),
            priority: 100,
            enabled: true,
            available: true,
        };
        (oauth, relay)
    }

    #[test]
    fn same_public_model_keeps_oauth_ahead_of_relay() {
        let (oauth, relay) = openai_pools();
        let table = RouteTable::new(vec![relay.clone(), oauth.clone()]).unwrap();
        let pools = table.pools("gpt-5.6-sol");
        assert_eq!(pools[0].pool_id, oauth.pool_id);
        assert_eq!(pools[1].pool_id, relay.pool_id);
        let selected = select_pool(
            &table,
            "gpt-5.6-sol",
            None,
            &ContinuationBindings::new(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(selected.pool_id, oauth.pool_id);
    }

    #[test]
    fn excluded_oauth_pool_falls_through_to_relay() {
        let (oauth, relay) = openai_pools();
        let table = RouteTable::new(vec![oauth.clone(), relay.clone()]).unwrap();
        let mut bindings = ContinuationBindings::new();
        let key = table
            .continuation_key(None, Some("resp-sol"), None, None)
            .unwrap();
        bindings.bind(key.clone(), oauth.pool_id.clone(), Duration::from_secs(60));
        let exclude = HashSet::from([oauth.pool_id.clone()]);
        let selected = select_pool(&table, "gpt-5.6-sol", Some(&key), &bindings, &exclude).unwrap();
        assert_eq!(selected.pool_id, relay.pool_id);
        assert!(has_fallback_pool(&table, "gpt-5.6-sol", &HashSet::new()));
        assert!(!has_fallback_pool(
            &table,
            "gpt-5.6-sol",
            &HashSet::from([oauth.pool_id, relay.pool_id])
        ));
    }
}
