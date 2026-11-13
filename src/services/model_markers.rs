pub fn strip_model_context_marker(model: &str) -> (&str, Option<&'static str>) {
    let trimmed = model.trim();
    if trimmed
        .get(trimmed.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case("[1m]"))
    {
        return (&trimmed[..trimmed.len() - 4], Some("1m"));
    }
    (model, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_only_final_context_marker() {
        assert_eq!(
            strip_model_context_marker(" claude[1M] "),
            ("claude", Some("1m"))
        );
        assert_eq!(strip_model_context_marker(" claude "), (" claude ", None));
        assert_eq!(strip_model_context_marker("é"), ("é", None));
    }
}
