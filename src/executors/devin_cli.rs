pub(super) fn unsupported() -> anyhow::Error {
    // Upstream auto-approves shell commands. Never silently expose host execution.
    anyhow::anyhow!(
        "Unsupported: Devin ACP execution requires a sandbox and explicit permission policy"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn refuses_unsafe_host_execution() {
        assert!(super::unsupported().to_string().contains("sandbox"));
    }
}
