#![allow(dead_code)]

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThinkingOverride {
    None,
    Auto,
    Budget(u64),
    Level(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkingConfig {
    pub mode: String, // "none" | "auto" | "budget" | "level"
    pub budget: Option<u64>,
    pub level: Option<String>,
}

pub const EFFORT_DEFAULT: &str = "medium";
pub const EFFORT_MAX_BUDGET: u64 = 16384;

pub fn effort_to_budget(level: &str) -> Option<u64> {
    match level.to_lowercase().as_str() {
        "none" | "off" | "disabled" => Some(0),
        "minimal" => Some(512),
        "low" => Some(1024),
        "medium" => Some(8192),
        "high" | "xhigh" | "max" => Some(EFFORT_MAX_BUDGET),
        _ => None,
    }
}

pub fn budget_to_effort(budget: u64) -> &'static str {
    if budget == 0 {
        "none"
    } else if budget <= 1024 {
        "low"
    } else if budget <= 8192 {
        "medium"
    } else {
        "high"
    }
}

/// Parse trailing `(level)` or `(budget)` suffix from a model string.
/// E.g., `claude-sonnet-4.5(high)` -> `("claude-sonnet-4.5", Some(Level("high")))`,
/// `claude-sonnet-4.5(8192)` -> `("claude-sonnet-4.5", Some(Budget(8192)))`,
/// `claude-sonnet-4.5(none)` -> `("claude-sonnet-4.5", Some(None))`.
pub fn parse_suffix(model: &str) -> (String, Option<ThinkingOverride>) {
    let trimmed = model.trim();
    if let Some(open) = trimmed.rfind('(') {
        if trimmed.ends_with(')') && open < trimmed.len() - 1 {
            let inner = trimmed[open + 1..trimmed.len() - 1].trim();
            let clean = trimmed[..open].to_string();
            let lower = inner.to_lowercase();
            let ov = match lower.as_str() {
                "none" | "off" | "disabled" => ThinkingOverride::None,
                "auto" => ThinkingOverride::Auto,
                "minimal" | "low" | "medium" | "high" | "xhigh" | "max" => {
                    ThinkingOverride::Level(lower)
                }
                _ => {
                    if let Ok(b) = inner.parse::<u64>() {
                        if b == 0 {
                            ThinkingOverride::None
                        } else {
                            ThinkingOverride::Budget(b)
                        }
                    } else {
                        return (model.to_string(), None);
                    }
                }
            };
            return (clean, Some(ov));
        }
    }
    (model.to_string(), None)
}

/// Extract normalized thinking intent from any client request body shape.
pub fn extract_thinking(body: &Value) -> Option<ThinkingConfig> {
    if !body.is_object() {
        return None;
    }

    // 1. Claude output_config.effort
    if let Some(effort) = body
        .get("output_config")
        .and_then(|o| o.get("effort"))
        .and_then(|v| v.as_str())
    {
        let lower = effort.to_lowercase();
        if matches!(lower.as_str(), "none" | "off" | "disabled") {
            return Some(ThinkingConfig {
                mode: "none".to_string(),
                budget: None,
                level: None,
            });
        }
        return Some(ThinkingConfig {
            mode: "level".to_string(),
            budget: None,
            level: Some(lower),
        });
    }

    // 2. Claude thinking object
    if let Some(thinking) = body.get("thinking").and_then(|v| v.as_object()) {
        let t = thinking
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if t == "disabled" || t == "none" || t == "off" {
            return Some(ThinkingConfig {
                mode: "none".to_string(),
                budget: None,
                level: None,
            });
        }
        if t == "adaptive" {
            return Some(ThinkingConfig {
                mode: "auto".to_string(),
                budget: None,
                level: None,
            });
        }
        if let Some(b) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
            if b == 0 {
                return Some(ThinkingConfig {
                    mode: "none".to_string(),
                    budget: None,
                    level: None,
                });
            }
            return Some(ThinkingConfig {
                mode: "budget".to_string(),
                budget: Some(b),
                level: None,
            });
        }
        if t == "enabled" {
            return Some(ThinkingConfig {
                mode: "auto".to_string(),
                budget: None,
                level: None,
            });
        }
    }

    // 3. OpenAI reasoning_effort
    if let Some(re) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        let lower = re.to_lowercase();
        if matches!(lower.as_str(), "none" | "off" | "disabled") {
            return Some(ThinkingConfig {
                mode: "none".to_string(),
                budget: None,
                level: None,
            });
        }
        return Some(ThinkingConfig {
            mode: "level".to_string(),
            budget: None,
            level: Some(lower),
        });
    }

    // 4. OpenAI Responses reasoning object
    if let Some(reasoning) = body.get("reasoning").and_then(|v| v.as_object()) {
        if let Some(eff) = reasoning.get("effort").and_then(|v| v.as_str()) {
            let lower = eff.to_lowercase();
            if matches!(lower.as_str(), "none" | "off" | "disabled") {
                return Some(ThinkingConfig {
                    mode: "none".to_string(),
                    budget: None,
                    level: None,
                });
            }
            return Some(ThinkingConfig {
                mode: "level".to_string(),
                budget: None,
                level: Some(lower),
            });
        }
    }

    // 5. Gemini generationConfig.thinkingConfig
    if let Some(tc) = body
        .get("generationConfig")
        .and_then(|g| g.get("thinkingConfig"))
        .and_then(|v| v.as_object())
    {
        if tc.get("includeThoughts").and_then(|v| v.as_bool()) == Some(false) {
            return Some(ThinkingConfig {
                mode: "none".to_string(),
                budget: None,
                level: None,
            });
        }
        if let Some(b) = tc.get("thinkingBudget").and_then(|v| v.as_u64()) {
            if b == 0 {
                return Some(ThinkingConfig {
                    mode: "none".to_string(),
                    budget: None,
                    level: None,
                });
            }
            return Some(ThinkingConfig {
                mode: "budget".to_string(),
                budget: Some(b),
                level: None,
            });
        }
        if let Some(lvl) = tc.get("thinkingLevel").and_then(|v| v.as_str()) {
            return Some(ThinkingConfig {
                mode: "level".to_string(),
                budget: None,
                level: Some(lvl.to_lowercase()),
            });
        }
    }

    // 6. Qwen / DeepSeek enable_thinking
    if let Some(et) = body.get("enable_thinking").and_then(|v| v.as_bool()) {
        if !et {
            return Some(ThinkingConfig {
                mode: "none".to_string(),
                budget: None,
                level: None,
            });
        }
        if let Some(b) = body.get("thinking_budget").and_then(|v| v.as_u64()) {
            return Some(ThinkingConfig {
                mode: "budget".to_string(),
                budget: Some(b),
                level: None,
            });
        }
        return Some(ThinkingConfig {
            mode: "auto".to_string(),
            budget: None,
            level: None,
        });
    }

    None
}

