use super::*;

const CODE_QUALITY: &str = "# Code quality

## `unsafe`

 An indented line is not a heading.

| Crate | File | Why |
|---|---|---|
| none yet | | |

### A subsection

## Types

| Crate | File | Why |
|---|---|---|
| `log` | `crates/log/src/lib.rs` | not the unsafe table |
";

fn listed() -> String {
    CODE_QUALITY.replacen(
        "| none yet | | |",
        "| `tools` | `crates/tools/src/pty.rs` | pre_exec |",
        1,
    )
}

const DEPENDENCIES: &str = "# Dependencies

## Runtime dependencies

| Crate | Used for |
|---|---:|
| serde, serde_json | wire formats |
| `ureq` | HTTP |
| all of the above together | |

## Waiting on other decisions

| Crate | Needed if |
|---|---|
| rusqlite, SQLite bundled | a database |

## Tests and development tools

| Crate or tool | Kind | Used for |
|---|---|---|
| insta | dev-dependency | snapshots |
";

fn signal_line(pattern: &str, needs_kill: bool) -> String {
    let body = if needs_kill {
        format!("run kill {pattern}1")
    } else {
        format!("call {pattern} now")
    };
    format!("clean\n{body}")
}

#[test]
fn signal_sites_reports_each_pattern_outside_the_allowlist() {
    for &(pattern, needs_kill) in SIGNAL_PATTERNS {
        let files = [file("log", "src/lib.rs", signal_line(pattern, needs_kill))];
        let hits = signal_sites(&files);
        assert!(
            hits.contains(&format!("crates/log/src/lib.rs:2: {pattern}")),
            "{pattern}: {hits:?}"
        );
        assert!(
            hits.iter()
                .all(|hit| hit.starts_with("crates/log/src/lib.rs:2: ")),
            "{pattern}: {hits:?}"
        );
    }
}

#[test]
fn signal_sites_ignores_each_pattern_inside_the_allowlist() {
    for &(pattern, needs_kill) in SIGNAL_PATTERNS {
        let line = signal_line(pattern, needs_kill);
        for (krate, rel) in [
            ("fakes", "src/process_group.rs"),
            ("mcp", "src/registry.rs"),
            ("tools", "src/shell/process_group.rs"),
        ] {
            let files = [file(krate, rel, line.clone())];
            assert_eq!(signal_sites(&files), Vec::<String>::new(), "{pattern}");
        }
    }
}

