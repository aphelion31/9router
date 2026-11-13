//! Host-side browser executable classification, ported from browserExecutableCheck.js.
//! Missing Chrome or missing display is a host/config problem, not a transient upstream fault.

use std::path::{Path, PathBuf};

/// True when a browser launch error means the binary is not installed.
pub(crate) fn is_missing_browser_executable(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("executable doesn't exist")
        || lower.contains("executablenotfound")
        || lower.contains("playwright install")
        || (lower.contains("chromium") && lower.contains("download"))
}

/// True when a headed browser failed because no X display is available.
pub(crate) fn is_missing_display(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("xserver")
        || lower.contains("x server")
        || lower.contains("headed browser without")
        || lower.contains("$display")
}

/// First existing system Chrome path, honoring the same env overrides as the JS helper.
pub(crate) fn resolve_system_chrome_executable() -> Option<PathBuf> {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(v) = env("NINEROUTER_BROWSER_BIN") {
        candidates.push(v.into());
    }
    if let Some(v) = env("CHROME_PATH") {
        candidates.push(v.into());
    }
    candidates.extend(
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ]
        .map(PathBuf::from),
    );
    if let Some(root) = env("PROGRAMFILES") {
        candidates.push(Path::new(&root).join("Google/Chrome/Application/chrome.exe"));
    }
    if let Some(root) = env("PROGRAMFILES(X86)") {
        candidates.push(Path::new(&root).join("Google/Chrome/Application/chrome.exe"));
    }
    if let Some(root) = env("LOCALAPPDATA") {
        candidates.push(Path::new(&root).join("Google/Chrome/Application/chrome.exe"));
    }
    candidates
        .into_iter()
        .find(|p| p.as_os_str().to_string_lossy().trim().len() > 0 && p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_host_errors() {
        assert!(is_missing_browser_executable("chromium.launch: Executable doesn't exist at /x"));
        assert!(is_missing_browser_executable("Please run playwright install"));
        assert!(!is_missing_browser_executable("upstream 503"));
        assert!(is_missing_display("Looks like you launched a headed browser without having a XServer running"));
        assert!(!is_missing_display("timeout"));
    }

    #[test]
    fn env_override_wins_when_file_exists() {
        let path = std::env::temp_dir().join("ninerouter-fake-chrome");
        std::fs::write(&path, b"").unwrap();
        // SAFETY: test-only env mutation; this test is the only one touching NINEROUTER_BROWSER_BIN.
        unsafe { std::env::set_var("NINEROUTER_BROWSER_BIN", &path) };
        assert_eq!(resolve_system_chrome_executable(), Some(path.clone()));
        unsafe { std::env::remove_var("NINEROUTER_BROWSER_BIN") };
        std::fs::remove_file(path).unwrap();
    }
}
