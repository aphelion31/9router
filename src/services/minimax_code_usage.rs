use serde_json::{Value, json};

pub fn parse_membership(value: &Value) -> Value {
    let pick = |key: &str| {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .unwrap_or(&value["data"][key])
    };
    let mut out = json!({});
    if let Some(has) = pick("has_token_plan").as_bool() {
        out["hasTokenPlan"] = json!(has);
    }
    for (src, dst) in [("op_group_id", "opGroupId"), ("token_plan_tier", "tier")] {
        if let Some(s) = pick(src).as_str().map(str::trim).filter(|s| !s.is_empty()) {
            out[dst] = json!(s);
        }
    }
    if let Some(n) = pick("token_plan_expires_at").as_f64().filter(|n| *n > 0.0) {
        out["expiresAt"] = json!(n);
    }
    let summary = value
        .get("op_credit_summary")
        .filter(|v| v.is_object())
        .unwrap_or(&value["data"]["op_credit_summary"]);
    let balance = summary["total_remaining_amount"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let balance = pick("opcredit_balance");
            balance
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| balance.as_number().map(ToString::to_string))
        });
    if let Some(balance) = balance {
        out["balance"] = json!(balance);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_extracts_nested_plan_and_credit_balance() {
        let membership = parse_membership(
            &json!({"data":{"has_token_plan":true,"op_group_id":" group ","token_plan_tier":"pro","op_credit_summary":{"total_remaining_amount":" 2.5 "}}}),
        );
        assert_eq!(
            membership,
            json!({"hasTokenPlan":true,"opGroupId":"group","tier":"pro","balance":"2.5"})
        );
        assert_eq!(parse_membership(&Value::Null), json!({}));
    }
}
