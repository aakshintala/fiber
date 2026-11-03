use super::*;
use fakes::TempDir;

const DOC: &str = "docs/target.md";
const GONE: &str = "docs/gone.md";
const RESEARCH: &str = "research/notes.md";
const TARGET: &str = "intro\nmore intro\n## The gate\ntext\ntext\ntext\ntext\ntext\n## `unsafe`\ntext\ntext\ntext\ntext\ntext\n```\n## Not a heading\n```\ntext\ntext\ntext\n";
const RESEARCH_MD: &str = "# Notes\n\n## Findings\n";

fn run(files: &[(&str, &str)], allow: Option<&str>) -> Vec<String> {
    let dir = TempDir::new("xtask-doc-citations");
    dir.write("docs/target.md", TARGET);
    for (path, content) in files {
        dir.write(path, content);
    }
    if let Some(table) = allow {
        dir.write(ALLOW, table);
    }
    check(dir.path()).unwrap()
}

#[test]
fn a_line_cite_names_the_heading_above_its_line() {
    let q = '"';
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// one\n// two\n// {DOC}:4\n")
            )],
            None
        ),
        [format!(
            "crates/a/src/x.rs:3: unlisted cite {q}{DOC}:4{q}: use {q}{DOC}, {q}The gate{q}{q}"
        )]
    );
    assert_eq!(
        run(&[("crates/a/src/x.rs", &format!("// {DOC}:3\n"))], None),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{DOC}:3{q}: use {q}{DOC}, {q}The gate{q}{q}"
        )]
    );
    assert_eq!(
        run(&[("crates/a/src/x.rs", &format!("// {DOC}:2\n"))], None),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{DOC}:2{q}: no heading of {DOC} at or above that line: delete the citation"
        )]
    );
    assert_eq!(
        run(&[("crates/a/src/x.rs", &format!("// {DOC}:9\n"))], None),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{DOC}:9{q}: use {q}{DOC}, {q}`unsafe`{q}{q}"
        )]
    );
    assert_eq!(
        run(&[("crates/a/src/x.rs", &format!("// {DOC}:99\n"))], None),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{DOC}:99{q}: use {q}{DOC}, {q}`unsafe`{q}{q}"
        )]
    );
    assert_eq!(
        run(&[("crates/a/src/x.rs", &format!("// {GONE}:3\n"))], None),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{GONE}:3{q}: no such file"
        )]
    );
}

#[test]
fn the_three_spellings_and_the_span_are_reported_as_written() {
    let q = '"';
    let unsafe_use = format!("use {q}{DOC}, {q}`unsafe`{q}{q}");
    let gate_use = format!("use {q}{DOC}, {q}The gate{q}{q}");
    let fallback = format!("no heading of {DOC} at or above that line: delete the citation");
    let tails: &[(&str, &str, String)] = &[
        (":12", ":12", unsafe_use.clone()),
        (":12-14", ":12-14", unsafe_use.clone()),
        (":74,77,227-229", ":74,77,227-229", unsafe_use.clone()),
        (":1, 2", ":1, 2", fallback.clone()),
        (":12:", ":12", unsafe_use.clone()),
        (":12 and", ":12", unsafe_use.clone()),
        (":12-", ":12", unsafe_use.clone()),
        ("`:12", "`:12", unsafe_use.clone()),
        ("` line 274", "` line 274", unsafe_use.clone()),
        (" line 7", " line 7", gate_use.clone()),
    ];
    for (tail, key_tail, reason) in tails {
        let content = format!("// {DOC}{tail}\n");
        let failures = run(&[("crates/a/src/x.rs", &content)], None);
        assert_eq!(
            failures,
            [format!(
                "crates/a/src/x.rs:1: unlisted cite {q}{DOC}{key_tail}{q}: {reason}"
            )],
            "tail {tail}"
        );
    }
    let quiet: &[&str] = &[": text", ":x", " line x", " lines 7", "#L12"];
    for tail in quiet {
        let content = format!("// {DOC}{tail}\n");
        assert_eq!(
            run(&[("crates/a/src/x.rs", &content)], None),
            Vec::<String>::new(),
            "tail {tail}"
        );
    }
    for placeholder in ["docs/<area>.md", "docs/*.md"] {
        let content = format!("// {placeholder}:3\n");
        assert_eq!(
            run(&[("crates/a/src/x.rs", &content)], None),
            Vec::<String>::new(),
            "placeholder {placeholder}"
        );
    }
    let dir = TempDir::new("xtask-doc-citations-research");
    dir.write("docs/target.md", TARGET);
    dir.write(RESEARCH, RESEARCH_MD);
    dir.write("crates/a/src/x.rs", &format!("// {RESEARCH}:3\n"));
    assert_eq!(
        check(dir.path()).unwrap(),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{RESEARCH}:3{q}: use {q}{RESEARCH}, {q}Findings{q}{q}"
        )]
    );
}

#[test]
fn two_cites_on_one_line_each() {
    let failures = run(
        &[("crates/a/src/x.rs", &format!("// {DOC}:4 {DOC}:9\n"))],
        None,
    );
    assert_eq!(failures.len(), 2);
    assert!(failures[0].contains(":1:"));
    assert!(failures[0].contains(&format!("{DOC}:4")));
    assert!(failures[1].contains(&format!("{DOC}:9")));
    for failure in &failures {
        assert!(failure.contains("unlisted"), "{failure}");
    }
}

