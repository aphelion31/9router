// Port of src/lib/db: schema, versioned migrations, repos (subset for step 1).
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// Upstream `schema.js` SCHEMA_VERSION, stored as `_meta.backupSchemaVersion`.
pub const SCHEMA_VERSION: i64 = 6;
/// Upstream `migrations/index.js` latestVersion(), stored as `_meta.schemaVersion`.
pub const MIGRATION_VERSION: i64 = 1;

const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";

/// Data file: `DATA_DIR` env, else `~/.9router`. Path = <dir>/db/data.sqlite.
pub fn data_file() -> Result<PathBuf> {
    let dir = match std::env::var("DATA_DIR") {
        Ok(d) if !d.trim().is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join(".9router"),
    };
    let db_dir = dir.join("db");
    std::fs::create_dir_all(&db_dir).context("create db dir")?;
    Ok(db_dir.join("data.sqlite"))
}

/// Open DB and run migrations. Existing data with an unknown schema version is refused.
pub fn open(path: &Path) -> Result<Connection> {
    guard_schema(path)?;
    let conn = Connection::open(path).context("open sqlite")?;
    conn.execute_batch(PRAGMAS)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Refuses to migrate a non-empty database whose schema version is not `SCHEMA_VERSION`
/// (for example a legacy Node DB). Inspection is read-only, so the file is never modified.
fn guard_schema(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let ro = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {} read-only", path.display()))?;
    let count = |sql: &str| -> Result<i64> { Ok(ro.query_row(sql, [], |r| r.get(0))?) };
    if count("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")? == 0 {
        return Ok(());
    }
    let has_meta = count("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_meta'")? > 0;
    let meta = |k: &str| -> Result<i64> {
        Ok(ro.query_row("SELECT value FROM _meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional()?.and_then(|v| v.parse().ok()).unwrap_or(0))
    };
    let (mig, back) = if has_meta { (meta("schemaVersion")?, meta("backupSchemaVersion")?) } else { (0, 0) };
    // Upstream 9router-pro DBs (and ours): known migration chain, schema not newer than ours.
    // Builds before this fix stamped schemaVersion=6 with no backupSchemaVersion; accept those too.
    if (1..=MIGRATION_VERSION).contains(&mig) && back <= SCHEMA_VERSION || (mig == SCHEMA_VERSION && back == 0) {
        return Ok(());
    }
    bail!(
        "refusing to migrate {}: schemaVersion {mig} / backupSchemaVersion {back} is not supported \
         (expected <= {MIGRATION_VERSION} / {SCHEMA_VERSION}). The file was not modified. \
         Back it up, or point DATA_DIR at an empty directory.",
        path.display()
    )
}

pub fn migrate(conn: &Connection) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!("providers/schema.json"))?;
    let tables = schema.as_object().context("invalid schema snapshot")?;
    conn.execute_batch("SAVEPOINT schema_sync")?;
    let result = (|| -> Result<()> {
        for (name, def) in tables {
            let columns = def["columns"]
                .as_object()
                .context("invalid schema columns")?;
            let mut definitions: Vec<String> = columns
                .iter()
                .map(|(name, value)| format!("\"{name}\" {}", value.as_str().unwrap()))
                .collect();
            if let Some(pk) = def["primaryKey"].as_str() {
                definitions.push(pk.to_owned());
            }
            conn.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS \"{name}\" ({})",
                definitions.join(", ")
            ))?;
            let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{name}\")"))?;
            let existing: Vec<String> = stmt
                .query_map([], |row| row.get(1))?
                .collect::<rusqlite::Result<_>>()?;
            for (column, definition) in columns {
                if !existing.contains(column) {
                    let definition = definition
                        .as_str()
                        .context("invalid column definition")?
                        .replace("PRIMARY KEY AUTOINCREMENT", "")
                        .replace("PRIMARY KEY", "")
                        .replace("UNIQUE", "");
                    conn.execute_batch(&format!(
                        "ALTER TABLE \"{name}\" ADD COLUMN \"{column}\" {definition}"
                    ))?;
                }
            }
            if let Some(indexes) = def["indexes"].as_array() {
                for index in indexes {
                    conn.execute_batch(index.as_str().context("invalid index")?)?;
                }
            }
        }
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_settings_id ON settings(id)",
        )?;
        // Same two stamps as upstream migrate.js: migration chain + backup schema version.
        for (key, value) in [("schemaVersion", MIGRATION_VERSION), ("backupSchemaVersion", SCHEMA_VERSION)] {
            conn.execute(
                "INSERT INTO _meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value.to_string()],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE schema_sync")?,
        Err(err) => {
            conn.execute_batch("ROLLBACK TO schema_sync; RELEASE schema_sync")?;
            return Err(err);
        }
    }
    Ok(())
}

