//! Scored auto-routing, port of `src/sse/services/autoRouting.js`.
//!
//! A client sends `auto:<intent>` as the model. Candidates come from providers with at least one
//! active connection, scored by health tier, capabilities, context size, latency and free-ness.
//! Ties break alphabetically. Per-provider queues are interleaved so failover spans providers.
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

pub const AUTO_INTENTS: [&str; 4] = ["coding", "fast", "cheap", "headroom"];
pub const AUTO_CANDIDATE_LIMIT: usize = 8;
pub const AUTO_EXPLORATION_SHARE: f64 = 0.05;
/// At most this many candidates per provider, so failover spans providers.
pub const AUTO_PER_PROVIDER_LIMIT: usize = 3;
/// Upstream SUSPECT_PROBATION_MS.
const SUSPECT_PROBATION_MS: i64 = 60 * 60 * 1000;
/// Upstream DEFAULT_CAPABILITIES.contextWindow.
const DEFAULT_CONTEXT: u64 = 200_000;
const LATENCY_WINDOW_MS: i64 = 7 * 24 * 3600 * 1000;
const LATENCY_ROWS: i64 = 5000;

/// `auto:coding` (case-insensitive) → `"coding"`. Anything else → `None`.
pub fn parse_auto_target(value: &str) -> Option<&'static str> {
    let value = value.trim();
    let intent = value.strip_prefix_ci("auto:")?.to_ascii_lowercase();
    AUTO_INTENTS.into_iter().find(|i| *i == intent)
}

trait StripPrefixCi {
    fn strip_prefix_ci(&self, prefix: &str) -> Option<&str>;
}

impl StripPrefixCi for str {
    fn strip_prefix_ci(&self, prefix: &str) -> Option<&str> {
        let head = self.get(..prefix.len())?;
        head.eq_ignore_ascii_case(prefix).then(|| &self[prefix.len()..])
    }
}

/// 0..40 from context size: 4k → 6, 32k → 15, 200k → 23, 1M → 30.
fn ctx_score(ctx: u64) -> i64 {
    let ctx = ctx.max(1000) as f64;
    ((10.0 * (ctx / 1000.0).log10()).round() as i64).clamp(0, 40)
}

fn latency_bonus(p50: Option<f64>) -> i64 {
    match p50 {
        Some(p) if p.is_finite() && p >= 0.0 => (10.0 - p / 500.0).round().clamp(0.0, 10.0) as i64,
        _ => 0,
    }
}

fn fast_latency_score(p50: Option<f64>) -> Option<i64> {
    match p50 {
        Some(p) if p.is_finite() && p >= 0.0 => Some((40.0 - p / 100.0).round().clamp(0.0, 40.0) as i64),
        _ => None,
    }
}

/// Capabilities used for scoring, from registry metadata. Upstream defaults: tools on, reasoning off.
struct Caps {
    tools: bool,
    reasoning: bool,
    context: u64,
}

fn caps_of(metadata: &HashMap<String, Value>) -> Caps {
    let caps = metadata.get("capabilities");
    let has = |key: &str| match caps {
        Some(Value::Object(map)) => map.get(key).and_then(Value::as_bool).unwrap_or(false),
        Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some(key)),
        _ => false,
    };
    let tools = match caps {
        Some(Value::Object(map)) => map.get("tools").and_then(Value::as_bool).unwrap_or(true),
        Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some("tools")),
        _ => true,
    };
    Caps {
        tools,
        reasoning: has("reasoning") || metadata.get("supportsReasoning").and_then(Value::as_bool) == Some(true),
        context: metadata.get("contextLength").and_then(Value::as_u64).unwrap_or(DEFAULT_CONTEXT),
    }
}

/// Upstream scoreCandidate.
fn score(intent: &str, caps: &Caps, health: i64, free: bool, latency: Option<f64>) -> i64 {
    let ctx = ctx_score(caps.context);
    let bonus = latency_bonus(latency);
    match intent {
        "coding" => health + if caps.tools { 30 } else { 0 } + if caps.reasoning { 20 } else { 0 } + ctx + bonus,
        "fast" => match fast_latency_score(latency) {
            Some(measured) => health + measured,
            None => health + (40 - ctx),
        },
        // Upstream Math.round(20 - ctx/2): round half up, so 20 - floor(ctx/2) for these values.
        "cheap" => health + if free { 60 } else { 0 } + (20 - ctx / 2) + bonus,
        "headroom" => health + ctx * 2 + bonus,
        _ => health,
    }
}

