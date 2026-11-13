// ponytail: requires browser automation; implement a browser-backed transport to support it.
pub(super) fn unsupported() -> anyhow::Error {
    anyhow::anyhow!("Unsupported: gemini-web requires a real browser")
}
