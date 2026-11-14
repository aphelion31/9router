use serde_json::Value;

fn parse(value: &Value) -> Option<Option<usize>> {
    let n = match value { Value::Number(n) => n.as_f64(), Value::String(s) if !s.trim().is_empty() => s.trim().parse::<f64>().ok(), _ => None }?;
    if !n.is_finite() { return None; }
    if n == 0.0 { Some(None) } else { Some(Some(n.floor().clamp(1.0, 10.0) as usize)) }
}
pub fn resolve_combo_member_attempt_cap(settings: &Value, provider: &str, model: &str) -> Option<usize> {
    let overrides = &settings["comboMemberAttemptCapOverrides"];
    if !provider.is_empty() {
        if !model.is_empty() { if let Some(cap) = parse(&overrides[format!("{provider}/{model}")]) { return cap; } }
        if let Some(cap) = parse(&overrides[provider]) { return cap; }
    }
    parse(&settings["comboMemberAttemptCap"]).unwrap_or(Some(3))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn cap_resolution() {
        assert_eq!(resolve_combo_member_attempt_cap(&json!({}),"p","m"),Some(3));
        let s=json!({"comboMemberAttemptCap":7,"comboMemberAttemptCapOverrides":{"p":0,"p/m":"12"}});
        assert_eq!(resolve_combo_member_attempt_cap(&s,"p","m"),Some(10));
        assert_eq!(resolve_combo_member_attempt_cap(&s,"p","other"),None);
        assert_eq!(resolve_combo_member_attempt_cap(&s,"q","m"),Some(7));
    }
}
