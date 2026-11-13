pub fn smart_truncate(input: &str) -> String {
    truncate(input, false)
}

pub fn read_numbered(input: &str) -> String {
    truncate(input, true)
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
