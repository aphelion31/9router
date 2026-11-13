use serde_json::Value;

pub(super) fn prepare(model: &str, body: &mut Value, account: bool) {
    if !account
        || !matches!(
            model.rsplit('/').next().unwrap_or(model),
            "mimo-v2.6-pro" | "mimo-v2.6-flash" | "mimo-v2.6-pro-ultraspeed"
        )
    {
        return;
    }
    if let Some(o) = body.as_object_mut() {
        if let Some(effort) = o
            .remove("reasoning_effort")
            .and_then(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
        {
            let effort = if effort == "xhigh" { "high" } else { &effort };
            if !o.get("output_config").is_some_and(Value::is_object) {
                o.insert("output_config".into(), serde_json::json!({}));
            }
            o["output_config"]["effort"] = effort.into();
        }
        o.entry("temperature").or_insert(1.0.into());
        o.entry("top_p").or_insert(0.95.into());
    }
}
