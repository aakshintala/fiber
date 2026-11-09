use std::process::Command;

use grep_matcher::Matcher as _;
use grep_regex::RegexMatcherBuilder;

const PHRASES: &str =
    r"\b(?:changed[ \t]+from|previously|now[ \t]+uses|fixed[ \t]+the[ \t]+bug[ \t]+where)\b";

pub(crate) fn narrating_comments(base: &str) -> Result<Vec<String>, String> {
    let merge_base = git(&["merge-base", base, "HEAD"])?;
    let diff = git(&[
        "-c",
        "core.quotePath=false",
        "diff",
        "--no-ext-diff",
        "--no-color",
        "--find-renames",
        "--unified=0",
        &merge_base,
    ])?;
    let mut failures = check_diff(&diff)?;

    // Untracked files are absent from Git's diff, so represent each as added lines.
    for path in git_bytes(&["ls-files", "--others", "--exclude-standard", "-z"])?
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|e| format!("git path is not UTF-8: {e}"))?;
        if !is_source(path) {
            continue;
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8(bytes).map_err(|e| format!("{path}: {e}"))?;
        let mut patch = format!(
            "diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
            text.lines().count()
        );
        for line in text.lines() {
            patch.push('+');
            patch.push_str(line);
            patch.push('\n');
        }
        failures.extend(check_diff(&patch)?);
    }
    Ok(failures)
}

fn is_source(path: &str) -> bool {
    path.starts_with("scripts/")
        || [".rs", ".sh", ".py", ".toml", ".yaml", ".yml"]
            .iter()
            .any(|suffix| path.ends_with(suffix))
}

fn git(args: &[&str]) -> Result<String, String> {
    let bytes = git_bytes(args)?;
    String::from_utf8(bytes)
        .map(|text| text.trim_end().to_owned())
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
}

fn git_bytes(args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn check_diff(diff: &str) -> Result<Vec<String>, String> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(true)
        .build(PHRASES)
        .map_err(|e| format!("narrating-comments matcher: {e}"))?;
    let mut failures = Vec::new();
    let mut path = "";
    let mut new_line = 0usize;
    let mut in_hunk = false;

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            path = "";
            in_hunk = false;
        } else if line.starts_with("@@") {
            new_line = line
                .split_whitespace()
                .nth(2)
                .expect("git emits a new-file hunk range")
                .strip_prefix('+')
                .expect("git emits a new-file hunk range")
                .split(',')
                .next()
                .expect("git emits a new-file hunk start")
                .parse()
                .expect("git emits a numeric new-file hunk start");
            in_hunk = true;
        } else if in_hunk {
            if let Some(added) = line.strip_prefix('+') {
                if is_source(path)
                    && matcher
                        .is_match(comment(path, added).as_bytes())
                        .map_err(|e| format!("{path}:{new_line}: {e}"))?
                {
                    failures.push(format!("{path}:{new_line}: comment narrates history"));
                }
                new_line += 1;
            }
        } else if let Some(header) = line.strip_prefix("+++ b/") {
            path = header;
        }
    }
    Ok(failures)
}

fn comment<'a>(path: &str, line: &'a str) -> &'a str {
    if path.ends_with(".rs") {
        rust_comment(line)
    } else {
        shell_comment(line)
    }
}

fn shell_comment(line: &str) -> &str {
    line.char_indices()
        .find(|(position, ch)| {
            *ch == '#'
                && (*position == 0
                    || line[..*position]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
        })
        .and_then(|(position, _)| line[position..].strip_prefix('#'))
        .unwrap_or("")
}

fn rust_comment(line: &str) -> &str {
    if let Some(position) = line.find("//") {
        &line[position + 2..]
    } else if line.trim_start().starts_with("/*") || line.trim_start().starts_with('*') {
        line
    } else {
        ""
    }
}
