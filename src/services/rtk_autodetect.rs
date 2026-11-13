// Port of autodetect.js (rtk/src/cmds/system/pipe_cmd.rs)
// Detection order: git-log -> git-diff -> git-status -> build-output -> grep -> find -> tree -> ls
//                  -> search-list -> read-numbered -> dedup-log -> smart-truncate -> None

use crate::services::rtk_filters;

use crate::services::rtk_constants::{DETECT_WINDOW, SMART_TRUNCATE_MIN_LINES, READ_NUMBERED_MIN_HIT_RATIO};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    GitLog,
    GitDiff,
    GitStatus,
    BuildOutput,
    Grep,
    Find,
    Tree,
    Ls,
    SearchList,
    ReadNumbered,
    DedupLog,
    SmartTruncate,
}

impl FilterKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::GitLog => crate::services::rtk_constants::FILTER_GIT_LOG,
            Self::GitDiff => crate::services::rtk_constants::FILTER_GIT_DIFF,
            Self::GitStatus => crate::services::rtk_constants::FILTER_GIT_STATUS,
            Self::BuildOutput => crate::services::rtk_constants::FILTER_BUILD_OUTPUT,
            Self::Grep => crate::services::rtk_constants::FILTER_GREP,
            Self::Find => crate::services::rtk_constants::FILTER_FIND,
            Self::Tree => crate::services::rtk_constants::FILTER_TREE,
            Self::Ls => crate::services::rtk_constants::FILTER_LS,
            Self::SearchList => crate::services::rtk_constants::FILTER_SEARCH_LIST,
            Self::ReadNumbered => crate::services::rtk_constants::FILTER_READ_NUMBERED,
            Self::DedupLog => crate::services::rtk_constants::FILTER_DEDUP_LOG,
            Self::SmartTruncate => crate::services::rtk_constants::FILTER_SMART_TRUNCATE,
        }
    }

    pub fn filter_fn(self) -> fn(&str) -> String {
        match self {
            Self::GitLog => |s| rtk_filters::git_log(s, crate::services::rtk_constants::GIT_LOG_MAX_LINES),
            Self::GitDiff => |s| rtk_filters::git_diff(s, 500),
            Self::GitStatus => rtk_filters::git_status,
            Self::BuildOutput => rtk_filters::build_output,
            Self::Grep => rtk_filters::grep,
            Self::Find => rtk_filters::find,
            Self::Tree => rtk_filters::tree,
            Self::Ls => rtk_filters::ls,
            Self::SearchList => rtk_filters::search_list,
            Self::ReadNumbered => rtk_filters::read_numbered,
            Self::DedupLog => rtk_filters::dedup_log,
            Self::SmartTruncate => rtk_filters::smart_truncate,
        }
    }
}

fn floor_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

fn is_git_log(head: &str) -> bool {
    head.lines().any(|l| {
        let rest = l.trim_start_matches(['*', '|', '/', '\\', ' ']);
        rest.strip_prefix("commit ").is_some_and(|sha| {
            let token = sha.split_whitespace().next().unwrap_or("");
            (7..=40).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit())
        })
    })
}

fn is_git_diff(head: &str) -> bool {
    head.lines()
        .any(|l| l.starts_with("diff --git ") || l.starts_with("@@ "))
}

fn is_git_status_header(head: &str) -> bool {
    head.lines().any(|l| {
        l.starts_with("On branch ")
            || l.starts_with("nothing to commit")
            || l.starts_with("Changes not ")
            || l.starts_with("Changes to be ")
            || l.starts_with("Untracked files:")
    })
}

fn is_build_output(head: &str) -> bool {
    head.lines().any(|l| {
        let lower = l.to_ascii_lowercase();
        let trimmed = lower.trim_start();
        lower.starts_with("npm warn")
            || lower.starts_with("npm error")
            || lower.starts_with("npm err!")
            || lower.starts_with("yarn warn")
            || lower.starts_with("yarn error")
            || trimmed.starts_with("compiling ")
            || trimmed.starts_with("downloading ")
            || (lower.starts_with("added ") && lower.contains("package"))
            || lower.starts_with("[error]")
            || lower.starts_with("build success")
            || lower.starts_with("build failed")
            || trimmed.starts_with("finished ")
            || lower.starts_with("successfully installed")
            || lower.starts_with("successfully built")
            || lower.starts_with("error:")
    })
}

