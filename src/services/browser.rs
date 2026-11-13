#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("browser operation unsupported: {0}")]
pub struct Unsupported(pub &'static str);

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

pub fn is_pool_enabled(flag: Option<&str>) -> bool {
    !matches!(flag, Some("off" | "0" | "false"))
}

pub fn has_display(display: Option<&str>, wayland_display: Option<&str>) -> bool {
    display.is_some_and(|s| !s.is_empty()) || wayland_display.is_some_and(|s| !s.is_empty())
}

// ponytail: browser automation and virtual-display management need a browser runtime; add one when supported.
pub fn browser_pool() -> Result<(), Unsupported> {
    Err(Unsupported("browserPool"))
}
pub fn web_login() -> Result<(), Unsupported> {
    Err(Unsupported("webLogin"))
}
pub fn virtual_display() -> Result<(), Unsupported> {
    Err(Unsupported("virtualDisplay"))
}
pub fn obscura() -> Result<(), Unsupported> {
    Err(Unsupported("obscura"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_boundaries_are_explicit() {
        assert!(is_pool_enabled(None));
        assert!(!is_pool_enabled(Some("false")));
        assert!(has_display(None, Some("wayland-0")));
        assert!(!has_display(Some(""), None));
        assert_eq!(browser_pool().unwrap_err(), Unsupported("browserPool"));
        assert!(web_login().is_err() && virtual_display().is_err() && obscura().is_err());
    }
}
