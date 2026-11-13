// Port of src/lib/db: schema, versioned migrations, repos (subset for step 1).
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 6;

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
    let found = if has_meta { Some(schema_version(&ro)?).filter(|v| *v != 0) } else { None };
    if found == Some(SCHEMA_VERSION) {
        return Ok(());
    }
    let found = found.map_or("none".to_string(), |v| v.to_string());
    bail!(
        "refusing to migrate {}: schema version {found} is not supported (expected {SCHEMA_VERSION}). \
         The file was not modified. Back it up, or point DATA_DIR at an empty directory.",
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
        if schema_version(conn)? < SCHEMA_VERSION {
            conn.execute(
                "INSERT INTO _meta(key, value) VALUES('schemaVersion', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![SCHEMA_VERSION.to_string()],
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
}

pub fn usage_insert(conn: &Connection, r: &UsageRow) -> Result<()> {
    let meta = serde_json::json!({ "latencyMs": r.latency_ms }).to_string();
    conn.execute(
        "INSERT INTO usageHistory(timestamp, provider, model, connectionId, apiKey, endpoint,
            promptTokens, completionTokens, status, meta, requestId)
         VALUES(COALESCE(?1, strftime('%Y-%m-%dT%H:%M:%fZ','now')), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            if r.timestamp.is_empty() { None } else { Some(r.timestamp.as_str()) }, r.provider, r.model, r.connection_id, r.api_key, r.endpoint,
            r.prompt_tokens, r.completion_tokens, r.status.to_string(), meta, r.request_id
        ],
    )?;
    Ok(())
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

// ---- combos repo ----

#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub models: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub fn combo_insert(conn: &Connection, c: &Combo) -> Result<()> {
    let models = serde_json::to_string(&c.models)?;
    conn.execute(
        "INSERT INTO combos(id, name, kind, models, createdAt, updatedAt) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![c.id, c.name, c.kind, models, c.created_at, c.updated_at],
    )?;
    Ok(())
}

pub fn combo_get_by_id(conn: &Connection, id: &str) -> Result<Option<Combo>> {
    conn.query_row(
        "SELECT id, name, kind, models, createdAt, updatedAt FROM combos WHERE id = ?1",
        params![id],
        |r| {
            let models: String = r.get(3)?;
            Ok(Combo {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                models: serde_json::from_str(&models).unwrap_or_default(),
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn combo_delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM combos WHERE id = ?1", params![id])? > 0)
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
        assert_eq!(schema_version(&c).unwrap(), SCHEMA_VERSION);
        migrate(&c).unwrap();
        assert_eq!(schema_version(&c).unwrap(), SCHEMA_VERSION);
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
            request_id: "r".into(),
            ..Default::default()
        };
        usage_insert(&c, &row("gpt", 200)).unwrap();
        usage_insert(&c, &row("gpt", 502)).unwrap();
        let s = usage_summary(&c, "2026-01-01T00:00:00.000Z").unwrap();
        assert_eq!(s["totalRequests"], 2);
        assert_eq!(s["totalPromptTokens"], 20);
        assert_eq!(s["totalCompletionTokens"], 8);
        assert_eq!(s["totalErrors"], 1);
        assert_eq!(usage_summary(&c, "2027-01-01T00:00:00.000Z").unwrap()["totalRequests"], 0);
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
    fn combo_roundtrip() {
        let c = mem();
        let combo = Combo {
            id: "id1".into(),
            name: "fast".into(),
            kind: None,
            models: vec!["a/x".into(), "b/y".into()],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        combo_insert(&c, &combo).unwrap();
        assert_eq!(combo_get_by_id(&c, "id1").unwrap(), Some(combo));
        assert!(combo_delete(&c, "id1").unwrap());
        assert_eq!(combo_get_by_id(&c, "id1").unwrap(), None);
    }

    #[test]
    fn combo_name_case_insensitive_unique() {
        let c = mem();
        let mk = |id: &str, name: &str| Combo {
            id: id.into(),
            name: name.into(),
            kind: None,
            models: vec![],
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        combo_insert(&c, &mk("1", "Fast")).unwrap();
        assert!(combo_insert(&c, &mk("2", "fast")).is_err());
    }
}