#[test]
fn signal_sites_ignores_dash_operands_without_kill() {
    let source = [
        "names.push(\"find x -- -delete\")",
        "effects(&shell, \"npm test -- --watch\")",
        "classified(\"ls -- -l\")",
        "\"cargo clippy -- -D warnings\"",
    ]
    .join("\n");
    let files = [file("tools", "src/classify.rs", source)];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_the_word_kill_on_its_own() {
    let files = [file(
        "log",
        "src/lib.rs",
        "the watchdog kills the group".to_owned(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_reports_a_shell_kill_split_across_lines() {
    // Built from parts so this file holds no signal pattern itself: the
    // first line ends with a backslash, the second carries the operand.
    let first = ["kill -TERM ", "\\"].concat();
    let second = ["  --", " -1\""].concat();
    let source = format!("{first}\n{second}");
    let files = [file("log", "src/lib.rs", source)];
    let dash = ["--", " -"].concat();
    assert_eq!(
        signal_sites(&files),
        [format!("crates/log/src/lib.rs:2: {dash}")]
    );
}

#[test]
fn signal_sites_ignores_a_kill_the_fragment_does_not_continue() {
    // A kill two lines up, with an unbroken line between, is another
    // command: the operand's line is not joined to it.
    let dash = ["--", " -"].concat();
    let source = format!("kill -TERM 5\nlet x = 1;\nrun {dash}l");
    let files = [file("log", "src/lib.rs", source)];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_follows_a_chain_of_continued_lines() {
    let first = ["kill -TERM ", "\\"].concat();
    let middle = ["  -s KILL ", "\\"].concat();
    let last = ["  --", " -1"].concat();
    let source = format!("{first}\n{middle}\n{last}");
    let files = [file("log", "src/lib.rs", source)];
    let dash = ["--", " -"].concat();
    assert_eq!(
        signal_sites(&files),
        [format!("crates/log/src/lib.rs:3: {dash}")]
    );
}

fn file(krate: &str, rel: &str, source: String) -> RustFile {
    let dir = if krate == "xtask" {
        "xtask".to_owned()
    } else {
        format!("crates/{krate}")
    };
    RustFile {
        krate: krate.to_owned(),
        path: format!("{dir}/{rel}"),
        rel: rel.to_owned(),
        source,
    }
}

#[test]
fn the_line_cap_lists_a_source_file_over_800_lines() {
    let files = [
        file("log", "src/big.rs", "x\n".repeat(801)),
        file("log", "src/ok.rs", "x\n".repeat(800)),
    ];
    assert_eq!(
        over_cap(&files),
        ["crates/log/src/big.rs: 801 lines, over 800; file a split ticket"]
    );
}

#[test]
fn test_files_have_no_line_cap() {
    let files = [
        file("log", "src/tests.rs", "x\n".repeat(5000)),
        file("log", "src/a_tests.rs", "x\n".repeat(5000)),
        file("log", "tests/t.rs", "x\n".repeat(5000)),
    ];
    assert_eq!(over_cap(&files), Vec::<String>::new());
}

#[test]
fn unsafe_is_found_in_code() {
    assert!(uses_unsafe("fn f() { unsafe { g() } }").unwrap());
    assert!(uses_unsafe("unsafe fn f() {}").unwrap());
    assert!(uses_unsafe("let s = \"a\\\"b\"; unsafe {}").unwrap());
    assert!(uses_unsafe("let c = '\\''; let d = 'x'; unsafe {}").unwrap());
    assert!(uses_unsafe("fn f<'a>(x: &'a str) { unsafe {} }").unwrap());
    assert!(uses_unsafe("/* a */ unsafe {}").unwrap());
    assert!(uses_unsafe("let s = r#\"x\"#; unsafe {}").unwrap());
}

#[test]
fn unsafe_in_comments_strings_and_names_is_not_code() {
    assert!(
        !uses_unsafe("#![deny(unsafe_code)]\n// unsafe here\n/* unsafe\n */ fn f() {}").unwrap()
    );
    assert!(!uses_unsafe("let s = \"unsafe\"; let t = \"\\\" unsafe\";").unwrap());
    assert!(!uses_unsafe("let s = r#\"unsafe \"quoted\" \"#;").unwrap());
    assert!(!uses_unsafe("let s = r\"unsafe\";").unwrap());
    assert!(!uses_unsafe("fn not_unsafe() {} fn unsafely() {}").unwrap());
    assert!(!uses_unsafe("// unsafe at the end").unwrap());
}

#[test]
fn the_unsafe_check_passes_when_code_and_table_agree() {
    let clean = [file("log", "src/lib.rs", "fn f() {}".to_owned())];
    assert_eq!(unsafe_mismatches(&clean, CODE_QUALITY), Ok(vec![]));
    let pty = [file("tools", "src/pty.rs", "unsafe { x() }".to_owned())];
    assert_eq!(unsafe_mismatches(&pty, &listed()), Ok(vec![]));
}

#[test]
fn the_unsafe_check_fails_unsafe_the_table_does_not_list() {
    let files = [file("log", "src/lib.rs", "unsafe { x() }".to_owned())];
    assert_eq!(
        unsafe_mismatches(&files, CODE_QUALITY),
        Ok(vec!["crates/log/src/lib.rs: uses unsafe, but the table in docs/code-quality.md does not list it".to_owned()])
    );
}

#[test]
fn the_unsafe_check_fails_a_listed_file_without_unsafe() {
    let files = [file("tools", "src/pty.rs", "fn f() {}".to_owned())];
    assert_eq!(
        unsafe_mismatches(&files, &listed()),
        Ok(vec![
            "crates/tools/src/pty.rs: listed for tools in docs/code-quality.md, but uses no unsafe"
                .to_owned()
        ])
    );
}

#[test]
fn the_unsafe_check_needs_its_table() {
    assert!(unsafe_mismatches(&[], "# Code quality\n").is_err());
}

#[test]
fn a_section_ends_at_the_next_heading_of_its_level() {
    let lines = section(CODE_QUALITY, "`unsafe`").unwrap();
    assert_eq!(lines.first(), Some(&""));
    assert!(lines.contains(&"### A subsection"));
    assert!(!lines.contains(&"## Types"));
    assert_eq!(section(CODE_QUALITY, "Missing"), None);
    assert_eq!(section("#Types\n", "Types"), None);
}

#[test]
fn only_the_admitted_tables_list_crates() {
    let names: Vec<String> = admitted(DEPENDENCIES).unwrap().into_iter().collect();
    assert_eq!(names, ["insta", "serde", "serde_json", "ureq"]);
    assert!(admitted("# Dependencies\n").is_err());
}

#[test]
fn a_dependency_that_is_not_listed_fails() {
    let deps: BTreeSet<(String, String)> =
        [("contract", "serde"), ("log", "insta"), ("log", "rusqlite")]
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect();
    assert_eq!(
        unlisted(&deps, DEPENDENCIES),
        Ok(vec![
            "log depends on rusqlite, which docs/dependencies.md does not list".to_owned()
        ])
    );
}

/// Each Rust literal and comment form, holding quotes, backslashes and the
/// word itself, so a scanner that misreads its end sees the wrong code.
const FORMS: [&str; 17] = [
    r#""a\"b unsafe \\""#,
    r##"r#"a"b unsafe"#"##,
    r###"r##"a"#b"##"###,
    r#"r"a\""#,
    r#"b"a\"b unsafe""#,
    r##"br#"a"b unsafe"#"##,
    r#"br"a\""#,
    r#"c"a\"b unsafe""#,
    r##"cr#"a"b unsafe"#"##,
    r"'\''",
    r"'\u{22}'",
    "'\"'",
    r"b'\''",
    "b'\"'",
    "/* a \" /* b ' */ \" unsafe */",
    "/* \" */",
    "// \" unsafe\n",
];

#[test]
fn unsafe_after_every_literal_form_is_found() {
    for form in FORMS {
        assert!(
            uses_unsafe(&format!("let x = {form}; unsafe {{}}")).unwrap(),
            "{form}"
        );
    }
}

#[test]
fn unsafe_inside_every_literal_form_is_not_code() {
    for form in FORMS {
        assert!(!uses_unsafe(&format!("let x = {form};")).unwrap(), "{form}");
    }
}

#[test]
fn lifetimes_labels_and_raw_identifiers_are_code() {
    let unsafe_after = |code: &str| uses_unsafe(&format!("{code} unsafe {{}}")).unwrap();
    assert!(unsafe_after("fn f<'a>(x: &'a str) -> &'a str { x }"));
    assert!(unsafe_after("fn f() { 'outer: loop { break 'outer; } }"));
    assert!(unsafe_after("let r#type = 1; let b = br; let c = cr;"));
    assert!(!uses_unsafe("let r#unsafe = 1;").unwrap());
}

#[test]
fn a_string_right_after_an_identifier_hides_nothing() {
    assert!(uses_unsafe(r#"fn f() { stringify!(b""""); unsafe {} }"#).unwrap());
    assert!(uses_unsafe(r#"fn f() { m!(b"" ""); unsafe {} }"#).unwrap());
}

#[test]
fn a_file_that_does_not_tokenise_fails_the_check_by_name() {
    for source in [
        "\"unterminated unsafe",
        "/* never closed unsafe",
        "fn f() { unsafe {}",
        "r\"open",
    ] {
        assert!(uses_unsafe(source).is_err(), "{source:?}");
    }
    let files = [file("log", "src/lib.rs", "fn f() {".to_owned())];
    let error = unsafe_mismatches(&files, CODE_QUALITY).unwrap_err();
    assert!(
        error.starts_with("crates/log/src/lib.rs: does not tokenise as Rust: "),
        "{error}"
    );
}

fn tree(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("{name} v1.0.0 (/repo/{name}) (*)\n"))
        .collect()
}

#[test]
fn a_member_whose_tree_names_an_image_crate_is_reported_once_per_crate() {
    let trees = vec![
        (
            "tools".to_owned(),
            tree(&["tools", "serde", "image", "image"]),
        ),
        (
            "provider".to_owned(),
            tree(&["provider", "fast_image_resize"]),
        ),
    ];
    assert_eq!(
        leaks(&trees, &IMAGE),
        [
            "provider: its normal dependency tree holds fast_image_resize; only the image child links image code",
            "tools: its normal dependency tree holds image; only the image child links image code",
        ]
    );
}

#[test]
fn a_clean_tree_passes_and_picture_and_main_are_exempt() {
    let trees = vec![
        (
            "tools".to_owned(),
            tree(&["tools", "serde", "image_lookalike", "imagery"]),
        ),
        (
            "picture".to_owned(),
            tree(&["picture", "image", "fast_image_resize"]),
        ),
        ("main".to_owned(), tree(&["main", "picture", "image"])),
    ];
    assert!(leaks(&trees, &IMAGE).is_empty());
}

#[test]
fn only_the_crate_name_at_the_start_of_a_line_counts() {
    let trees = vec![(
        "tools".to_owned(),
        "tools v0.0.0 (/path/image)\nserde v1 image\n".to_owned(),
    )];
    assert!(leaks(&trees, &IMAGE).is_empty());
}

#[test]
fn a_member_whose_tree_names_a_tui_crate_is_reported_and_tui_and_main_are_exempt() {
    let trees = vec![
        (
            "hub".to_owned(),
            tree(&["hub", "crossterm", "ratatui_lookalike"]),
        ),
        ("tui".to_owned(), tree(&["tui", "ratatui", "crossterm"])),
        ("main".to_owned(), tree(&["main", "tui", "ratatui"])),
    ];
    assert_eq!(
        leaks(&trees, &TUI),
        [
            "hub: its normal dependency tree holds crossterm; only the terminal links terminal UI code"
        ]
    );
}
