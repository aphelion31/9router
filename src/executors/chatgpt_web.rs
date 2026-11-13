// ponytail: requires a headed browser for Sentinel and Turnstile; add browser automation to support it.
pub(super) fn unsupported() -> anyhow::Error {
    anyhow::anyhow!("Unsupported: chatgpt-web requires a real browser")
}
