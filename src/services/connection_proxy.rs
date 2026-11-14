use serde_json::{Value, json};

pub fn resolve_rotation_pool_ids(pools: &[Value], selected: &[String]) -> Vec<String> {
    pools
        .iter()
        .filter_map(|p| {
            let id = p["id"].as_str().filter(|s| !s.is_empty())?;
            p["proxyUrl"].as_str().filter(|s| !s.is_empty())?;
            if selected.is_empty() || selected.iter().any(|s| s == id) {
                Some(id.to_owned())
            } else {
                None
            }
        })
        .collect()
}

pub fn resolve_connection_proxy_config(data: &Value, pool: Result<Option<&Value>, ()>) -> Value {
    let pool_id = data["proxyPoolId"].as_str().unwrap_or("").trim();
    let pool_id = if pool_id == "__none__" { "" } else { pool_id };
    let pool_id_value = if pool_id.is_empty() {
        Value::Null
    } else {
        json!(pool_id)
    };
    let pool = if pool_id.is_empty() {
        None
    } else {
        match pool {
            Ok(pool) => pool,
            Err(()) => {
                return json!({"source":"error","proxyPoolId":pool_id_value,"proxyPool":null,"connectionProxyEnabled":false,"connectionProxyUrl":"","connectionNoProxy":"","strictProxy":true});
            }
        }
    };
    let strict = pool.is_some_and(|p| p["strictProxy"] == true);
    if let Some(pool) = pool.filter(|p| {
        p["isActive"] == true && p["proxyUrl"].as_str().is_some_and(|s| !s.trim().is_empty())
    }) {
        let url = pool["proxyUrl"].as_str().unwrap().trim();
        let no_proxy = pool["noProxy"].as_str().unwrap_or("").trim();
        let kind = pool["type"].as_str().unwrap_or("");
        if ["vercel", "cloudflare", "deno", "netlify"].contains(&kind) {
            return json!({"source":kind,"proxyPoolId":pool_id_value,"proxyPool":pool,"connectionProxyEnabled":false,"connectionProxyUrl":"","connectionNoProxy":no_proxy,"strictProxy":strict,"vercelRelayUrl":url});
        }
        return json!({"source":"pool","proxyPoolId":pool_id_value,"proxyPool":pool,"connectionProxyEnabled":true,"connectionProxyUrl":url,"connectionNoProxy":no_proxy,"strictProxy":strict});
    }
    let enabled = data["connectionProxyEnabled"] == true;
    let url = data["connectionProxyUrl"].as_str().unwrap_or("").trim();
    let no_proxy = data["connectionNoProxy"].as_str().unwrap_or("").trim();
    json!({"source":if enabled && !url.is_empty() { "legacy" } else { "none" },"proxyPoolId":pool_id_value,"proxyPool":null,"connectionProxyEnabled":enabled,"connectionProxyUrl":url,"connectionNoProxy":no_proxy,"strictProxy":strict})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_pool_failure_never_silently_goes_direct() {
        let data = json!({"proxyPoolId":"p"});
        assert_eq!(
            resolve_connection_proxy_config(&data, Err(()))["strictProxy"],
            true
        );
        assert_eq!(
            resolve_connection_proxy_config(
                &data,
                Ok(Some(&json!({"strictProxy":true,"isActive":false})))
            )["strictProxy"],
            true
        );
        let pool = json!({"strictProxy":true,"isActive":true,"proxyUrl":" http://proxy "});
        let config = resolve_connection_proxy_config(&data, Ok(Some(&pool)));
        assert_eq!(config["source"], "pool");
        assert_eq!(config["connectionProxyUrl"], "http://proxy");
    }
}

/// Round-robin rotation state per provider. Process-local, resets on restart (matches JS).
#[derive(Default)]
pub struct ProxyRotation {
    index: std::collections::HashMap<String, usize>,
}

impl ProxyRotation {
    /// Pick the next pool id. One pool or unknown strategy returns the first entry.
    pub fn pick(&mut self, pool_ids: &[String], strategy: &str, provider: &str) -> Option<String> {
        if pool_ids.is_empty() {
            return None;
        }
        if pool_ids.len() == 1 {
            return Some(pool_ids[0].clone());
        }
        if strategy == "random" {
            use ring::rand::SecureRandom;
            let mut bytes = [0u8; 8];
            if ring::rand::SystemRandom::new().fill(&mut bytes).is_ok() {
                return Some(pool_ids[(u64::from_ne_bytes(bytes) % pool_ids.len() as u64) as usize].clone());
            }
        }
        if strategy != "round-robin" { return Some(pool_ids[0].clone()); }
        let slot = self.index.entry(provider.to_owned()).or_insert(usize::MAX);
        *slot = slot.wrapping_add(1) % pool_ids.len();
        Some(pool_ids[*slot].clone())
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    #[test]
    fn round_robin_cycles_per_provider() {
        let mut rot = ProxyRotation::default();
        let ids: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let picks: Vec<_> = (0..4).map(|_| rot.pick(&ids, "round-robin", "p").unwrap()).collect();
        assert_eq!(picks, ["a", "b", "c", "a"]);
        assert_eq!(rot.pick(&ids, "round-robin", "q").unwrap(), "a");
        assert_eq!(rot.pick(&ids, "none", "p").unwrap(), "a");
        assert_eq!(rot.pick(&[], "round-robin", "p"), None);
    }
}