fn is_porcelain_line(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < 4 || b[2] != b' ' || b[3].is_ascii_whitespace() {
        return false;
    }
    let valid = |c: u8| matches!(c, b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?' | b'!');
    valid(b[0]) && valid(b[1])
}

fn is_mostly_porcelain(head: &str) -> bool {
    let lines: Vec<&str> = head.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let hits = lines.iter().filter(|l| is_porcelain_line(l)).count();
    (hits as f64) / (lines.len() as f64) >= 0.6
}

fn is_grep_line(line: &str) -> bool {
    let Some(first) = line.find(':') else {
        return false;
    };
    let rest = &line[first + 1..];
    let Some(second) = rest.find(':') else {
        return false;
    };
    let lineno = &rest[..second];
    !lineno.is_empty() && lineno.chars().all(|c| c.is_ascii_digit())
}

fn is_path_like(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let b = t.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return true;
    }
    if t.contains(':') {
        return false;
    }
    t.starts_with('.') || t.starts_with('/') || t.contains('/')
}

fn is_ls_row(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 10
        && matches!(b[0], b'-' | b'd' | b'l' | b'b' | b'c' | b'p' | b's')
        && b[1..10]
            .iter()
            .all(|&c| matches!(c, b'r' | b'w' | b'x' | b'-'))
}

fn is_search_list_header(head: &str) -> bool {
    head.lines().any(|l| {
        l.starts_with("Result of search in '")
            && l.contains("(total ")
            && l.contains(" files):")
    })
}

fn is_read_numbered_line(line: &str) -> bool {
    let Some((num, _)) = line.split_once('|') else {
        return false;
    };
    let t = num.trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

fn is_line_numbered(lines: &[&str]) -> bool {
    let mut hits = 0usize;
    let mut non_empty = 0usize;
    for l in lines.iter().take(100) {
        if l.is_empty() {
            continue;
        }
        non_empty += 1;
        if is_read_numbered_line(l) {
            hits += 1;
        }
    }
    if non_empty < 5 {
        return false;
    }
    (hits as f64) / (non_empty as f64) >= READ_NUMBERED_MIN_HIT_RATIO
}

pub fn auto_detect_filter(text: &str) -> Option<FilterKind> {
    let head = floor_char_boundary(text, DETECT_WINDOW);

    if is_git_log(head) {
        return Some(FilterKind::GitLog);
    }
    if is_git_diff(head) {
        return Some(FilterKind::GitDiff);
    }
    if is_git_status_header(head) {
        return Some(FilterKind::GitStatus);
    }
    if is_build_output(head) {
        return Some(FilterKind::BuildOutput);
    }
    if is_mostly_porcelain(head) {
        return Some(FilterKind::GitStatus);
    }

    let lines: Vec<&str> = head.split('\n').collect();
    let non_empty: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();

    if non_empty.iter().take(5).any(|l| is_grep_line(l)) {
        return Some(FilterKind::Grep);
    }
    if non_empty.len() >= 3 && non_empty.iter().all(|l| is_path_like(l)) {
        return Some(FilterKind::Find);
    }
    if head.contains("├──") || head.contains("└──") || head.contains("│  ") {
        return Some(FilterKind::Tree);
    }
    if head.lines().any(|l| {
        l.strip_prefix("total ")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    }) || head.lines().filter(|l| is_ls_row(l)).count() >= 3
    {
        return Some(FilterKind::Ls);
    }
    if is_search_list_header(head) {
        return Some(FilterKind::SearchList);
    }
    if lines.len() >= SMART_TRUNCATE_MIN_LINES && is_line_numbered(&lines) {
        return Some(FilterKind::ReadNumbered);
    }
    if non_empty.len() >= 5 {
        return Some(FilterKind::DedupLog);
    }
    if text.split('\n').count() >= SMART_TRUNCATE_MIN_LINES {
        return Some(FilterKind::SmartTruncate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_tool_outputs() {
        assert_eq!(
            auto_detect_filter("commit 1234567\nAuthor: a\nDate: b"),
            Some(FilterKind::GitLog)
        );
        assert_eq!(
            auto_detect_filter("diff --git a/x b/x\n@@ -1 +1 @@"),
            Some(FilterKind::GitDiff)
        );
        assert_eq!(
            auto_detect_filter("   Compiling foo v0.1.0\n   Finished dev"),
            Some(FilterKind::BuildOutput)
        );
        assert_eq!(
            auto_detect_filter("src/a.rs:10: fn main() {}"),
            Some(FilterKind::Grep)
        );
        assert_eq!(
            auto_detect_filter("./a.rs\n./b.rs\n./c.rs"),
            Some(FilterKind::Find)
        );
    }
}
