use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

const MODELS_URL: &str = "https://api.cline.bot/api/v1/models";
const FREE_URL: &str = "https://api.cline.bot/api/v1/ai/cline/recommended-models";

fn normalize(list: &[Value], pass_only: bool) -> Vec<Value> {
    list.iter().filter_map(|item| {
        let id = item["id"].as_str()?;
        if if pass_only { !id.starts_with("cline-pass/") } else { id.trim().is_empty() } { return None; }
        Some(json!({"id":id,"name":item["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id)}))
    }).collect()
}

pub fn cline_models_from_payload(
    raw: &Value,
    free: Option<&Value>,
    pass_only: bool,
) -> Option<Vec<Value>> {
    let list = raw.as_array().or_else(|| raw["data"].as_array())?;
    let mut result = normalize(list, pass_only);
    if !pass_only {
        let mut seen: HashSet<String> = result
            .iter()
            .filter_map(|v| v["id"].as_str().map(str::to_owned))
            .collect();
        if let Some(list) = free.and_then(|v| v["free"].as_array()) {
            for model in normalize(list, false) {
                if seen.insert(model["id"].as_str().unwrap().to_owned()) {
                    result.push(model);
                }
            }
        }
    }
    (!result.is_empty()).then_some(result)
}

pub async fn resolve_cline_models(
    client: &reqwest::Client,
    token: &str,
    api_key: bool,
    pass_only: bool,
) -> Option<Vec<Value>> {
    if token.is_empty() {
        return None;
    }
    let token = token.trim();
    let jwt = token.starts_with("eyJ") && token.split('.').count() >= 2;
    let token = if !api_key && jwt {
        format!("workos:{token}")
    } else {
        token.to_owned()
    };
    let mut request = client
        .get(MODELS_URL)
        .timeout(Duration::from_secs(5))
        .header("Accept", "application/json")
        .bearer_auth(token);
    if !api_key {
        request = request
            .header("HTTP-Referer", "https://cline.bot")
            .header("X-Title", "Cline")
            .header("X-CLIENT-TYPE", "9router")
            .header("X-IS-MULTIROOT", "false");
    }
    let raw = request
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    let raw: Value = serde_json::from_str(&raw).ok()?;
    let free = if pass_only {
        None
    } else {
        match client
            .get(FREE_URL)
            .timeout(Duration::from_secs(5))
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(response) => match response.error_for_status() {
                Ok(response) => response
                    .text()
                    .await
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok()),
                Err(_) => None,
            },
            Err(_) => None,
        }
    };
    cline_models_from_payload(&raw, free.as_ref(), pass_only)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merges_free_models_first_writer_wins() {
        let raw = json!({"data":[{"id":"a","name":"Original"},{"id":"cline-pass/b"}]});
        let free = json!({"free":[{"id":"a","name":"Duplicate"},{"id":"free"}]});
        let all = cline_models_from_payload(&raw, Some(&free), false).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0]["name"], "Original");
        assert_eq!(
            cline_models_from_payload(&raw, None, true).unwrap().len(),
            1
        );
    }
}
