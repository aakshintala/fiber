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
            ("cli", "src/extension_test.rs"),
            ("tui", "src/paste_image.rs"),
            ("support", "src/group.rs"),
        ] {
            let files = [file(krate, rel, line.clone())];
            assert_eq!(signal_sites(&files), Vec::<String>::new(), "{pattern}");
        }
    }
}

#[test]
fn signal_sites_reports_a_signal_elsewhere_in_tui() {
    for &(pattern, needs_kill) in SIGNAL_PATTERNS {
        let files = [file("tui", "src/app.rs", signal_line(pattern, needs_kill))];
        let hits = signal_sites(&files);
        assert!(
            hits.contains(&format!("crates/tui/src/app.rs:2: {pattern}")),
            "{pattern}: {hits:?}"
        );
        assert!(
            hits.iter()
                .all(|hit| hit.starts_with("crates/tui/src/app.rs:2: ")),
            "{pattern}: {hits:?}"
        );
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
        ["crates/log/src/big.rs: 801 lines, over 800"]
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
        ("tools".to_owned(), tree(&["tools", "lopdf"])),
    ];
    assert_eq!(
        leaks(&trees, &IMAGE),
        [
            "provider: its normal dependency tree holds fast_image_resize; only the image child links image code",
            "tools: its normal dependency tree holds image; only the image child links image code",
            "tools: its normal dependency tree holds lopdf; only the image child links image code",
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
            tree(&["picture", "image", "fast_image_resize", "lopdf"]),
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

/// A `--prefix depth` tree where `tools` and `main` enable
/// `preserve_order`: the crates in the `default` and `std` subtrees, and
/// the deeper crate under `tools`, enabled nothing.
const PRESERVE_VIOLATION: &str = "0serde_json v1.0.151\n\
1serde_json feature \"default\"\n\
2cli v0.0.0 (/repo/crates/cli)\n\
3cli feature \"default\" (command-line)\n\
1serde_json feature \"preserve_order\"\n\
2tools v0.0.0 (/repo/crates/tools)\n\
2\n\
3tools feature \"default\" (command-line)\n\
3hub v0.0.0 (/repo/crates/hub)\n\
2main v0.0.0 (/repo/crates/main) (*)\n\
2tools v0.0.0 (/repo/crates/tools) (*)\n\
1serde_json feature \"std\"\n\
2serde_json feature \"default\" (*)\n\
2config v0.0.0 (/repo/crates/config)\n\
1serde_json feature \"preserve_order\" (*)\n";

#[test]
fn preserve_order_reports_each_enabling_crate_once() {
    assert_eq!(
        preserve_order(PRESERVE_VIOLATION),
        [
            "main: enables serde_json feature \"preserve_order\"; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)",
            "tools: enables serde_json feature \"preserve_order\"; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)",
        ]
    );
}

/// The workspace tree on main today: only `default` and `std`, so no
/// violation. `preserve_order_helper` is a crate name, not the feature.
const PRESERVE_CLEAN: &str = "0serde_json v1.0.151\n\
1serde_json feature \"default\"\n\
2cli v0.0.0 (/repo/crates/cli)\n\
3cli feature \"default\" (command-line)\n\
2preserve_order_helper v1.2.3 (/repo/other)\n\
1serde_json feature \"std\"\n\
2config v0.0.0 (/repo/crates/config)\n";

#[test]
fn preserve_order_passes_without_the_feature() {
    assert!(preserve_order(PRESERVE_CLEAN).is_empty());
    assert!(preserve_order("").is_empty());
}

#[test]
fn preserve_order_ignores_a_longer_feature_name() {
    let tree = "0serde_json v1.0.151\n1serde_json feature \"preserve_order_extra\"\n2tools v0.0.0 (/repo/crates/tools)\n";
    assert!(preserve_order(tree).is_empty());
}

#[test]
fn preserve_order_names_a_nested_enabler_and_ends_at_the_sibling() {
    let tree = "0serde_json v1.0.151\n\
1serde_json feature \"default\"\n\
2cli v0.0.0 (/repo/crates/cli)\n\
3serde_json feature \"preserve_order\"\n\
4tools v0.0.0 (/repo/crates/tools)\n\
2config v0.0.0 (/repo/crates/config)\n";
    assert_eq!(
        preserve_order(tree),
        [
            "tools: enables serde_json feature \"preserve_order\"; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)"
        ]
    );
}

#[test]
fn preserve_order_names_a_crate_that_forwards_through_its_own_feature() {
    let tree = "0serde_json v1.0.151\n\
1serde_json feature \"preserve_order\"\n\
2log feature \"ordered\" (command-line)\n\
1serde_json feature \"std\"\n\
2config v0.0.0 (/repo/crates/config)\n";
    assert_eq!(
        preserve_order(tree),
        [
            "log: enables serde_json feature \"preserve_order\"; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)"
        ]
    );
}

#[test]
fn preserve_order_without_a_named_enabler_still_fails() {
    let tree = "0serde_json v1.0.151\n1serde_json feature \"preserve_order\"\n1serde_json feature \"std\"\n";
    assert_eq!(
        preserve_order(tree),
        [
            "serde_json feature \"preserve_order\" is enabled but no crate in the tree names it; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)"
        ]
    );
}

const REGISTERED_TOOLS: [&str; 14] = [
    "ask_user",
    "delegate_models",
    "delegate_spawn",
    "edit",
    "handoff",
    "jobs",
    "name_session",
    "read",
    "session_search",
    "shell",
    "skill",
    "web_fetch",
    "web_search",
    "write",
];

fn registrations(names: &[&str]) -> RustFile {
    let source = names
        .iter()
        .map(|name| format!("Tool {{ name: {name:?}.to_owned() }}"))
        .collect::<Vec<_>>()
        .join("\n");
    file("tools", "src/definitions.rs", source)
}

fn source_file(krate: &str, rel: &str, source: &str) -> RustFile {
    file(krate, rel, source.to_owned())
}

#[test]
fn session_paths_only_rejects_construction_outside_log() {
    assert!(session_paths(&[]).is_empty());
    let files = [
        file(
            "log",
            "src/lib.rs",
            "let dir = home.join(\"sessions\");".to_owned(),
        ),
        file(
            "config",
            "src/lib.rs",
            "let dir = home.join(\"sessions_extra\");".to_owned(),
        ),
        file(
            "hub",
            "src/lib.rs",
            "fn clean() { let s = \"sessions in prose\"; }".to_owned(),
        ),
    ];
    assert_eq!(session_paths(&files), Vec::<String>::new());
}

#[test]
fn session_paths_match_segments_joined_paths_and_raw_literals() {
    let files = [
        source_file(
            "hub",
            "src/new.rs",
            "fn first() {\n    let dir = project\n        .join(\n            r#\"sessions\"#\n        );\n}\nfn second() {\n    let path = std::fs::read(\"projects/key/sessions/id/events.jsonl\");\n}\n",
        ),
        source_file(
            "main",
            "src/other.rs",
            "fn third() {\n    let path = dir.join(\"events.jsonl\");\n}\n",
        ),
    ];
    assert_eq!(
        session_paths(&files),
        [
            "crates/hub/src/new.rs:4: session path outside log",
            "crates/hub/src/new.rs:8: session path outside log",
            "crates/main/src/other.rs:2: session path outside log",
        ]
    );
}

#[test]
fn session_paths_ignore_comments_descriptive_strings_and_segment_lookalikes() {
    let files = [source_file(
        "hub",
        "src/clean.rs",
        "// fs::read(\"sessions/old/events.jsonl\")\n/* .join(\"sessions\") */\nfn clean() {\n    let text = r#\"sessions/ is documentation\"#;\n    let description = \"sessions/story\"; let _ = read(\"notes.txt\");\n    let a = dir.join(\"sessions_extra\");\n    let b = dir.join(\"my_sessions\");\n    let c = dir.join(\"events.jsonl.old\");\n}\n",
    )];
    assert!(session_paths(&files).is_empty());
}

#[test]
fn session_paths_keep_scanning_after_a_quote_character_literal() {
    let files = [source_file(
        "hub",
        "src/quotes.rs",
        "fn f() { let quote = '\"'; let path = dir.join(\"sessions\"); }\n",
    )];
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/quotes.rs:1: session path outside log"]
    );
}

#[test]
fn session_paths_skip_test_files_cfg_test_items_and_test_only_modules() {
    let files = [
        source_file(
            "hub",
            "src/lib.rs",
            "#[cfg(test)]\n#[path = \"fake.rs\"]\nmod fake;\n#[cfg(test)]\nfn test_only() { let _ = dir.join(\"sessions\"); }\nfn production() {}\n",
        ),
        source_file("hub", "src/fake.rs", "mod deeper;\n"),
        source_file(
            "hub",
            "src/fake/deeper.rs",
            "fn test_fake() { let _ = dir.join(\"sessions\"); }",
        ),
        file(
            "hub",
            "src/paths_tests.rs",
            "fn test() { let _ = dir.join(\"sessions\"); }".to_owned(),
        ),
        file(
            "hub",
            "tests/paths.rs",
            "fn test() { let _ = dir.join(\"sessions\"); }".to_owned(),
        ),
        source_file(
            "fakes",
            "src/lib.rs",
            "fn fake() { let _ = dir.join(\"sessions\"); }",
        ),
    ];
    assert_eq!(production_files(&files).len(), 1);
    assert!(session_paths(&files).is_empty());
}

#[test]
fn fiber_home_paths_check_construction_and_file_reads_outside_config() {
    assert!(fiber_home_paths(&[]).is_empty());
    let files = [
        source_file(
            "cli",
            "src/new.rs",
            "fn f(inputs: Inputs) {\n    let path = inputs.home\n        .join(\"projects\")\n        .join(\"config.json\");\n    let _ = read(&path);\n}\n",
        ),
        source_file(
            "main",
            "src/other.rs",
            "fn f(home: &Path) {\n    let _ = std::fs::read(home.join(\"SYSTEM.md\"));\n}\n",
        ),
    ];
    assert_eq!(
        fiber_home_paths(&files),
        [
            "crates/cli/src/new.rs:3: Fiber-home path outside config",
            "crates/cli/src/new.rs:4: Fiber-home path outside config",
            "crates/main/src/other.rs:2: Fiber-home path outside config",
        ]
    );
}

#[test]
fn fiber_home_paths_match_prompt_reads_but_not_os_home_or_relative_joins() {
    let files = [
        source_file(
            "main",
            "src/prompt_fixture.rs",
            "fn prompts(home: &Path, project: &ProjectKey) {\n    read_one(home, project, \"SYSTEM.md\");\n    read_one(home, project, r#\"APPEND_SYSTEM.md\"#);\n}\nfn read_one(home: &Path, project: &ProjectKey, name: &str) {\n    let _ = home.join(\"projects\").join(project).join(name);\n}\n",
        ),
        source_file(
            "loop",
            "src/skills.rs",
            "fn places(home: &Path, dir: &Path, other: &Other) {\n    let os = home.join(\".agents/skills\");\n    let os_nested = home.join(\".agents\").join(\"skills\");\n    let config = home.join(\".config\");\n    let config_nested = home.join(\".config\").join(\"config.json\");\n    let relative = dir.join(\"projects\");\n    let lookalike = home.join(\"configurable/projects\");\n    let caller_home = other.home.join(\"projects\");\n    let prompt_prose = \"SYSTEM.md\"; let _ = read(\"notes.txt\");\n}\n",
        ),
    ];
    assert_eq!(
        home_sites(&files[0]).len(),
        4,
        "{:?}",
        home_sites(&files[0])
    );
    assert_eq!(
        fiber_home_paths(&files),
        [
            "crates/main/src/prompt_fixture.rs:2: Fiber-home path outside config",
            "crates/main/src/prompt_fixture.rs:3: Fiber-home path outside config",
            "crates/main/src/prompt_fixture.rs:6: Fiber-home path outside config",
            "crates/main/src/prompt_fixture.rs:6: Fiber-home path outside config",
        ]
    );
}

#[test]
fn cfg_test_struct_fields_do_not_hide_following_production_paths() {
    let files = [source_file(
        "doors",
        "src/isolation.rs",
        "struct Isolation {\n    #[cfg(test)]\n    pause: Option<fn()>,\n}\nfn production(home: &Path) {\n    let path = home\n        .join(\"projects\");\n}\n",
    )];
    assert_eq!(
        fiber_home_paths(&files),
        ["crates/doors/src/isolation.rs:7: Fiber-home path outside config"]
    );
}

#[test]
fn fiber_home_paths_recognize_only_fiber_home_roots() {
    let files = [source_file(
        "main",
        "src/roots.rs",
        "fn f(&self, hub: &Hub, inputs: &Inputs, other: &Other) {\n    self.home.join(\"projects\");\n    hub.home.join(\"run\");\n    inputs.home.join(\"cache\");\n    other.home.join(\"projects\");\n}\n",
    )];
    assert_eq!(
        fiber_home_paths(&files),
        [
            "crates/main/src/roots.rs:2: Fiber-home path outside config",
            "crates/main/src/roots.rs:3: Fiber-home path outside config",
            "crates/main/src/roots.rs:4: Fiber-home path outside config",
        ]
    );
}

#[test]
fn fiber_home_paths_check_configuration_literals_used_by_file_operations() {
    let files = [source_file(
        "main",
        "src/config.rs",
        "fn read_config(workspace: &Path) {\n    let _ = std::fs::read(workspace.join(\".fiber/config.json\"));\n}\n",
    )];
    assert_eq!(
        fiber_home_paths(&files),
        ["crates/main/src/config.rs:2: Fiber-home path outside config"]
    );
}

#[test]
fn home_allowances_distinguish_repeated_sites_in_one_function() {
    let file = source_file(
        "extensions",
        "src/installed.rs",
        "fn removal() {\n    let path = home.join(\"extensions\");\n    dirs.push(home.join(\"extensions\"));\n}\n",
    );
    let sites = home_sites(&file);
    assert_eq!(sites.len(), 2);
    assert!(sites[0].context.contains("::let path"));
    assert!(sites[1].context.contains("::dirs . push"));
    assert_ne!(sites[0].context, sites[1].context);
    let one_site = source_file(
        "extensions",
        "src/installed.rs",
        "fn removal() {\n    let path = home.join(\"extensions\");\n}\n",
    );
    assert_eq!(home_sites(&one_site)[0].context, sites[0].context);
}

#[test]
fn fiber_home_paths_accept_config_ownership_and_require_a_layout_segment() {
    let files = [
        source_file(
            "config",
            "src/read.rs",
            "fn read_config(home: &Path) {\n    let _ = home.join(\"config.json\");\n}\n",
        ),
        source_file(
            "cli",
            "src/relative.rs",
            "fn f(dir: &Path, home: &Path) {\n    let _ = dir.join(\"configurable\");\n    let _ = home.join(\"configurable\");\n}\n",
        ),
    ];
    assert!(fiber_home_paths(&files).is_empty());
}

#[test]
fn loop_tool_names_derives_registered_names_from_owned_definitions() {
    let tools = registrations(&REGISTERED_TOOLS);
    let jobs = source_file(
        "jobs",
        "src/extra.rs",
        "Tool { name: \"delegate_message\".to_owned() }\nTool { name: \"pwd\" }\n",
    );
    let names = derived_tool_names(&[tools, jobs]);
    for name in REGISTERED_TOOLS {
        assert!(names.contains(name), "derived names: {names:?}");
    }
    assert!(names.contains("delegate_message"));
    assert!(!names.contains("pwd"));
    assert!(!names.contains("shellish"));
}

#[test]
fn loop_tool_names_rejects_every_derived_identity_outside_its_label_context() {
    let tools = registrations(&REGISTERED_TOOLS);
    for name in REGISTERED_TOOLS {
        let source = format!("fn f() {{\n    let tool = {name:?};\n}}\n");
        let files = [tools.clone(), file("loop", "src/new.rs", source)];
        assert_eq!(
            loop_tool_names(&files),
            [format!(
                "crates/loop/src/new.rs:2: built-in tool name {name}"
            )],
            "{name}"
        );
    }
}

#[test]
fn loop_tool_names_ignore_longer_names_other_crates_and_comments() {
    let tools = registrations(&REGISTERED_TOOLS);
    let files = [
        tools,
        source_file(
            "loop",
            "src/clean.rs",
            "// \"shell\" is only a comment\nfn f() { let a = \"shellish\"; let b = r#\"my_shell\"#; }\n",
        ),
        file("tools", "src/path.rs", "let x = \"shell\";".to_owned()),
    ];
    assert!(loop_tool_names(&files).is_empty());
}

#[test]
fn loop_tool_names_allows_only_the_five_known_label_contexts() {
    let tools = registrations(&REGISTERED_TOOLS);
    let labels = [
        file("loop", "src/opening.rs", "fn render() { let row = (\"shell\", environment.shell.as_str()); }".to_owned()),
        file("loop", "src/status.rs", "fn key() { let key = match state { SessionState::Jobs => (\"jobs\", None) }; }".to_owned()),
        file("loop", "src/skills.rs", "fn commands() { let tag = if skill.model_invocable { \"skill\" } else { \"template\" }; }".to_owned()),
        file("loop", "src/handoff/carry.rs", "fn restart() { let args = &[(\"jobs\", listed.join(\"\\n\").as_str())]; }".to_owned()),
        file("loop", "src/reviewer.rs", "fn sections() { let sections = Sections { handoff: crate::prompt::section(md, \"handoff\") }; }".to_owned()),
    ];
    let mut files = vec![tools];
    files.extend(labels);
    assert!(
        loop_tool_names(&files).is_empty(),
        "{:?}",
        loop_tool_names(&files)
    );
}

#[test]
fn loop_tool_label_exceptions_do_not_exempt_other_uses_in_the_same_files() {
    let tools = registrations(&REGISTERED_TOOLS);
    let files = [
        tools,
        file("loop", "src/opening.rs", "fn render() { let row = (\"shell\", environment.shell.as_str()); let name = \"shell\"; }".to_owned()),
        file("loop", "src/status.rs", "fn key() { let key = SessionState::Jobs => (\"jobs\", None); let name = \"jobs\"; }".to_owned()),
        file("loop", "src/skills.rs", "fn commands() { let tag = if skill.model_invocable { \"skill\" } else { \"template\" }; let name = \"skill\"; }".to_owned()),
        file("loop", "src/handoff/carry.rs", "fn restart() { let args = &[(\"jobs\", listed.join(\"\\n\").as_str())]; let name = \"jobs\"; }".to_owned()),
        file("loop", "src/reviewer.rs", "fn sections() { let sections = Sections { handoff: crate::prompt::section(md, \"handoff\") }; let name = \"handoff\"; }".to_owned()),
    ];
    let failures = loop_tool_names(&files);
    assert_eq!(failures.len(), 5, "{failures:?}");
    assert!(
        failures
            .iter()
            .any(|line| line.contains("built-in tool name shell"))
    );
    assert!(
        failures
            .iter()
            .any(|line| line.contains("built-in tool name jobs"))
    );
    assert!(
        failures
            .iter()
            .any(|line| line.contains("built-in tool name skill"))
    );
    assert!(
        failures
            .iter()
            .any(|line| line.contains("built-in tool name handoff"))
    );
}

#[test]
fn loop_tool_names_fail_loudly_on_unrecognized_owned_definitions() {
    let tools = source_file(
        "tools",
        "src/definitions.rs",
        "name: String::from(\"first\")\nname: \"second\".to_string()\nname: \"third\".into()\nname: \"pwd\",\n",
    );
    let failures = loop_tool_names(&[tools]);
    assert_eq!(
        failures,
        [
            "crates/tools/src/definitions.rs:1: unrecognized owned tool-name definition",
            "crates/tools/src/definitions.rs:2: unrecognized owned tool-name definition",
            "crates/tools/src/definitions.rs:3: unrecognized owned tool-name definition",
        ]
    );
}

#[test]
fn source_tokens_scan_after_block_comment_closes_and_preserve_comment_boundaries() {
    let files = [source_file(
        "hub",
        "src/comments.rs",
        "fn f() {\n    /* one line */\n    let _ = dir.join(\"sessions\");\n    /* outer\n       /* inner */ let _ = dir.join(\"sessions\");\n    */\n    let _ = dir./* between tokens */join(\"sessions\");\n}\n",
    )];
    assert_eq!(
        session_paths(&files),
        [
            "crates/hub/src/comments.rs:3: session path outside log",
            "crates/hub/src/comments.rs:7: session path outside log",
        ]
    );
}

#[test]
fn source_tokens_count_nested_comment_closes_before_reporting_following_paths() {
    let files = [source_file(
        "hub",
        "src/nested_comments.rs",
        "fn f() {\n    /* outer /* middle /* inner */ let _ = dir.join(\"sessions\"); */ still outer */\n    let _ = dir.join(\"sessions\");\n}\n",
    )];
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/nested_comments.rs:3: session path outside log"]
    );
}

#[test]
fn source_tokens_track_raw_and_multiline_string_delimiters_and_lines() {
    let files = [source_file(
        "hub",
        "src/strings.rs",
        r###"fn f() {
    let _ = r##"#not-the-close " # and dir.join("sessions")"##;
    let _ = "ordinary
        multiline";
    let _ = r#"raw
        multiline"#;
    let _ = dir.join("sessions");
}
"###,
    )];
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/strings.rs:7: session path outside log"]
    );
}

