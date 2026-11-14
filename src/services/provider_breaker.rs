use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use serde_json::{Value, json};

const WINDOW: u64 = 300_000;
const OPEN: u64 = 60_000;
const PROBE: u64 = 30_000;
#[derive(Default)]
struct Model { failures: Vec<Failure>, until: Option<u64>, level: u32 }
#[derive(Clone)]
struct Failure { at: u64, connection: String, model: Option<String> }
#[derive(Default)]
struct Entry { state: u8, failures: Vec<Failure>, opened: u64, probe: u64, models: HashMap<String, Model> }
#[derive(Default)]
pub struct Breakers { entries: HashMap<String, Entry> }
fn key(provider: &str, model: Option<&str>) -> Option<String> {
    model.filter(|m| !m.is_empty()).map(|m| m.strip_prefix(&format!("{provider}/")).unwrap_or(m).to_owned())
}
fn prune(f: &mut Vec<Failure>, now: u64) { f.retain(|x| x.at >= now.saturating_sub(WINDOW)); }
fn tripped(f: &[Failure]) -> bool { f.len() >= 5 && f.iter().map(|x| &x.connection).collect::<HashSet<_>>().len() >= 2 }
impl Breakers {
    pub fn gate(&mut self, provider: &str, model: Option<&str>, now: u64) -> bool {
        let Some(e) = self.entries.get_mut(provider) else { return true };
        if e.state == 1 {
            if now < e.opened + OPEN { return false; }
            e.state = 2; e.probe = now + PROBE; return true;
        }
        if e.state == 2 && now >= e.probe { e.state = 0; e.failures.clear(); e.opened = 0; e.probe = 0; return true; }
        if let Some(m) = key(provider, model).and_then(|k| e.models.get_mut(&k)) {
            if let Some(until) = m.until {
                if now < until { return false; }
                if e.state == 0 { m.until = None; m.failures.clear(); }
            }
        }
        true
    }
    pub fn failure(&mut self, provider: &str, connection: &str, model: Option<&str>, now: u64) {
        if provider.is_empty() || connection.is_empty() { return; }
        let e = self.entries.entry(provider.to_owned()).or_default();
        if e.state == 2 { e.state = 1; e.opened = now; e.probe = 0; e.failures.clear(); return; }
        if e.state == 1 { return; }
        let model = key(provider, model);
        if let Some(k) = &model {
            let m = e.models.entry(k.clone()).or_default();
            if m.until.is_some_and(|until| now < until) { return; }
            prune(&mut m.failures, now);
            m.failures.push(Failure { at: now, connection: connection.into(), model: None });
            if tripped(&m.failures) {
                m.until = Some(now + OPEN.saturating_mul(2u64.saturating_pow(m.level)).min(600_000));
                m.level = m.level.saturating_add(1); m.failures.clear();
                e.failures.retain(|f| f.model.as_ref() != Some(k)); return;
            }
            if m.level > 0 { return; }
        }
        prune(&mut e.failures, now);
        e.failures.push(Failure { at: now, connection: connection.into(), model });
        if tripped(&e.failures) { e.state = 1; e.opened = now; e.failures.clear(); }
    }
    pub fn success(&mut self, provider: &str, model: Option<&str>, now: u64) {
        let Some(e) = self.entries.get_mut(provider) else { return };
        if let Some(m) = key(provider, model).and_then(|k| e.models.get_mut(&k)) { m.until = None; m.failures.clear(); m.level = 0; }
        if e.state == 2 { e.state = 0; e.failures.clear(); e.opened = 0; e.probe = 0; }
        if e.state == 0 { prune(&mut e.failures, now); }
    }
    pub fn snapshot(&mut self, now: u64) -> Value {
        let mut out = serde_json::Map::new();
        for (provider, e) in &mut self.entries {
            let models: serde_json::Map<String, Value> = e.models.iter().filter_map(|(k,m)| m.until.filter(|t| now < *t).map(|t| (k.clone(), json!({"state":"cooldown","cooldownUntil": crate::services::usage_shared::unix_millis_iso(t as i64),"backoffLevel":m.level})))).collect();
            let mut v = if e.state == 0 { prune(&mut e.failures, now); if e.failures.is_empty() && models.is_empty() { continue; } json!({"state":"closed","failuresInWindow":e.failures.len()}) } else { json!({"state": if e.state == 1 {"open"} else {"half-open"}, "openedAt":crate::services::usage_shared::unix_millis_iso(e.opened as i64)}) };
            if e.state == 2 { v["probeUntil"] = json!(crate::services::usage_shared::unix_millis_iso(e.probe as i64)); }
            if !models.is_empty() { v["models"] = Value::Object(models); }
            out.insert(provider.clone(), v);
        }
        Value::Object(out)
    }
}
pub fn global() -> &'static Mutex<Breakers> { static STATE: OnceLock<Mutex<Breakers>> = OnceLock::new(); STATE.get_or_init(Default::default) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn transitions() { let mut b=Breakers::default(); for i in 0..5 { b.failure("p", if i%2==0 {"a"} else {"b"}, None, 1000+i); } assert!(!b.gate("p",None,1005)); assert!(b.gate("p",None,61004)); b.failure("p","a",None,61005); assert!(!b.gate("p",None,61006)); assert!(b.gate("p",None,121005)); b.success("p",None,121006); assert!(b.gate("p",None,121007)); }
    #[test] fn model_isolation() { let mut b=Breakers::default(); for i in 0..5 { b.failure("p",if i%2==0 {"a"} else {"b"},Some("p/bad"),1000+i); } assert!(!b.gate("p",Some("bad"),1005)); assert!(b.gate("p",Some("good"),1005)); b.success("p",Some("good"),1005); assert!(!b.gate("p",Some("bad"),1005)); assert!(b.gate("p",Some("bad"),61004)); }
}
