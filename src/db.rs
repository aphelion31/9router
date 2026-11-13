// Port of src/lib/db: schema, versioned migrations, repos (subset for step 1).
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
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

/// Open DB and run migrations.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).context("open sqlite")?;
    conn.execute_batch(PRAGMAS)?;
    migrate(&conn)?;
    Ok(conn)
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