#[test]
fn source_tokens_do_not_end_an_escaped_quote_string_early() {
    let files = [source_file(
        "hub",
        "src/escaped_quote.rs",
        r###"fn f() {
    let _ = "escaped \" let _ = dir.join(\"sessions\")";
    let _ = dir.join("sessions");
}
"###,
    )];
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/escaped_quote.rs:3: session path outside log"]
    );
}

#[test]
fn source_tokens_skip_escaped_and_unicode_character_literals() {
    for literal in [r"'x'", r"'\''", r"'\x41'", r"'\u{1F980}'", "'🦀'"] {
        let source = format!("fn f() {{ let c = {literal}; let _ = dir.join(\"sessions\"); }}");
        let expected = source_tokens("fn f() { let c = ; let _ = dir.join(\"sessions\"); }");
        assert_eq!(source_tokens(&source), expected, "{literal}");
        let files = [source_file("hub", "src/chars.rs", &source)];
        assert_eq!(
            session_paths(&files),
            ["crates/hub/src/chars.rs:1: session path outside log"],
            "{literal}"
        );
    }
}

#[test]
fn cfg_test_detection_distinguishes_other_attributes_and_root_attributes() {
    let files = [source_file(
        "hub",
        "src/attributes.rs",
        "#[cfg(not(test))]\nfn not_test() { let _ = dir.join(\"sessions\"); }\n#[cfg(feature = \"x\")]\nfn feature() { let _ = dir.join(\"sessions\"); }\n#[cfg(test, feature = \"x\")]\nfn combined() { let _ = dir.join(\"sessions\"); }\n#[cfg(test)]\nfn test_only() { let _ = dir.join(\"sessions\"); }\n",
    )];
    assert_eq!(
        session_paths(&files),
        [
            "crates/hub/src/attributes.rs:2: session path outside log",
            "crates/hub/src/attributes.rs:4: session path outside log",
            "crates/hub/src/attributes.rs:6: session path outside log",
        ]
    );
    let root_test = [source_file(
        "hub",
        "src/root_test.rs",
        "#![cfg(test)]\nfn only_test() { let _ = dir.join(\"sessions\"); }\n",
    )];
    assert!(session_paths(&root_test).is_empty());
}

