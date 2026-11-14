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
    // Upstream LOCAL_PROVIDER_ALIASES, applied on top of the registry.
    if id == "xmtp" { return "xiaomi-tokenplan".to_owned(); }
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
    // Upstream getModelInfo: reserved provider ids/aliases never consult user nodes.
    let id = if is_reserved_prefix(prefix) { provider(prefix) } else { node_for_prefix(db, prefix).unwrap_or_else(|| provider(prefix)) };
    Some(format!("{id}/{}", model.to_lowercase()))
}

/// Built-in provider ids/aliases, plus the local alias overrides. Mirrors RESERVED_PROVIDER_PREFIXES.
fn is_reserved_prefix(prefix: &str) -> bool {
    matches!(prefix, "xmtp" | "xiaomi-tokenplan") || crate::providers::lookup(prefix).is_some()
}

/// First provider node whose prefix matches, in upstream rank order:
/// openai-compatible, then anthropic-compatible, then custom-embedding.
fn node_for_prefix(db: &Connection, prefix: &str) -> Option<String> {
    let mut stmt = db.prepare("SELECT id,type,data FROM providerNodes ORDER BY createdAt").ok()?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))).ok()?;
    rows.flatten().filter_map(|(id, kind, data)| {
        let data: Value = serde_json::from_str(data.as_deref().unwrap_or("{}")).ok()?;
        if data["prefix"].as_str() != Some(prefix) { return None; }
        let rank = match kind.as_deref() { Some("openai-compatible") => 0, Some("anthropic-compatible") => 1, Some("custom-embedding") => 2, _ => return None };
        Some((rank, id.to_lowercase()))
    }).min_by_key(|(rank, _)| *rank).map(|(_, id)| id)
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

/// Target a request will route to, as the handler itself derives it. `None` means
/// the target cannot be read (e.g. multipart videos), which a restricted key must
/// be denied on, matching upstream `enforceKeyAccessResolved`.
pub fn request_target(path: &str, value: &Value, multipart_model: Option<String>) -> Option<String> {
    if let Some(name) = path.strip_prefix("/v1beta/models/") {
        return name.strip_suffix(":generateContent").filter(|m| !m.is_empty()).map(str::to_owned);
    }
    if path == "/v1/search" || path == "/v1/web/fetch" {
        return value["provider"].as_str().or_else(|| value["model"].as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
    }
    if path == "/v1/audio/transcriptions" {
        return multipart_model.filter(|s| !s.is_empty());
    }
    if path.starts_with("/v1/videos/") {
        // Upstream reads only the body model; a missing one is denied for restricted keys.
        return value["model"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    }
    value["model"].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Text field of a multipart body, read with the same parser the handler uses.
pub fn multipart_text_field(headers: &axum::http::HeaderMap, body: &[u8], name: &str) -> Option<String> {
    let boundary = crate::handlers::media::multipart::extract_boundary(headers)?;
    crate::handlers::media::multipart::parse_multipart(body, &boundary).ok()?.get_text(name)
}
