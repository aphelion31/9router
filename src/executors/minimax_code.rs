pub(super) fn is_quota(status: u16, body: &str) -> bool {
    if !matches!(status, 402 | 403 | 429) {
        return false;
    }
    let text = body.to_lowercase();
    [
        "insufficient",
        "balance",
        "credit",
        "quota",
        "exhaust",
        "limit",
        "余额",
        "积分",
        "额度",
        "不足",
        "用完",
        "上限",
    ]
    .iter()
    .any(|s| text.contains(s))
}