#[test]
fn cfg_test_non_struct_items_do_not_treat_commas_as_field_terminators() {
    let files = [source_file(
        "hub",
        "src/items.rs",
        "#[cfg(test)]\ntype TestOnly = First, let _ = dir.join(\"sessions\");\nfn production() {}\n",
    )];
    assert!(session_paths(&files).is_empty());
}

#[test]
fn cfg_test_struct_array_fields_do_not_hide_following_production_paths() {
    let files = [source_file(
        "hub",
        "src/array_field.rs",
        "struct Fake {\n    #[cfg(test)]\n    bytes: [u8; 2],\n}\nfn production() { let _ = dir.join(\"sessions\"); }\n",
    )];
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/array_field.rs:5: session path outside log"]
    );
}

#[test]
fn test_only_files_shared_with_a_production_module_edge_remain_production() {
    let files = [
        source_file(
            "hub",
            "src/lib.rs",
            "#[cfg(test)] mod fake;\nmod shared;\n",
        ),
        source_file("hub", "src/fake.rs", "#[path = \"shared.rs\"] mod shared;\n"),
        source_file(
            "hub",
            "src/shared.rs",
            "fn production() { let _ = dir.join(\"sessions\"); }\n",
        ),
    ];
    let paths = production_files(&files)
        .into_iter()
        .map(|file| file.rel.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["src/lib.rs", "src/shared.rs"]);
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/shared.rs:1: session path outside log"]
    );
}

