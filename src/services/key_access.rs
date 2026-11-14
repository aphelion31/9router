use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct KeyAccess {
    combos: HashSet<String>,
    models: HashSet<String>,
    providers: HashSet<String>,
}

fn provider(id: &str) -> String {
    crate::providers::lookup(id).map(|p| p.id.as_str()).unwrap_or(id).to_lowercase()
}

pub fn resolve_model(db: &Connection, requested: &str) -> Option<String> {
    if requested.is_empty() { return None; }
    let resolved = if requested.contains('/') { requested.to_owned() } else {
        let raw = crate::db::kv_get(db, "modelAliases", requested).ok().flatten();
        let value = raw.and_then(|s| serde_json::from_str::<Value>(&s).ok());
        value.as_ref().and_then(|v| v.as_str().map(str::to_owned).or_else(|| Some(format!("{}/{}", v["provider"].as_str()?, v["model"].as_str()?))))
            .unwrap_or_else(|| {
                let lower = requested.to_lowercase();
                let p = if lower.starts_with("claude-") { "anthropic" } else if lower.starts_with("gemini-") { "gemini" } else if lower.starts_with("deepseek-") { "openrouter" } else if lower.starts_with("gpt-5.") || lower.starts_with("gpt-6") || lower == "codex-auto-review" { "codex" } else { "openai" };
                format!("{p}/{requested}")
            })
    };
    let (prefix, model) = resolved.split_once('/')?;
    if prefix.is_empty() || model.is_empty() { return None; }
    let id = if crate::providers::lookup(prefix).is_some() { provider(prefix) } else {
        let mut stmt = db.prepare("SELECT id,data FROM providerNodes").ok()?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?;
        rows.flatten().find_map(|(id, data)| serde_json::from_str::<Value>(&data).ok().filter(|v| v["prefix"].as_str() == Some(prefix)).map(|_| id.to_lowercase())).unwrap_or_else(|| provider(prefix))
    };
    Some(format!("{id}/{}", model.to_lowercase()))
}

fn combo_exists(db: &Connection, name: &str) -> bool {
    !name.contains('/') && db.query_row("SELECT 1 FROM combos WHERE name=?1", [name], |_| Ok(())).optional().ok().flatten().is_some()
}

impl KeyAccess {
    pub fn load(db: &Connection, raw: Option<String>) -> Self {
        let allow: Vec<String> = raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut result = Self { combos: HashSet::new(), models: HashSet::new(), providers: HashSet::new() };
        for entry in allow {
            let is_combo = db.query_row("SELECT 1 FROM combos WHERE lower(name)=lower(?1)", [&entry], |_| Ok(())).optional().ok().flatten().is_some();
            if is_combo { result.combos.insert(entry.to_lowercase()); continue; }
            if let Some(model) = resolve_model(db, &entry) { result.models.insert(model); }
            if !entry.contains('/') { result.providers.insert(provider(&entry)); }
        }
        result
    }
    pub fn allows(&self, db: &Connection, requested: &str) -> bool {
        if requested.is_empty() { return false; }
        if combo_exists(db, requested) { return self.combos.contains(&requested.to_lowercase()); }
        resolve_model(db, requested).is_some_and(|m| self.models.contains(&m))
    }
    pub fn allows_provider(&self, db: &Connection, requested: &str) -> bool {
        if requested.is_empty() { return false; }
        if combo_exists(db, requested) { return self.combos.contains(&requested.to_lowercase()); }
        self.providers.contains(&provider(requested))
    }
    pub fn allows_entry(&self, db: &Connection, entry: &Value) -> bool {
        let Some(id) = entry["id"].as_str() else { return false; };
        if entry["owned_by"] == "combo" { return self.combos.contains(&id.to_lowercase()); }
        if entry["kind"] == "webSearch" || entry["kind"] == "webFetch" {
            return resolve_model(db, id).and_then(|m| m.split_once('/').map(|(p, _)| self.providers.contains(p))).unwrap_or(false);
        }
        resolve_model(db, id).is_some_and(|m| self.models.contains(&m))
    }
}