pub fn capture_thinking(body: &Value) -> Option<ThinkingConfig> {
    extract_thinking(body)
}

/// Apply captured thinking intent onto an already-translated body for `target_format`.
pub fn apply_thinking(
    target_format: &str,
    _model: &str,
    body: &mut Value,
    intent: Option<&ThinkingConfig>,
) {
    let Some(cfg) = intent else {
        return;
    };
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    match target_format {
        "claude" => match cfg.mode.as_str() {
            "none" => {
                obj.remove("thinking");
                obj.remove("output_config");
            }
            "budget" => {
                if let Some(b) = cfg.budget {
                    obj.insert(
                        "thinking".to_string(),
                        json!({ "type": "enabled", "budget_tokens": b }),
                    );
                }
            }
            "level" => {
                if let Some(lvl) = &cfg.level {
                    let b = effort_to_budget(lvl).unwrap_or(8192);
                    if b == 0 {
                        obj.remove("thinking");
                    } else {
                        obj.insert(
                            "thinking".to_string(),
                            json!({ "type": "enabled", "budget_tokens": b }),
                        );
                    }
                }
            }
            "auto" => {
                obj.entry("thinking")
                    .or_insert_with(|| json!({ "type": "enabled", "budget_tokens": 8192 }));
            }
            _ => {}
        },
        "openai" => match cfg.mode.as_str() {
            "none" => {
                obj.remove("reasoning_effort");
            }
            "level" => {
                if let Some(lvl) = &cfg.level {
                    obj.insert("reasoning_effort".to_string(), Value::String(lvl.clone()));
                }
            }
            "budget" => {
                if let Some(b) = cfg.budget {
                    obj.insert(
                        "reasoning_effort".to_string(),
                        Value::String(budget_to_effort(b).to_string()),
                    );
                }
            }
            _ => {}
        },
        "openai-responses" | "codex" => match cfg.mode.as_str() {
            "none" => {
                obj.remove("reasoning");
            }
            "level" => {
                if let Some(lvl) = &cfg.level {
                    let entry = obj
                        .entry("reasoning")
                        .or_insert_with(|| Value::Object(Map::new()));
                    if let Some(robj) = entry.as_object_mut() {
                        robj.insert("effort".to_string(), Value::String(lvl.clone()));
                    }
                }
            }
            "budget" => {
                if let Some(b) = cfg.budget {
                    let entry = obj
                        .entry("reasoning")
                        .or_insert_with(|| Value::Object(Map::new()));
                    if let Some(robj) = entry.as_object_mut() {
                        robj.insert(
                            "effort".to_string(),
                            Value::String(budget_to_effort(b).to_string()),
                        );
                    }
                }
            }
            _ => {}
        },
        "gemini" | "gemini-cli" | "vertex" | "antigravity" => {
            let gen_cfg = obj
                .entry("generationConfig")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(gobj) = gen_cfg.as_object_mut() {
                match cfg.mode.as_str() {
                    "none" => {
                        gobj.insert(
                            "thinkingConfig".to_string(),
                            json!({ "includeThoughts": false, "thinkingBudget": 0 }),
                        );
                    }
                    "budget" => {
                        if let Some(b) = cfg.budget {
                            gobj.insert(
                                "thinkingConfig".to_string(),
                                json!({ "includeThoughts": true, "thinkingBudget": b }),
                            );
                        }
                    }
                    "level" => {
                        if let Some(lvl) = &cfg.level {
                            let b = effort_to_budget(lvl).unwrap_or(8192);
                            gobj.insert(
                                "thinkingConfig".to_string(),
                                json!({ "includeThoughts": b > 0, "thinkingBudget": b }),
                            );
                        }
                    }
                    "auto" => {
                        gobj.entry("thinkingConfig")
                            .or_insert_with(|| json!({ "includeThoughts": true }));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