#[test]
fn home_sites_keep_variable_join_context_and_name_token_line() {
    let file = source_file(
        "main",
        "src/prompt_fixture.rs",
        "fn helper() {}\nfn read_one(home: &Path, name: &str) {\n    let path = home.join(\n        name\n    ).join(\"projects\");\n}\n",
    );
    let sites = home_sites(&file);
    assert!(sites.iter().any(|site| {
        site.operation == "join:projects"
            && site.context == "fn read_one|root:home/name"
            && site.line == 5
    }), "{sites:?}");
    assert!(sites.iter().any(|site| {
        site.operation == "join:prompt-name"
            && site.context == "fn read_one|root:home"
            && site.line == 4
    }), "{sites:?}");

    let near_function_start = source_file(
        "main",
        "src/prompt_start.rs",
        "fn preceding() {}\nfn read_one(home: &Path, name: &str) {\n    let path = home.join(name);\n}\n",
    );
    assert!(home_sites(&near_function_start).iter().any(|site| {
        site.operation == "join:prompt-name"
            && site.context == "fn read_one|root:home"
            && site.line == 3
    }));
}

#[test]
fn test_only_read_one_join_name_is_not_a_home_path() {
    let files = [source_file(
        "main",
        "src/prompt_fixture.rs",
        "#[cfg(test)]\nmod fake {\n    fn read_one(home: &Path, name: &str) {\n        let path = home.join(\"projects\").join(name);\n    }\n}\n",
    )];
    assert!(home_sites(&files[0]).is_empty());
    assert!(fiber_home_paths(&files).is_empty());
}

