use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};

fn strip_prefix(raw: &str) -> &str {
    let raw = raw.trim();
    raw.split_once('#').map_or(raw, |(_, rest)| rest.trim())
}
fn decode(input: &[u8]) -> Option<Vec<u8>> {
    STANDARD
        .decode(input)
        .or_else(|_| STANDARD_NO_PAD.decode(input))
        .ok()
}
pub fn is_valid_claude_signature(raw: &str) -> bool {
    let signature = strip_prefix(raw);
    if signature.len() > 32 * 1024 * 1024 {
        return false;
    }
    match signature.as_bytes().first() {
        Some(b'E') => decode(signature.as_bytes()).is_some_and(|d| d.first() == Some(&0x12)),
        Some(b'R') => decode(signature.as_bytes())
            .filter(|d| d.first() == Some(&b'E'))
            .and_then(|d| decode(&d))
            .is_some_and(|d| d.first() == Some(&0x12)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_both_signature_layers() {
        assert!(is_valid_claude_signature("Eg=="));
        assert!(is_valid_claude_signature("cache#RWc9PQ=="));
        assert!(!is_valid_claude_signature("not-a-signature"));
    }
}