#[test]
fn a_cite_in_a_string_literal_is_scanned() {
    let q = '"';
    let failures = run(
        &[("crates/a/src/x.rs", &format!("let s = {q}{DOC}:12{q};\n"))],
        None,
    );
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("unlisted"), "{}", failures[0]);
}

#[test]
fn a_dangling_heading_fails_and_a_resolving_one_passes() {
    let q = '"';
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// ({DOC}, {q}Not a heading{q})\n"),
            )],
            None
        ),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{DOC}, {q}Not a heading{q}{q}: no such heading"
        )]
    );
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// ({DOC}, {q}The gate{q})\n"),
            )],
            None
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// `{DOC}`, {q}The gate{q}\n"),
            )],
            None
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// ({DOC}, {q}`unsafe`{q})\n"),
            )],
            None
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// ({DOC}, {q}the gate{q})\n"),
            )],
            None
        ),
        [format!(
            "crates/a/src/x.rs:1: unlisted cite {q}{DOC}, {q}the gate{q}{q}: no such heading"
        )]
    );
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// ({GONE}, {q}A{q})\n// ({GONE}, {q}B{q})\n"),
            )],
            None
        ),
        [
            format!("crates/a/src/x.rs:1: unlisted cite {q}{GONE}, {q}A{q}{q}: no such file"),
            format!("crates/a/src/x.rs:2: unlisted cite {q}{GONE}, {q}B{q}{q}: no such file"),
        ]
    );
    assert_eq!(
        run(
            &[("crates/a/src/x.rs", "// see docs/target.md here\n")],
            None
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        run(
            &[(
                "crates/a/src/x.rs",
                &format!("// ({DOC}, \\{q}The gate\\{q})\n"),
            )],
            None
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_cite_broken_over_comment_lines_resolves() {
    let q = '"';
    for marker in ["//", "///", "//!"] {
        let content = format!("{marker} {DOC},\n{marker} {q}The gate{q}\n");
        assert_eq!(
            run(&[("crates/a/src/x.rs", &content)], None),
            Vec::<String>::new(),
            "marker {marker}"
        );
        let missing = format!("{marker} {DOC},\n{marker} {q}Missing{q}\n");
        let failures = run(&[("crates/a/src/x.rs", &missing)], None);
        assert_eq!(failures.len(), 1, "marker {marker}");
        assert!(
            failures[0].starts_with("crates/a/src/x.rs:1:"),
            "{}",
            failures[0]
        );
        assert!(failures[0].contains("unlisted"), "{}", failures[0]);
    }
    let content = format!("# {DOC},\n# {q}The gate{q}\n");
    assert_eq!(
        run(&[("scripts/run.sh", &content)], None),
        Vec::<String>::new()
    );
    let missing = format!("# {DOC},\n# {q}Missing{q}\n");
    let failures = run(&[("scripts/run.sh", &missing)], None);
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].starts_with("scripts/run.sh:1:"),
        "{}",
        failures[0]
    );
}

#[test]
fn any_text_file_under_the_three_roots_is_scanned() {
    let scanned = [
        "crates/a/tests/fixtures/data.jsonl",
        "crates/a/README.md",
        "xtask/x.json",
        "scripts/tool",
        "scripts/lib/z.sh",
        "crates/a/src/target/x.rs",
        "crates/a/target/y.rs",
        "crates/a/.git/z",
    ];
    for path in scanned {
        let failures = run(&[(path, &format!("// {DOC}:4\n"))], None);
        assert_eq!(failures.len(), 1, "{path}");
        assert!(
            failures[0].starts_with(&format!("{path}:1:")),
            "{}",
            failures[0]
        );
    }
    let unscanned = ["docs/x.md", "README.md", ".github/x.yml"];
    for path in unscanned {
        assert_eq!(
            run(&[(path, &format!("// {DOC}:4\n"))], None),
            Vec::<String>::new(),
            "{path}"
        );
    }
    let dir = TempDir::new("xtask-doc-citations-bytes");
    dir.write("docs/target.md", TARGET);
    std::fs::create_dir_all(dir.path().join("crates/a")).unwrap();
    let mut bytes = format!("// {DOC}:4\n").into_bytes();
    bytes.push(0xff);
    bytes.push(b'\n');
    std::fs::write(dir.path().join("crates/a/bin.dat"), bytes).unwrap();
    assert_eq!(check(dir.path()).unwrap(), Vec::<String>::new());
    assert_eq!(
        run(&[("crates/a/src/x.rs", &format!("// {DOC}:4\n"))], None).len(),
        1
    );
}

#[test]
fn only_the_table_itself_is_exempt() {
    let content = format!("// {DOC}:4\n");
    let failures = run(
        &[
            ("xtask/doc-citations.allow", content.as_str()),
            ("xtask/src/docs_tests.rs", content.as_str()),
            ("xtask/doc-citations.allow.bak", content.as_str()),
        ],
        None,
    );
    assert_eq!(failures.len(), 2);
    assert!(
        failures[0].starts_with("xtask/doc-citations.allow.bak:1:"),
        "{}",
        failures[0]
    );
    assert!(
        failures[1].starts_with("xtask/src/docs_tests.rs:1:"),
        "{}",
        failures[1]
    );
}