#[test]
fn loop_tool_gate_ignores_owned_name_syntax_outside_tools_and_jobs() {
    let tools = registrations(&REGISTERED_TOOLS);
    let loop_source = source_file(
        "loop",
        "src/new.rs",
        "fn unrelated() { let name = String::from(\"not-a-tool-definition\"); }\n",
    );
    assert!(loop_tool_names(&[tools, loop_source]).is_empty());
}

#[test]
fn individual_allowances_are_consumed_once_and_match_path_context_and_operation() {
    let allowance = SourceAllowance {
        path: "crates/hub/src/feed.rs",
        context: "fn feed",
        operation: "join:events.jsonl",
        ticket: "#1725",
    };
    let site = SourceSite {
        path: "crates/hub/src/feed.rs".to_owned(),
        line: 1,
        context: "fn feed".to_owned(),
        operation: "join:events.jsonl".to_owned(),
    };
    assert_eq!(allowance.ticket, "#1725");
    assert!(consume_allowance(&[allowance], &site));
    let mut zero_sites = std::iter::empty();
    let mut unused = vec![allowance];
    assert!(!consume_next_allowance(&mut unused, &mut zero_sites));
    assert_eq!(unused.len(), 1);
    let mut two_sites = [site.clone(), site.clone()].into_iter();
    let mut remaining = vec![allowance];
    assert!(consume_next_allowance(&mut remaining, &mut two_sites));
    assert!(!consume_next_allowance(&mut remaining, &mut two_sites));
    assert!(
        !consume_allowance(
            &[allowance],
            &SourceSite {
                path: "crates/hub/src/other.rs".to_owned(),
                ..site.clone()
            }
        )
    );
    assert!(
        !consume_allowance(
            &[allowance],
            &SourceSite {
                context: "fn other".to_owned(),
                ..site.clone()
            }
        )
    );
    assert!(
        !consume_allowance(
            &[allowance],
            &SourceSite {
                operation: "join:sessions".to_owned(),
                ..site
            }
        )
    );
}
