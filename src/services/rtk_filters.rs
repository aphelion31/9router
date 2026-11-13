pub fn smart_truncate(input: &str) -> String {
    truncate(input, false)
}

pub fn read_numbered(input: &str) -> String {
    truncate(input, true)
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

#[cfg(test)]
mod tests {
    use super::*;
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