#[cfg(test)]
pub fn schema_version(conn: &Connection) -> Result<i64> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM _meta WHERE key = 'schemaVersion'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
}

// ---- kv repo ----

pub fn kv_set(conn: &Connection, scope: &str, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv(scope, key, value) VALUES(?1, ?2, ?3)
         ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
        params![scope, key, value],
    )?;
    Ok(())
}

pub fn kv_get(conn: &Connection, scope: &str, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM kv WHERE scope = ?1 AND key = ?2",
            params![scope, key],
            |r| r.get(0),
        )
        .optional()?)
}

// ---- usage repo ----

#[derive(Debug, Clone, Default)]
pub struct UsageRow {
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub connection_id: Option<String>,
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub status: i64,
    pub latency_ms: i64,
    pub request_id: String,
    /// Extra metadata merged into `meta` (accrual basis, search reason). Null means none.
    pub meta: serde_json::Value,
}

/// Port of upstream `saveRequestUsage`: price the row, insert it, fold it into the
/// `usageDaily` rollup for its local date and bump `totalRequestsLifetime`, all in
/// one transaction. Returns `false` when a row with the same `requestId` already exists.
pub fn usage_record(conn: &Connection, r: &UsageRow, tokens: &crate::services::pricing::TokenCounts, cost_override: Option<f64>) -> Result<bool> {
    let cost = cost_override.unwrap_or_else(|| crate::services::pricing::cost_for(conn, &r.provider, &r.model, tokens));
    let tx = conn.unchecked_transaction()?;
    if !r.request_id.is_empty() {
        let dup: Option<i64> = tx.query_row("SELECT id FROM usageHistory WHERE requestId = ?1 ORDER BY id DESC LIMIT 1", [&r.request_id], |x| x.get(0)).optional()?;
        if dup.is_some() { return Ok(false); }
    }
    let mut meta_obj = if r.meta.is_object() { r.meta.clone() } else { serde_json::json!({}) };
    meta_obj["latencyMs"] = serde_json::json!(r.latency_ms);
    let meta = meta_obj.to_string();
    let tokens_json = serde_json::json!({ "prompt_tokens": tokens.prompt_tokens, "completion_tokens": tokens.completion_tokens, "cached_tokens": tokens.cached_tokens, "cache_creation_input_tokens": tokens.cache_creation_tokens, "reasoning_tokens": tokens.reasoning_tokens }).to_string();
    tx.execute(
        "INSERT INTO usageHistory(timestamp, provider, model, connectionId, apiKey, endpoint, promptTokens, completionTokens, cost, status, tokens, meta, requestId)
         VALUES(COALESCE(?1, strftime('%Y-%m-%dT%H:%M:%fZ','now')), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            if r.timestamp.is_empty() { None } else { Some(r.timestamp.as_str()) }, r.provider, r.model, r.connection_id, r.api_key, r.endpoint,
            tokens.prompt_tokens, tokens.completion_tokens, cost, r.status.to_string(), tokens_json, meta, r.request_id
        ],
    )?;
    let date_key: String = tx.query_row("SELECT date(COALESCE(?1, 'now'), 'localtime')", params![if r.timestamp.is_empty() { None } else { Some(r.timestamp.as_str()) }], |x| x.get(0))?;
    let existing: Option<String> = tx.query_row("SELECT data FROM usageDaily WHERE dateKey = ?1", [&date_key], |x| x.get(0)).optional()?;
    let mut day: serde_json::Value = existing.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({"requests":0,"promptTokens":0,"completionTokens":0,"cost":0,"byProvider":{},"byModel":{},"byAccount":{},"byApiKey":{},"byEndpoint":{}}));
    fold_into_day(&mut day, r, tokens, cost);
    tx.execute("INSERT INTO usageDaily(dateKey, data) VALUES(?1, ?2) ON CONFLICT(dateKey) DO UPDATE SET data = excluded.data", params![date_key, day.to_string()])?;
    let lifetime: i64 = tx.query_row("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'", [], |x| x.get::<_, String>(0)).optional()?.and_then(|v| v.parse().ok()).unwrap_or(0);
    tx.execute("INSERT INTO _meta(key, value) VALUES('totalRequestsLifetime', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [(lifetime + 1).to_string()])?;
    tx.commit()?;
    Ok(true)
}

