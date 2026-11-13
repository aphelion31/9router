// ponytail: requires Playwright DOM automation; implement a browser-backed transport to support it.
pub(super) fn unsupported() -> anyhow::Error {
    anyhow::anyhow!("Unsupported: muse-web requires a real browser")
}
