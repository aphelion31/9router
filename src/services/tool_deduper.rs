use serde_json::Value;
use std::collections::HashSet;

fn name(tool: &Value) -> &str {
    tool["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| tool["function"]["name"].as_str())
        .unwrap_or("")
}
fn deepseek(model: &str) -> bool {
    let model = model.trim_end();
    let model = if model.ends_with(')') {
        model.rfind('(').map(|i| model[..i].trim()).unwrap_or(model)
    } else {
        model.trim()
    };
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .get(..9)
        .is_some_and(|s| s.eq_ignore_ascii_case("deepseek-"))
}

pub fn dedupe_tools(tools: &[Value], client: &str, model: &str) -> (Vec<Value>, Vec<String>) {
    let names: Vec<_> = tools.iter().map(name).collect();
    let web = client == "claude"
        && names.iter().any(|n| {
            matches!(
                *n,
                "mcp__exa__web_search_exa"
                    | "mcp__exa__web_fetch_exa"
                    | "mcp__tavily__tavily_search"
                    | "mcp__tavily__tavily_extract"
            )
        });
    let browser = client == "claude" && names.iter().any(|n| n.starts_with("mcp__browsermcp__"));
    let mut strip = Vec::<String>::new();
    for n in &names {
        if (web && matches!(*n, "WebSearch" | "WebFetch" | "mcp__workspace__web_fetch"))
            || (browser && n.starts_with("mcp__Claude_in_Chrome__"))
        {
            if !strip.iter().any(|s| s == n) {
                strip.push((*n).to_owned());
            }
        }
    }
    let mut seen = HashSet::new();
    let mut dropped = Vec::new();
    let mut out = Vec::new();
    for tool in tools {
        let n = name(tool);
        let duplicate = deepseek(model) && !n.is_empty() && !seen.insert(n);
        if duplicate {
            dropped.push(n.to_owned());
        }
        if !duplicate && !strip.iter().any(|s| s == n) {
            out.push(tool.clone());
        }
    }
    dropped.extend(strip);
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn removes_redundant_tools_only_for_matching_clients_or_models() {
        let tools = vec![
            json!({"name":"WebSearch"}),
            json!({"name":"mcp__exa__web_search_exa"}),
            json!({"function":{"name":"x"}}),
            json!({"name":"x"}),
        ];
        let (out, stripped) = dedupe_tools(&tools, "claude", "vendor/deepseek-v3(high)");
        assert_eq!(out.len(), 2);
        assert_eq!(stripped, ["x", "WebSearch"]);
        assert_eq!(dedupe_tools(&tools, "codex", "gpt").0.len(), 4);
    }
}
