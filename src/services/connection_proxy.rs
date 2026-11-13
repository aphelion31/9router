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
