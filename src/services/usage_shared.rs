use serde_json::Value;

pub fn to_finite_number(value: &Value, fallback: f64) -> f64 {
    value
        .as_f64()
        .or_else(|| {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .and_then(|s| s.trim().parse::<f64>().ok())
        })
        .filter(|n| n.is_finite())
        .unwrap_or(fallback)
}

pub fn normalize_cloud_code_project_id(project: &Value) -> Option<&str> {
    project
        .as_str()
        .or_else(|| project["id"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn finite_values_and_project_ids() {
        assert_eq!(to_finite_number(&json!(" 2.5 "), 0.0), 2.5);
        assert_eq!(to_finite_number(&json!("NaN"), 4.0), 4.0);
        assert_eq!(
            normalize_cloud_code_project_id(&json!({"id":" abc "})),
            Some("abc")
        );
        assert_eq!(normalize_cloud_code_project_id(&json!("  ")), None);
    }
}
