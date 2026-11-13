pub fn smart_truncate(input: &str) -> String {
    truncate(input, false)
}

pub fn safe_apply(filter: Option<fn(&str) -> String>, text: &str) -> String {
    let Some(filter) = filter else {
        return text.to_owned();
    };
    std::panic::catch_unwind(|| filter(text)).unwrap_or_else(|_| {
        tracing::warn!("rtk filter panicked, passing through raw output");
        text.to_owned()
    })
}

pub fn read_numbered(input: &str) -> String {
    truncate(input, true)
}

pub fn git_diff(diff: &str, max_lines: usize) -> String {
    let mut result = Vec::new();
    let mut file = "";
    let (mut added, mut removed, mut shown, mut skipped) = (0, 0, 0, 0);
    let (mut in_hunk, mut truncated) = (false, false);
    for line in diff.split('\n') {
        if line.starts_with("diff --git") {
            if skipped > 0 {
                result.push(format!("  ... ({skipped} lines truncated)"));
                truncated = true;
                skipped = 0;
            }
            if !file.is_empty() && (added > 0 || removed > 0) {
                result.push(format!("  +{added} -{removed}"));
            }
            file = line.split_once(" b/").map_or("unknown", |(_, name)| name);
            result.push(format!("\n{file}"));
            (added, removed, shown) = (0, 0, 0);
            in_hunk = false;
        } else if line.starts_with("@@") {
            if skipped > 0 {
                result.push(format!("  ... ({skipped} lines truncated)"));
                truncated = true;
                skipped = 0;
            }
            in_hunk = true;
            shown = 0;
            result.push(format!("  {line}"));
        } else if in_hunk {
            if (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
            {
                if line.starts_with('+') {
                    added += 1;
                } else {
                    removed += 1;
                }
                if shown < 100 {
                    result.push(format!("  {line}"));
                    shown += 1;
                } else {
                    skipped += 1;
                }
            } else if shown > 0 && shown < 100 && !line.starts_with('\\') {
                result.push(format!("  {line}"));
                shown += 1;
            }
        }
        if result.len() >= max_lines {
            result.push("\n... (more changes truncated)".into());
            truncated = true;
            break;
        }
    }
    if skipped > 0 {
        result.push(format!("  ... ({skipped} lines truncated)"));
        truncated = true;
    }
    if !file.is_empty() && (added > 0 || removed > 0) {
        result.push(format!("  +{added} -{removed}"));
    }
    if truncated {
        result.push("[full diff: rtk git diff --no-compact]".into());
    }
    result.join("\n")
}

pub fn search_list(input: &str) -> String {
    let mut lines = input.split('\n');
    let header = lines.next().unwrap_or("");
    let paths: Vec<_> = lines
        .filter_map(|line| line.trim().strip_prefix("- "))
        .collect();
    if paths.is_empty() {
        return input.into();
    }
    let mut directories = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for path in &paths {
        let (dir, name) = match path.rfind('/') {
            Some(i) => (if i == 0 { "/" } else { &path[..i] }, &path[i + 1..]),
            None => (".", *path),
        };
        directories.entry(dir).or_default().push(name);
    }
    let mut out = format!(
        "{header}\n{} files in {} dirs:\n\n",
        paths.len(),
        directories.len()
    );
    for (dir, names) in directories.iter().take(20) {
        out.push_str(&format!("{dir}/ ({}):\n", names.len()));
        for name in names.iter().take(10) {
            out.push_str(&format!("  {name}\n"));
        }
        if names.len() > 10 {
            out.push_str(&format!("  +{}\n", names.len() - 10));
        }
        out.push('\n');
    }
    if directories.len() > 20 {
        out.push_str(&format!("+{} more dirs\n", directories.len() - 20));
    }
    out.trim_end_matches('\n').to_owned()
}

pub fn grep(input: &str) -> String {
    let mut files = std::collections::BTreeMap::<&str, Vec<(&str, &str)>>::new();
    let mut total = 0;
    for line in input.split('\n') {
        let mut fields = line.splitn(3, ':');
        let (Some(file), Some(number), Some(content)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        total += 1;
        files.entry(file).or_default().push((number, content));
    }
    if total == 0 {
        return input.into();
    }
    let mut out = format!("{total} matches in {}F:\n\n", files.len());
    for (file, matches) in files {
        out.push_str(&format!("[file] {file} ({}):\n", matches.len()));
        for (number, content) in matches.iter().take(10) {
            out.push_str(&format!("  {number:>4}: {}\n", content.trim()));
        }
        if matches.len() > 10 {
            out.push_str(&format!("  +{}\n", matches.len() - 10));
        }
        out.push('\n');
    }
    out
}

pub fn find(input: &str) -> String {
    let lines: Vec<_> = input
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return input.into();
    }
    let mut directories = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for path in &lines {
        let (directory, name) = match path.rfind(['/', '\\']) {
            Some(index) => (
                if index == 0 { "/" } else { &path[..index] },
                &path[index + 1..],
            ),
            None => (".", *path),
        };
        directories.entry(directory).or_default().push(name);
    }
    let mut out = format!("{} files in {} dirs:\n\n", lines.len(), directories.len());
    for (dir, files) in directories.iter().take(20) {
        out.push_str(&format!("{}/  ({})\n", dir.replace('\\', "/"), files.len()));
        for file in files.iter().take(10) {
            out.push_str(&format!("  {file}\n"));
        }
        if files.len() > 10 {
            out.push_str(&format!("  +{}\n", files.len() - 10));
        }
    }
    if directories.len() > 20 {
        out.push_str(&format!("\n+{} more dirs\n", directories.len() - 20));
    }
    out
}

pub fn git_status(input: &str) -> String {
    if input.trim().is_empty() {
        return "Clean working tree".into();
    }
    let mut branch = "";
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicts = 0;
    for raw in input.split('\n') {
        if let Some(value) = raw.strip_prefix("On branch ") {
            branch = value.split_whitespace().next().unwrap_or("");
            continue;
        }
        if let Some(value) = raw.strip_prefix("##") {
            branch = value.trim_start();
            continue;
        }
        let bytes = raw.as_bytes();
        if bytes.len() >= 3
            && bytes[2] == b' '
            && b" MADRCU?!".contains(&bytes[0])
            && b" MADRCU?!".contains(&bytes[1])
        {
            let file = &raw[3..];
            if &bytes[..2] == b"??" {
                untracked.push(file);
                continue;
            }
            if b"MADRC".contains(&bytes[0]) {
                staged.push(file);
            } else if bytes[0] == b'U' {
                conflicts += 1;
            }
            if b"MD".contains(&bytes[1]) {
                modified.push(file);
            }
            continue;
        }
        if let Some((kind, path)) = raw.trim_start().split_once(':') {
            let path = path.trim();
            if path.is_empty() {
                continue;
            }
            match kind {
                "both modified" => conflicts += 1,
                "modified" | "deleted" => modified.push(path),
                "new file" | "renamed" => staged.push(path),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if !branch.is_empty() {
        out.push(format!("* {branch}"));
    }
    for (prefix, files) in [
        ("+ Staged", &staged),
        ("~ Modified", &modified),
        ("? Untracked", &untracked),
    ] {
        if !files.is_empty() {
            out.push(format!("{prefix}: {} files", files.len()));
            out.extend(files.iter().take(10).map(|f| format!("   {f}")));
            if files.len() > 10 {
                out.push(format!("   ... +{} more", files.len() - 10));
            }
        }
    }
    if conflicts > 0 {
        out.push(format!("conflicts: {conflicts} files"));
    }
    if staged.is_empty() && modified.is_empty() && untracked.is_empty() && conflicts == 0 {
        out.push("clean — nothing to commit".into());
    }
    out.join("\n")
}

pub fn tree(input: &str) -> String {
    let mut lines = Vec::new();
    for line in input.split('\n') {
        if (line.contains("director") && line.contains("file"))
            || (line.trim().is_empty() && lines.is_empty())
        {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.len() > 200 {
        return format!(
            "{}\n... +{} more lines",
            lines[..200].join("\n"),
            lines.len() - 200
        );
    }
    lines.join("\n")
}

fn truncate(input: &str, numbered: bool) -> String {
    let lines: Vec<_> = input.split('\n').collect();
    if lines.len() < 250 {
        return input.to_owned();
    }
    let suffix = if numbered { " (file continues)" } else { "" };
    let mut result = lines[..120].join("\n");
    result.push_str(&format!(
        "\n... +{} lines truncated{}\n",
        lines.len() - 180,
        suffix
    ));
    result.push_str(&lines[lines.len() - 60..].join("\n"));
    result
}

pub fn dedup_log(input: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut previous: Option<&str> = None;
    let mut count = 0usize;
    let mut blank_streak = 0usize;
    fn flush(out: &mut Vec<String>, previous: Option<&str>, count: usize) {
        if previous.is_some() && count > 1 {
            out.push(format!("  ... ({} duplicate lines)", count - 1));
        }
    }
    for line in input.split('\n') {
        if line.trim().is_empty() {
            if blank_streak < 1 {
                out.push(line.to_owned());
            }
            blank_streak += 1;
            flush(&mut out, previous, count);
            previous = None;
            count = 0;
            continue;
        }
        blank_streak = 0;
        if previous == Some(line) {
            count += 1;
            continue;
        }
        flush(&mut out, previous, count);
        out.push(line.to_owned());
        previous = Some(line);
        count = 1;
        if out.len() >= 2000 {
            out.push("... (truncated at 2000 lines)".to_owned());
            return out.join("\n");
        }
    }
    flush(&mut out, previous, count);
    out.join("\n")
}

pub fn build_output(input: &str) -> String {
    let lines: Vec<&str> = input.split('\n').collect();
    if lines.is_empty() {
        return input.to_owned();
    }
    let mut errors: Vec<&str> = Vec::new();
    let mut warnings: Vec<&str> = Vec::new();
    let mut deprecations: Vec<&str> = Vec::new();
    let mut summary: Vec<&str> = Vec::new();
    let mut compiling_count = 0usize;
    let mut downloading_count = 0usize;
    let mut in_cargo_error = false;

    for line in &lines {
        let trimmed = line.trim();
        if in_cargo_error {
            if trimmed.is_empty() {
                in_cargo_error = false;
                continue;
            }
            let ts = line.trim_start();
            if ts.starts_with("-->")
                || ts.starts_with('|')
                || ts.starts_with('=')
                || ts
                    .split_once('|')
                    .is_some_and(|(num, _)| !num.is_empty() && num.trim().chars().all(|c| c.is_ascii_digit()))
            {
                errors.push(line);
                continue;
            }
            in_cargo_error = false;
        }
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("npm err!") || lower.starts_with("npm error") || lower.starts_with("yarn error") {
            errors.push(line);
            continue;
        }
        if lower.starts_with("npm warn deprecated") {
            deprecations.push(line);
            continue;
        }
        if lower.starts_with("npm warn") || lower.starts_with("yarn warn") {
            warnings.push(line);
            continue;
        }
        if lower.starts_with("error[") || lower.starts_with("error:") || trimmed.starts_with("error -->") {
            if lower.starts_with("error:") && !lower.starts_with("error[") {
                errors.push(line);
                in_cargo_error = true;
            } else {
                errors.push(line);
                in_cargo_error = true;
            }
            continue;
        }
        if lower.starts_with("warning[") || lower.starts_with("warning:") || trimmed.starts_with("warning -->") {
            warnings.push(line);
            in_cargo_error = true;
            continue;
        }
        if lower.starts_with("[error]") || lower.starts_with("build failed") {
            errors.push(line);
            continue;
        }
        if lower.starts_with("[warning]") {
            warnings.push(line);
            continue;
        }
        if lower.starts_with("compiling ") {
            compiling_count += 1;
            continue;
        }
        if lower.starts_with("downloading ") || lower.starts_with("fetching ") {
            downloading_count += 1;
            continue;
        }
        if ["added ", "removed ", "changed ", "audited ", "installed "]
            .iter()
            .any(|p| lower.starts_with(p))
            && lower.contains("package")
            || lower.starts_with("finished ")
            || lower.starts_with("build success")
            || lower.starts_with("successfully installed")
            || lower.starts_with("successfully built")
            || lower.starts_with("to address ")
            || lower.starts_with("run `npm audit")
            || lower.starts_with("run `npm fund")
            || lower.contains("packages are looking for funding")
            || lower
                .split_whitespace()
                .next()
                .is_some_and(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
                && ["vulnerabilities", "package", "warning", "error"]
                    .iter()
                    .any(|kw| lower.contains(kw))
        {
            summary.push(line);
        }
    }

    let mut out = String::new();
    for d in deprecations.iter().take(3) {
        out.push_str(d);
        out.push('\n');
    }
    if deprecations.len() > 3 {
        out.push_str(&format!("... +{} more deprecated packages\n", deprecations.len() - 3));
    }
    if compiling_count > 0 {
        out.push_str(&format!("Compiled {compiling_count} packages\n"));
    }
    if downloading_count > 0 {
        out.push_str(&format!("Downloaded {downloading_count} packages\n"));
    }
    for e in errors {
        out.push_str(e);
        out.push('\n');
    }
    for w in warnings.iter().take(5) {
        out.push_str(w);
        out.push('\n');
    }
    if warnings.len() > 5 {
        out.push_str(&format!("... +{} more warnings\n", warnings.len() - 5));
    }
    if !summary.is_empty() {
        out.push_str(&summary.join("\n"));
        out.push('\n');
    }
    let trimmed_out = out.trim_end_matches('\n');
    if trimmed_out.is_empty() {
        input.to_owned()
    } else {
        trimmed_out.to_owned()
    }
}

fn is_hex_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn git_log(input: &str, max_lines: usize) -> String {
    if input.is_empty() {
        return String::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut in_commit = false;
    let mut subject_seen = false;

    let mut push_line = |l: String| {
        if out.len() < max_lines {
            out.push(l);
        } else {
            skipped += 1;
        }
    };

    for raw in input.split('\n') {
        let line = raw.trim_end();
        let trimmed = line.trim();
        let no_graph = trimmed.trim_start_matches(['*', '|', '/', '\\', ' ']);
        if let Some(rest) = no_graph.strip_prefix("commit ")
            && is_hex_sha(rest.split_whitespace().next().unwrap_or(""))
        {
            in_commit = true;
            subject_seen = false;
            push_line(line.to_owned());
            continue;
        }
        if in_commit {
            let lower = no_graph.to_ascii_lowercase();
            if lower.starts_with("author:") || lower.starts_with("date:") {
                push_line(trimmed.to_owned());
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            if !subject_seen && line.trim_start_matches(['*', '|', '/', '\\', ' ']).len() < line.len()
                && line.contains("    ")
            {
                push_line(format!("  Subject: {trimmed}"));
                subject_seen = true;
                continue;
            }
            if trimmed
                .split_whitespace()
                .next()
                .is_some_and(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
                && (trimmed.contains("file changed") || trimmed.contains("files changed"))
            {
                push_line(format!("  {trimmed}"));
                continue;
            }
            if trimmed.starts_with("diff --git ") {
                push_line("  ... diff body omitted".to_owned());
                continue;
            }
            continue;
        }

        if let Some((sha, rest)) = no_graph.split_once(char::is_whitespace)
            && is_hex_sha(sha)
            && !rest.trim().is_empty()
        {
            push_line(no_graph.to_owned());
            continue;
        }
        if !trimmed.is_empty()
            && trimmed.chars().all(|c| matches!(c, '*' | '|' | '/' | '\\' | ' '))
            && trimmed.chars().any(|c| matches!(c, '*' | '|' | '/' | '\\'))
        {
            continue;
        }
        push_line(trimmed.to_owned());
    }

    if skipped > 0 {
        out.push(format!("... ({skipped} more lines)"));
    }
    let result = out.join("\n");
    if result.is_empty() || result.len() > input.len() {
        input.to_owned()
    } else {
        result
    }
}

const LS_NOISE_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "__pycache__", ".next", "dist", "build", ".cache",
    ".turbo", ".vercel", ".pytest_cache", ".mypy_cache", ".tox", ".venv", "venv", "env",
    "coverage", ".nyc_output", ".DS_Store", "Thumbs.db", ".idea", ".vscode", ".vs",
    "*.egg-info", ".eggs",
];
const MONTHS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn human_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

fn parse_ls_line(line: &str) -> Option<(char, u64, &str)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 8 {
        return None;
    }
    let file_type = tokens[0].chars().next()?;
    let month_idx = tokens.iter().position(|t| MONTHS.contains(t))?;
    if month_idx < 4 || month_idx + 2 >= tokens.len() {
        return None;
    }
    let day = tokens[month_idx + 1];
    if !day.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let time_or_year = tokens[month_idx + 2];
    let valid_ty = (time_or_year.len() == 4 && time_or_year.chars().all(|c| c.is_ascii_digit()))
        || (time_or_year.len() == 5 && time_or_year.as_bytes()[2] == b':');
    if !valid_ty {
        return None;
    }
    let size = tokens[..month_idx]
        .iter()
        .rev()
        .find_map(|t| t.parse::<u64>().ok())
        .unwrap_or(0);
    let ty_pos = line.find(time_or_year)?;
    let name = line[ty_pos + time_or_year.len()..].trim_start();
    if name.is_empty() {
        return None;
    }
    Some((file_type, size, name))
}

pub fn ls(input: &str) -> String {
    let mut dirs: Vec<&str> = Vec::new();
    let mut files: Vec<(&str, String)> = Vec::new();
    let mut by_ext: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();

    for line in input.split('\n') {
        if line.starts_with("total ") || line.is_empty() {
            continue;
        }
        let Some((ftype, size, name)) = parse_ls_line(line) else {
            continue;
        };
        if name == "." || name == ".." || LS_NOISE_DIRS.contains(&name) {
            continue;
        }
        if ftype == 'd' {
            dirs.push(name);
        } else if ftype == '-' || ftype == 'l' {
            let ext = name
                .rfind('.')
                .filter(|&i| i > 0)
                .map_or("no ext", |i| &name[i..]);
            *by_ext.entry(ext).or_default() += 1;
            files.push((name, human_size(size)));
        }
    }

    if dirs.is_empty() && files.is_empty() {
        return input.to_owned();
    }

    let mut out = String::new();
    for d in &dirs {
        out.push_str(d);
        out.push_str("/\n");
    }
    for (name, size) in &files {
        out.push_str(&format!("{name}  {size}\n"));
    }
    out.push_str(&format!("\nSummary: {} files, {} dirs", files.len(), dirs.len()));
    if !by_ext.is_empty() {
        let mut ext: Vec<(&str, usize)> = by_ext.into_iter().collect();
        ext.sort_by(|a, b| b.1.cmp(&a.1));
        let parts: Vec<String> = ext.iter().take(5).map(|(e, c)| format!("{c} {e}")).collect();
        out.push_str(&format!(" ({}", parts.join(", ")));
        if ext.len() > 5 {
            out.push_str(&format!(", +{} more", ext.len() - 5));
        }
        out.push(')');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_apply_preserves_raw_text_on_panic() {
        fn fail(_: &str) -> String {
            panic!("filter failed")
        }
        assert_eq!(safe_apply(Some(fail), "raw"), "raw");
        assert_eq!(safe_apply(None, "raw"), "raw");
    }
    #[test]
    fn git_diff_summarizes_changes() {
        assert_eq!(
            git_diff("diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new", 500),
            "\na\n  @@ -1 +1 @@\n  -old\n  +new\n  +1 -1"
        );
    }
    #[test]
    fn search_list_groups_paths() {
        assert_eq!(
            search_list("Result of search in '.' (total 2 files):\n- src/a\n- src/b"),
            "Result of search in '.' (total 2 files):\n2 files in 1 dirs:\n\nsrc/ (2):\n  a\n  b"
        );
    }
    #[test]
    fn grep_groups_matches_and_preserves_colons() {
        assert_eq!(
            grep("b:2: x:y\na:1: hello"),
            "2 matches in 2F:\n\n[file] a (1):\n     1: hello\n\n[file] b (1):\n     2: x:y\n\n"
        );
        assert_eq!(grep("no matches"), "no matches");
    }
    #[test]
    fn find_groups_unix_and_windows_paths() {
        assert_eq!(
            find("src/a\nsrc/b\nC:\\foo\\c"),
            "3 files in 2 dirs:\n\nC:/foo/  (1)\n  c\nsrc/  (2)\n  a\n  b\n"
        );
    }
    #[test]
    fn git_status_formats_porcelain() {
        assert_eq!(
            git_status("## main\nM  a\n M b\n?? c\n"),
            "* main\n+ Staged: 1 files\n   a\n~ Modified: 1 files\n   b\n? Untracked: 1 files\n   c"
        );
        assert_eq!(git_status(""), "Clean working tree");
    }
    #[test]
    fn tree_removes_summary_and_blank_edges() {
        assert_eq!(tree("\n.\n├── a\n\n1 directory, 1 file\n"), ".\n├── a");
    }
    #[test]
    fn truncation_threshold_and_numbered_suffix() {
        let short = (0..249)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(smart_truncate(&short), short);
        let long = format!("{short}\n249");
        assert!(smart_truncate(&long).contains("... +70 lines truncated\n190"));
        assert!(read_numbered(&long).contains("... +70 lines truncated (file continues)"));
    }
    #[test]
    fn deduplicates_runs_and_blank_lines() {
        assert_eq!(
            dedup_log("a\na\na\n\n\nb\nb"),
            "a\n\n  ... (2 duplicate lines)\nb\n  ... (1 duplicate lines)"
        );
    }
}
