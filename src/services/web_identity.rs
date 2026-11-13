use std::collections::HashMap;

pub fn cookie_jar(raw: &str) -> HashMap<String, String> {
    raw.split(';')
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            if key.is_empty() {
                return None;
            }
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_cookies_with_equals_and_last_value_wins() {
        let jar = cookie_jar("a=one; token=base64==; a=two; invalid; =no");
        assert_eq!(jar.get("a").map(String::as_str), Some("two"));
        assert_eq!(jar.get("token").map(String::as_str), Some("base64=="));
        assert_eq!(jar.get("").map(String::as_str), Some("no"));
        assert_eq!(jar.len(), 3);
    }
}
