use std::collections::BTreeMap;

pub fn upstream_response_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if name == "retry-after"
            || name == "x-should-retry"
            || name.starts_with("anthropic-ratelimit-")
        {
            result.insert(name, value.to_owned());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwards_only_approved_headers() {
        let result = upstream_response_headers([
            ("Retry-After", "10"),
            ("Anthropic-RateLimit-Tokens-Remaining", "4"),
            ("Set-Cookie", "secret"),
        ]);
        assert_eq!(result.len(), 2);
        assert_eq!(result["retry-after"], "10");
        assert!(!result.contains_key("set-cookie"));
    }
}
