use rusqlite::Connection;
use serde_json::Value;

pub fn parse_auto_target(value: &str) -> Option<&'static str> {
    let value = value.trim();
    ["coding", "fast", "cheap", "headroom"].into_iter().find(|intent| value.eq_ignore_ascii_case(&format!("auto:{intent}")))
}

pub fn build_auto_candidates(db: &Connection, intent: &str) -> Vec<String> {
    let Ok(mut stmt) = db.prepare("SELECT provider,data FROM providerConnections WHERE isActive=1") else { return vec![]; };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) else { return vec![]; };
    let connections: Vec<_> = rows.flatten().collect();
    let mut queues = Vec::new();
    for p in crate::providers::all() {
        let conns: Vec<Value> = connections.iter().filter(|(id, _)| id == &p.id).filter_map(|(_, raw)| crate::crypto::parse_data(raw)).collect();
        if conns.is_empty() { continue; }
        let disabled: Vec<String> = [p.id.as_str(), p.alias.as_str()].into_iter().flat_map(|key| crate::db::kv_get(db, "disabledModels", key).ok().flatten().and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok()).unwrap_or_default()).collect();
        let enabled: Vec<String> = conns.iter().flat_map(|c| c["providerSpecificData"]["enabledModels"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned)).collect();
        let mut models: Vec<(i64, String)> = p.models.iter().filter(|m| !disabled.contains(&m.id) && !disabled.contains(&format!("{}/{}",p.id,m.id))).filter(|m| enabled.is_empty() || enabled.iter().any(|e| e == &m.id || e == &format!("{}/{}",p.id,m.id) || e.split_once('/').is_some_and(|(_, rest)| rest == m.id))).map(|m| {
            let ctx = m.metadata.get("contextLength").and_then(Value::as_u64).unwrap_or(1000).max(1000) as f64;
            let ctx = (10.0 * (ctx / 1000.0).log10()).round().clamp(0.0, 40.0) as i64;
            let caps = m.metadata.get("capabilities");
            let has = |key: &str| caps.is_some_and(|c| c[key] == true || c.as_array().is_some_and(|a| a.iter().any(|v| v == key)));
            let score = match intent { "coding" => 30 * i64::from(has("tools")) + 20 * i64::from(has("reasoning")) + ctx, "fast" => 40 - ctx, "cheap" => 60 * i64::from(m.id.ends_with(":free")) + 20 - ctx / 2, "headroom" => ctx * 2, _ => 0 };
            (score, format!("{}/{}", p.id, m.id))
        }).collect();
        models.sort_by(|a,b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        models.truncate(3);
        if !models.is_empty() { queues.push(models); }
    }
    queues.sort_by(|a,b| b[0].0.cmp(&a[0].0).then(a[0].1.cmp(&b[0].1)));
    let mut out = Vec::new();
    for round in 0..3 { for q in &queues { if let Some((_, model)) = q.get(round) { if out.len() < 8 { out.push(model.clone()); } } } }
    out
}