/// Upstream `aggregateEntryToDay` + `addToCounter`: totals plus per-provider, model,
/// account, api-key and endpoint buckets. Keys match upstream composite forms.
fn fold_into_day(day: &mut serde_json::Value, r: &UsageRow, t: &crate::services::pricing::TokenCounts, cost: f64) {
    use serde_json::{Map, Value, json};
    // Adds one usage row into a counter bucket (upstream `addToCounter`).
    fn bump(bucket: &mut Map<String, Value>, key: &str, t: &crate::services::pricing::TokenCounts, cost: f64, meta: Option<Map<String, Value>>) {
        let e = bucket.entry(key.to_string()).or_insert_with(|| json!({"requests":0,"promptTokens":0,"completionTokens":0,"cachedTokens":0,"cost":0.0}));
        let obj = e.as_object_mut().expect("bucket is an object");
        let f = |o: &Map<String, Value>, k: &str| o.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let req = f(obj, "requests") + 1.0;
        let pt = f(obj, "promptTokens") + t.prompt_tokens as f64;
        let ct = f(obj, "completionTokens") + t.completion_tokens as f64;
        let cached = f(obj, "cachedTokens") + t.cached_tokens as f64;
        let c = f(obj, "cost") + cost;
        obj.insert("requests".into(), json!(req));
        obj.insert("promptTokens".into(), json!(pt));
        obj.insert("completionTokens".into(), json!(ct));
        obj.insert("cachedTokens".into(), json!(cached));
        obj.insert("cost".into(), json!(c));
        if let Some(m) = meta {
            for (k, v) in m {
                obj.insert(k, v);
            }
        }
    }
    let g = |o: &Value, k: &str| o[k].as_f64().unwrap_or(0.0);
    let (req, pt, ct, cached, total_cost) = (
        g(day, "requests") + 1.0,
        g(day, "promptTokens") + t.prompt_tokens as f64,
        g(day, "completionTokens") + t.completion_tokens as f64,
        g(day, "cachedTokens") + t.cached_tokens as f64,
        g(day, "cost") + cost,
    );
    day["requests"] = json!(req);
    day["promptTokens"] = json!(pt);
    day["completionTokens"] = json!(ct);
    day["cachedTokens"] = json!(cached);
    day["cost"] = json!(total_cost);
    let provider = if r.provider.is_empty() { None } else { Some(r.provider.clone()) };
    let prov_str = provider.clone().unwrap_or_else(|| "unknown".into());
    let raw_meta = |extra: Map<String, Value>| {
        let mut m = Map::new();
        m.insert("rawModel".into(), json!(r.model));
        m.insert("provider".into(), json!(provider));
        m.extend(extra);
        m
    };
    if let Some(p) = &provider {
        if let Some(b) = day["byProvider"].as_object_mut() { bump(b, p, t, cost, None); }
    }
    let model_key = match &provider { Some(p) => format!("{}|{}", r.model, p), None => r.model.clone() };
    if let Some(b) = day["byModel"].as_object_mut() { bump(b, &model_key, t, cost, Some(raw_meta(Map::new()))); }
    if let Some(c) = &r.connection_id {
        if let Some(b) = day["byAccount"].as_object_mut() { bump(b, c, t, cost, Some(raw_meta(Map::new()))); }
    }
    let key_raw = r.api_key.as_deref().filter(|k| !k.is_empty());
    let ak_key = match key_raw {
        Some(k) => api_key_bucket(k, &r.model, &prov_str),
        None => format!("local-no-key|{}|{}", r.model, prov_str),
    };
    let mut ak_extra = Map::new();
    ak_extra.insert("apiKeyMasked".into(), json!(key_raw.map(mask_key)));
    if let Some(b) = day["byApiKey"].as_object_mut() { bump(b, &ak_key, t, cost, Some(raw_meta(ak_extra))); }
    let endpoint = r.endpoint.clone().filter(|e| !e.is_empty()).unwrap_or_else(|| "Unknown".into());
    let ep_key = format!("{}|{}|{}", endpoint, r.model, prov_str);
    let mut ep_extra = Map::new();
    ep_extra.insert("endpoint".into(), json!(endpoint));
    if let Some(b) = day["byEndpoint"].as_object_mut() { bump(b, &ep_key, t, cost, Some(raw_meta(ep_extra))); }
}

