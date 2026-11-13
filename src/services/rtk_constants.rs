//! RTK caps mirrored from `open-sse/rtk/constants.js` (Rust pipe_cmd.rs parity).

pub const RAW_CAP: usize = 10 * 1024 * 1024;
pub const MIN_COMPRESS_SIZE: usize = 500;
pub const DETECT_WINDOW: usize = 1024;
pub const GIT_DIFF_HUNK_MAX_LINES: usize = 100;
pub const GIT_LOG_MAX_LINES: usize = 200;
pub const DEDUP_LINE_MAX: usize = 2000;

pub const GREP_PER_FILE_MAX: usize = 10;
pub const FIND_PER_DIR_MAX: usize = 10;
pub const FIND_TOTAL_DIR_MAX: usize = 20;

pub const STATUS_MAX_FILES: usize = 10;
pub const STATUS_MAX_UNTRACKED: usize = 10;

pub const LS_EXT_SUMMARY_TOP: usize = 5;
pub const LS_NOISE_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "__pycache__",
    ".next", "dist", "build", ".cache", ".turbo",
    ".vercel", ".pytest_cache", ".mypy_cache", ".tox",
    ".venv", "venv",
    "env",
    "coverage", ".nyc_output", ".DS_Store", "Thumbs.db",
    ".idea", ".vscode", ".vs", "*.egg-info", ".eggs",
];

pub const TREE_MAX_LINES: usize = 200;

pub const SEARCH_LIST_PER_DIR_MAX: usize = 10;
pub const SEARCH_LIST_TOTAL_DIR_MAX: usize = 20;

pub const SMART_TRUNCATE_HEAD: usize = 120;
pub const SMART_TRUNCATE_TAIL: usize = 60;
pub const SMART_TRUNCATE_MIN_LINES: usize = 250;

pub const READ_NUMBERED_MIN_HIT_RATIO: f64 = 0.7;

pub const FILTER_GIT_DIFF: &str = "git-diff";
pub const FILTER_GIT_STATUS: &str = "git-status";
pub const FILTER_GIT_LOG: &str = "git-log";
pub const FILTER_GREP: &str = "grep";
pub const FILTER_FIND: &str = "find";
pub const FILTER_LS: &str = "ls";
pub const FILTER_TREE: &str = "tree";
pub const FILTER_DEDUP_LOG: &str = "dedup-log";
pub const FILTER_SMART_TRUNCATE: &str = "smart-truncate";
pub const FILTER_READ_NUMBERED: &str = "read-numbered";
pub const FILTER_SEARCH_LIST: &str = "search-list";
pub const FILTER_BUILD_OUTPUT: &str = "build-output";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_match_js_source_values() {
        assert_eq!(RAW_CAP, 10 * 1024 * 1024);
        assert_eq!(GREP_PER_FILE_MAX, 10);
        assert_eq!(FIND_TOTAL_DIR_MAX, 20);
        assert_eq!(SMART_TRUNCATE_HEAD + SMART_TRUNCATE_TAIL, 180);
        assert!(LS_NOISE_DIRS.contains(&"node_modules"));
        assert!(!LS_NOISE_DIRS.contains(&".env"));
        assert_eq!(FILTER_GIT_DIFF, "git-diff");
    }
}
