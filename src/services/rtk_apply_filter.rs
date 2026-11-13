// Port of applyFilter.js (rtk/src/cmds/system/pipe_cmd.rs) — catch_unwind wrapper.
// On panic: passthrough raw output + warn via tracing.

pub fn safe_apply(filter: Option<fn(&str) -> String>, name: &str, text: &str) -> String {
    let Some(filter) = filter else {
        return text.to_owned();
    };
    let text_owned = text.to_owned();
    std::panic::catch_unwind(move || filter(&text_owned)).unwrap_or_else(|_| {
        tracing::warn!("[rtk] warning: filter '{name}' panicked — passing through raw output");
        text.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_apply_returns_raw_on_panic_or_none() {
        fn panicking(_: &str) -> String {
            panic!("boom");
        }
        assert_eq!(safe_apply(None, "none", "hello"), "hello");
        assert_eq!(safe_apply(Some(panicking), "boom", "hello"), "hello");
    }
}
