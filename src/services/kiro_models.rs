use serde_json::{Value, json};

pub fn region_from_profile_arn(arn: &str) -> &str {
    arn.split(':')
        .nth(3)
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1")
}

pub fn expand_kiro_catalog(raw: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for model in raw {
        let Some(id) = model["modelId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| model["id"].as_str().filter(|s| !s.is_empty()))
        else {
            continue;
        };
        let safe = id.strip_suffix("-agentic").unwrap_or(id);
        let safe = safe.strip_suffix("-thinking").unwrap_or(safe);
        let base = model["modelName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(id)
            .trim();
        let rate = model["rateMultiplier"]
            .as_f64()
            .or_else(|| {
                model["rateMultiplier"]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .filter(|n| n.is_finite())
            .unwrap_or(1.0);
        let display = if rate > 0.0 && (rate - 1.0).abs() >= 1e-9 {
            format!("Kiro {base} ({rate:.1}x credit)")
        } else {
            format!("Kiro {base}")
        };
        let context = model["tokenLimits"]["maxInputTokens"]
            .as_f64()
            .or_else(|| {
                model["tokenLimits"]["maxInputTokens"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
            })
            .filter(|n| *n != 0.0 && n.is_finite())
            .unwrap_or(200000.0);
        let mut variants = vec![
            ("", "", false, false),
            ("-thinking", " (Thinking)", true, false),
        ];
        if safe != "auto" {
            variants.extend([
                ("-agentic", " (Agentic)", false, true),
                ("-thinking-agentic", " (Thinking + Agentic)", true, true),
            ]);
        }
        for (suffix, label, thinking, agentic) in variants {
            out.push(json!({"id":format!("{safe}{suffix}"),"name":format!("{display}{label}"),"capabilities":{"thinking":thinking,"agentic":agentic},"contextLength":context,"rateMultiplier":rate,"upstreamModelId":id,"description":model["description"].as_str().unwrap_or("")}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_variants_and_skips_auto_agentic() {
        let models = expand_kiro_catalog(&[
            json!({"modelId":"auto"}),
            json!({"modelId":"claude","modelName":"Claude","rateMultiplier":2}),
        ]);
        assert_eq!(models.len(), 6);
        assert_eq!(models[0]["id"], "auto");
        assert_eq!(models[2]["name"], "Kiro Claude (2.0x credit)");
        assert_eq!(models[5]["id"], "claude-thinking-agentic");
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:eu-west-1:foo"),
            "eu-west-1"
        );
    }
}
