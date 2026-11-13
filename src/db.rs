// Port of src/lib/db: schema, versioned migrations, repos (subset for step 1).
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 1;

const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";

// Subset of TABLES in schema.js. Remaining tables are added in later steps.
const INITIAL_SQL: &str = "
CREATE TABLE IF NOT EXISTS _meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (scope, key)
);
CREATE INDEX IF NOT EXISTS idx_kv_scope ON kv(scope);
CREATE TABLE IF NOT EXISTS combos (
    id TEXT PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    kind TEXT,
    models TEXT NOT NULL,
    createdAt TEXT NOT NULL,
    updatedAt TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_combos_name_lower_unique ON combos(LOWER(name));
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

/// Open DB and run migrations.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).context("open sqlite")?;
    conn.execute_batch(PRAGMAS)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(INITIAL_SQL)?;
    if schema_version(conn)? < SCHEMA_VERSION {
        conn.execute(
            "INSERT INTO _meta(key, value) VALUES('schemaVersion', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM _meta WHERE key = 'schemaVersion'", [], |r| r.get(0))
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
            id: id.into(), name: name.into(), kind: None, models: vec![],
            created_at: "t".into(), updated_at: "t".into(),
        };
        combo_insert(&c, &mk("1", "Fast")).unwrap();
        assert!(combo_insert(&c, &mk("2", "fast")).is_err());
    }
}