/// Deterministic exploration: with probability `share`, promote one lower-ranked candidate to
/// the front. Seeded per intent+day, so one day's choice is stable. FNV-1a over the seed.
pub fn apply_exploration(ranked: Vec<String>, share: f64, seed: &str) -> Vec<String> {
    // `!(x > 0.0)` is deliberate: it also rejects NaN, which `x <= 0.0` would not.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if ranked.len() < 2 || !(share > 0.0) {
        return ranked;
    }
    let mut h: u32 = 0x811c_9dc5;
    for b in seed.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    if (h % 1000) as f64 / 1000.0 >= share {
        return ranked;
    }
    let idx = 1 + (h as usize) % (ranked.len() - 1);
    let mut out = Vec::with_capacity(ranked.len());
    out.push(ranked[idx].clone());
    out.extend(ranked[..idx].iter().cloned());
    out.extend(ranked[idx + 1..].iter().cloned());
    out
}

/// One active connection: its provider id and decrypted data.
pub struct Conn {
    pub provider: String,
    pub data: Value,
}

/// Everything the ranking reads, so the pure function is testable without a DB.
pub struct Inputs {
    pub connections: Vec<Conn>,
    /// Disabled model ids keyed by provider id or alias.
    pub disabled: HashMap<String, Vec<String>>,
    pub latency_by_provider: HashMap<String, f64>,
    pub free_providers: HashSet<String>,
}