/// Upstream `maskApiKey`: first 8 + `***` + last 4, short keys keep one char.
pub fn mask_key(key: &str) -> String {
    let t = key.trim();
    let c: Vec<char> = t.chars().collect();
    if c.len() <= 12 { return format!("{}***", c.first().map(|x| x.to_string()).unwrap_or_default()); }
    format!("{}***{}", c[..8].iter().collect::<String>(), c[c.len() - 4..].iter().collect::<String>())
}

/// Upstream `apiKeyBucketKey`: `mask#sha256[..10]|model|provider`. The raw key never lands in the rollup.
pub fn api_key_bucket(key: &str, model: &str, provider: &str) -> String {
    use ring::digest::{SHA256, digest};
    let hex: String = digest(&SHA256, key.trim().as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("{}#{}|{}|{}", mask_key(key), &hex[..10], model, provider)
}

/// Totals per provider and model for the last `since` (ISO-8601 lower bound), plus grand totals.
pub fn usage_summary(conn: &Connection, since: &str) -> Result<serde_json::Value> {
    let mut stmt = conn.prepare(
        "SELECT provider, model, COUNT(*), COALESCE(SUM(promptTokens),0), COALESCE(SUM(completionTokens),0),
                SUM(CASE WHEN status NOT LIKE '2%' THEN 1 ELSE 0 END)
         FROM usageHistory WHERE timestamp >= ?1 GROUP BY provider, model ORDER BY COUNT(*) DESC",
    )?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map([since], |r| {
            Ok(serde_json::json!({
                "provider": r.get::<_, Option<String>>(0)?,
                "model": r.get::<_, Option<String>>(1)?,
                "requests": r.get::<_, i64>(2)?,
                "promptTokens": r.get::<_, i64>(3)?,
                "completionTokens": r.get::<_, i64>(4)?,
                "errors": r.get::<_, i64>(5)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let total = |k: &str| rows.iter().filter_map(|r| r[k].as_i64()).sum::<i64>();
    Ok(serde_json::json!({
        "since": since,
        "totalRequests": total("requests"),
        "totalPromptTokens": total("promptTokens"),
        "totalCompletionTokens": total("completionTokens"),
        "totalErrors": total("errors"),
        "byModel": rows,
    }))
}

// ---- settings repo ----

/// Settings row `id = 1`. Missing row reads as an empty object.
pub fn settings_get(conn: &Connection) -> Result<serde_json::Value> {
    let data: Option<String> = conn
        .query_row("SELECT data FROM settings WHERE id = 1", [], |r| r.get(0))
        .optional()?;
    Ok(match data {
        Some(d) => serde_json::from_str(&d).unwrap_or_else(|_| serde_json::json!({})),
        None => serde_json::json!({}),
    })
}

pub fn settings_put(conn: &Connection, value: &serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(id, data) VALUES(1, ?1)
         ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        params![value.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(PRAGMAS).unwrap();
        migrate(&c).unwrap();
        c
    }

    #[test]
    fn migrations_idempotent_and_versioned() {
        let c = mem();
        assert_eq!(schema_version(&c).unwrap(), MIGRATION_VERSION);
        migrate(&c).unwrap();
        assert_eq!(schema_version(&c).unwrap(), MIGRATION_VERSION);
    }

    #[test]
    fn schema_covers_upstream_and_upgrades_existing_tables() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE apiKeys (id TEXT PRIMARY KEY, key TEXT UNIQUE NOT NULL, name TEXT, machineId TEXT, isActive INTEGER DEFAULT 1, createdAt TEXT NOT NULL); INSERT INTO apiKeys (id, key, createdAt) VALUES ('1', 'secret', 'now');").unwrap();
        migrate(&c).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("providers/schema.json")).unwrap();
        for (table, def) in schema.as_object().unwrap() {
            let mut stmt = c
                .prepare(&format!("PRAGMA table_info(\"{table}\")"))
                .unwrap();
            let names: Vec<String> = stmt
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            for column in def["columns"].as_object().unwrap().keys() {
                assert!(names.contains(column), "missing {table}.{column}");
            }
        }
        assert_eq!(
            c.query_row(
                "SELECT accessRestricted FROM apiKeys WHERE id='1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn usage_insert_and_summary() {
        let c = mem();
        let row = |model: &str, status: i64| UsageRow {
            timestamp: "2026-11-13T00:00:00.000Z".into(),
            provider: "openai".into(),
            model: model.into(),
            prompt_tokens: 10,
            completion_tokens: 4,
            status,
            latency_ms: 12,
            request_id: format!("r-{status}"),
            ..Default::default()
        };
        usage_record(&c, &row("gpt", 200), &crate::services::pricing::TokenCounts { prompt_tokens: 10, completion_tokens: 4, ..Default::default() }, None).unwrap();
        usage_record(&c, &row("gpt", 502), &crate::services::pricing::TokenCounts { prompt_tokens: 10, completion_tokens: 4, ..Default::default() }, None).unwrap();
        let s = usage_summary(&c, "2026-01-01T00:00:00.000Z").unwrap();
        assert_eq!(s["totalRequests"], 2);
        assert_eq!(s["totalPromptTokens"], 20);
        assert_eq!(s["totalCompletionTokens"], 8);
        assert_eq!(s["totalErrors"], 1);
        assert_eq!(usage_summary(&c, "2027-01-01T00:00:00.000Z").unwrap()["totalRequests"], 0);
    }

    // Upstream saveRequestUsage: usageDaily totals + byProvider/byModel/byApiKey buckets
    // and the totalRequestsLifetime counter all move together, cost included.
    #[test]
    fn usage_record_rolls_up_daily_and_lifetime_like_upstream() {
        let c = mem();
        let row = UsageRow {
            timestamp: "2026-11-13T00:00:00.000Z".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            api_key: Some("sk-abcdefghijklmnop".into()),
            endpoint: Some("/v1/chat/completions".into()),
            request_id: "rollup-1".into(),
            status: 200,
            ..Default::default()
        };
        let t = crate::services::pricing::TokenCounts { prompt_tokens: 1000, completion_tokens: 500, ..Default::default() };
        assert!(usage_record(&c, &row, &t, None).unwrap());
        // Same requestId is a duplicate: nothing moves.
        assert!(!usage_record(&c, &row, &t, None).unwrap());
        let (day_json, day_key): (String, String) = c.query_row("SELECT data, dateKey FROM usageDaily", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let day: serde_json::Value = serde_json::from_str(&day_json).unwrap();
        assert_eq!(day_key, "2026-11-13");
        assert_eq!(day["requests"], 1.0);
        assert!((day["cost"].as_f64().unwrap() - 0.0075).abs() < 1e-12);
        assert_eq!(day["byProvider"]["openai"]["requests"], 1.0);
        assert!((day["byModel"]["gpt-4o|openai"]["cost"].as_f64().unwrap() - 0.0075).abs() < 1e-12);
        // Raw key never lands in the rollup; only the masked bucket key.
        let ak = day["byApiKey"].as_object().unwrap().keys().next().unwrap().clone();
        assert!(ak.starts_with("sk-abcde***mnop#") && !ak.contains("sk-abcdefghijklmnop"), "{ak}");
        assert_eq!(c.query_row("SELECT value FROM _meta WHERE key='totalRequestsLifetime'", [], |r| r.get::<_, String>(0)).unwrap(), "1");
        let cost: f64 = c.query_row("SELECT cost FROM usageHistory", [], |r| r.get(0)).unwrap();
        assert!((cost - 0.0075).abs() < 1e-12);
    }

    #[test]
    fn settings_roundtrip_and_default() {
        let c = mem();
        assert_eq!(settings_get(&c).unwrap(), serde_json::json!({}));
        settings_put(&c, &serde_json::json!({"a": 1})).unwrap();
        settings_put(&c, &serde_json::json!({"a": 2})).unwrap();
        assert_eq!(settings_get(&c).unwrap(), serde_json::json!({"a": 2}));
    }

    #[test]
    fn upstream_production_meta_is_accepted() {
        // Real 9router-pro 0.5.109 DB: schemaVersion=1 (migration), backupSchemaVersion=6.
        let dir = std::env::temp_dir().join(format!("nr-up-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(PRAGMAS).unwrap();
            migrate(&c).unwrap();
            c.execute_batch("UPDATE _meta SET value='1' WHERE key='schemaVersion'; UPDATE _meta SET value='6' WHERE key='backupSchemaVersion'; INSERT INTO apiKeys(id,key,createdAt) VALUES('k','s','now');").unwrap();
        }
        let c = open(&path).unwrap();
        assert_eq!(schema_version(&c).unwrap(), 1);
        drop(c);
        // A newer upstream schema is still refused.
        Connection::open(&path).unwrap().execute_batch("UPDATE _meta SET value='7' WHERE key='backupSchemaVersion'").unwrap();
        assert!(open(&path).unwrap_err().to_string().contains("refusing to migrate"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_db_with_unknown_schema_is_refused_untouched() {
        let dir = std::env::temp_dir().join(format!("nr-guard-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE apiKeys (id TEXT PRIMARY KEY, key TEXT); INSERT INTO apiKeys VALUES ('1','secret');").unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let err = open(&path).unwrap_err().to_string();
        assert!(err.contains("refusing to migrate"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fresh_and_current_db_open() {
        let dir = std::env::temp_dir().join(format!("nr-fresh-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.sqlite");
        drop(open(&path).unwrap());
        drop(open(&path).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kv_roundtrip() {
        let c = mem();
        assert_eq!(kv_get(&c, "s", "k").unwrap(), None);
        kv_set(&c, "s", "k", "v1").unwrap();
        kv_set(&c, "s", "k", "v2").unwrap();
        assert_eq!(kv_get(&c, "s", "k").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn combo_name_case_insensitive_unique() {
        let c = mem();
        c.execute("INSERT INTO combos(id,name,models,createdAt,updatedAt) VALUES(?1,?2,'[]','t','t')", params!["1", "Fast"]).unwrap();
        assert!(c.execute("INSERT INTO combos(id,name,models,createdAt,updatedAt) VALUES(?1,?2,'[]','t','t')", params!["2", "fast"]).is_err());
    }
}