/// Upstream connectionHealthTier with probation: 0 clean, 1 suspect.
fn health_tier(data: &Value, now_ms: i64) -> i64 {
    let backoff = data["backoffLevel"].as_i64().unwrap_or(0) > 0;
    let last_error = data["lastError"].as_str().is_some_and(|s| !s.is_empty()) || data["lastError"].as_bool() == Some(true);
    if !(backoff || last_error) {
        return 0;
    }
    if let Some(at) = data["lastErrorAt"].as_str().and_then(parse_iso_ms)
        && now_ms - at >= SUSPECT_PROBATION_MS
    {
        return 0;
    }
    1
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.sss]Z` to epoch ms (UTC). `None` on anything else.
fn parse_iso_ms(s: &str) -> Option<i64> {
    let (date, time) = s.trim_end_matches('Z').split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000)
}

/// Upstream buildAutoCandidates. Returns ranked `provider/model` strings, capped.
pub fn rank_candidates(intent: &str, inputs: &Inputs, now_ms: i64, exploration_share: f64, seed: Option<&str>) -> Vec<String> {
    if !AUTO_INTENTS.contains(&intent) {
        return vec![];
    }
    // Per provider: best (lowest) health tier across its connections, union of enabledModels.
    let mut by_provider: BTreeMap<String, (i64, Option<Vec<String>>)> = BTreeMap::new();
    for conn in &inputs.connections {
        let tier = health_tier(&conn.data, now_ms);
        let entry = by_provider.entry(conn.provider.clone()).or_insert((1, None));
        entry.0 = entry.0.min(tier);
        if let Some(list) = conn.data["providerSpecificData"]["enabledModels"].as_array() {
            let merged = entry.1.get_or_insert_with(Vec::new);
            merged.extend(list.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }

    let mut scored: Vec<(String, String, i64)> = Vec::new();
    for prov in crate::providers::all() {
        let Some((tier, enabled)) = by_provider.get(&prov.id) else { continue };
        let health = if *tier == 0 { 100 } else { 0 };
        let mut disabled_ids: HashSet<String> = HashSet::new();
        for key in [prov.id.as_str(), prov.alias.as_str()] {
            disabled_ids.extend(inputs.disabled.get(key).cloned().unwrap_or_default());
        }
        for model in &prov.models {
            if model.id.is_empty() || disabled_ids.contains(&model.id) {
                continue;
            }
            if !enabled_matches(&model.id, enabled.as_deref(), &prov.id) {
                continue;
            }
            let candidate = format!("{}/{}", prov.id, model.id);
            if disabled_ids.contains(&candidate) {
                continue;
            }
            let caps = caps_of(&model.metadata);
            let free = inputs.free_providers.contains(&prov.id) || model.id.ends_with(":free");
            let latency = inputs.latency_by_provider.get(&prov.id).copied();
            scored.push((prov.id.clone(), candidate.clone(), score(intent, &caps, health, free, latency)));
        }
    }

    scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));

    // Per-provider queues, best first, capped. Round 0 = each provider's best, round 1 = second, ...
    let mut queues: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    for (provider, candidate, s) in &scored {
        let q = queues.entry(provider.clone()).or_default();
        if q.len() < AUTO_PER_PROVIDER_LIMIT {
            q.push((candidate.clone(), *s));
        }
    }
    let mut ordered: Vec<Vec<(String, i64)>> = queues.into_values().collect();
    ordered.sort_by(|a, b| b[0].1.cmp(&a[0].1).then_with(|| a[0].0.cmp(&b[0].0)));

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut round = 0;
    let mut progressed = true;
    while out.len() < AUTO_CANDIDATE_LIMIT && progressed {
        progressed = false;
        for q in &ordered {
            if out.len() >= AUTO_CANDIDATE_LIMIT {
                break;
            }
            if let Some((candidate, _)) = q.get(round).filter(|(c, _)| seen.insert(c.clone())) {
                out.push(candidate.clone());
                progressed = true;
            }
        }
        round += 1;
    }

    let seed = seed.map(str::to_owned).unwrap_or_else(|| format!("{intent}|{}", utc_date(now_ms)));
    apply_exploration(out, exploration_share, &seed)
}

/// UTC `YYYY-MM-DD` of an epoch ms (civil-from-days, Hinnant).
fn utc_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Upstream enabledMatches: no list means every model is enabled. Entries match the bare id,
/// `provider/id`, or any `x/id` suffix.
fn enabled_matches(model_id: &str, entries: Option<&[String]>, provider: &str) -> bool {
    let Some(entries) = entries else { return true };
    if entries.is_empty() {
        return true;
    }
    entries.iter().any(|e| {
        e == model_id || e == &format!("{provider}/{model_id}") || e.split_once('/').is_some_and(|(_, rest)| rest == model_id)
    })
}

/// Free providers: registry `auth_type == "free"`. Upstream FREE_PROVIDERS is a `category` field,
/// which the Rust registry does not carry, so this is the closest data-driven set.
fn free_provider_ids() -> &'static HashSet<String> {
    static FREE: OnceLock<HashSet<String>> = OnceLock::new();
    FREE.get_or_init(|| {
        let registry: Value = serde_json::from_str(include_str!("../providers/registry.json")).unwrap_or(Value::Null);
        registry
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["auth_type"].as_str() == Some("free"))
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect()
    })
}

/// Loads inputs from the DB and ranks. Empty when nothing is routable.
pub fn build_auto_candidates(db: &Connection, intent: &str) -> Vec<String> {
    let Ok(mut stmt) = db.prepare("SELECT provider,data FROM providerConnections WHERE isActive=1") else { return vec![] };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) else { return vec![] };
    let connections: Vec<Conn> = rows
        .flatten()
        .filter_map(|(provider, raw)| crate::crypto::parse_data(&raw).map(|data| Conn { provider, data }))
        .collect();

    let mut disabled: HashMap<String, Vec<String>> = HashMap::new();
    for p in crate::providers::all() {
        for key in [p.id.as_str(), p.alias.as_str()] {
            if let Some(list) = crate::db::kv_get(db, "disabledModels", key).ok().flatten().and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok()) {
                disabled.insert(key.to_owned(), list);
            }
        }
    }

    let now_ms = now_millis();
    let since = iso_ms(now_ms - LATENCY_WINDOW_MS);
    let latency_by_provider: HashMap<String, f64> = crate::services::latency_stats::get_latency_stats(db, Some(&since), LATENCY_ROWS)
        .map(|s| s.by_provider.into_iter().filter_map(|(p, v)| v.total_p50.map(|x| (p, x))).collect())
        .unwrap_or_default();

    let inputs = Inputs { connections, disabled, latency_by_provider, free_providers: free_provider_ids().clone() };
    rank_candidates(intent, &inputs, now_ms, AUTO_EXPLORATION_SHARE, None)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// ISO-8601 UTC with millisecond precision, the format the DB stores timestamps in.
fn iso_ms(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    let rem = ms.rem_euclid(86_400_000);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000)
}
