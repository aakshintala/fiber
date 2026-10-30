use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use fakes::{Deadline, finished};

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

/// Each signal the check reports, as (source, line, pattern). Every source
/// is valid Rust: proc-macro2 reads tokens, not lines, so only code and
/// string contents count. The shell cases hold `kill` beside the pattern
/// in one string, so each is built from parts: this file is scanned too,
/// and one literal holding both would flag itself.
fn signal_cases() -> Vec<(String, usize, &'static str)> {
    vec![
        (
            "fn clean() {}\nfn run() { let _ = Command::new(\"kill\"); }\n".to_owned(),
            2,
            "Command::new(\"kill\")",
        ),
        (
            [
                "fn clean() {}\nfn run() { let _ = do_it(\"",
                "kill",
                " -- -1\"); }\n",
            ]
            .concat(),
            2,
            "-- -",
        ),
        (
            [
                "fn clean() {}\nfn run() { let _ = check(\"kill -- \\\"",
                "-$1",
                "\\\"\"); }\n",
            ]
            .concat(),
            2,
            "\"-$",
        ),
        (
            "fn clean() {}\nfn run(id: u32) { kill_process_group(id); }\n".to_owned(),
            2,
            "kill_process_group",
        ),
        (
            "fn clean() {}\nfn run(id: u32) { test_kill_process(id); }\n".to_owned(),
            2,
            "kill_process(",
        ),
        (
            "fn clean() {}\nfn run(id: u32) { let _ = killpg(id); }\n".to_owned(),
            2,
            "killpg",
        ),
        (
            "fn clean() {}\nfn run(id: u32) { let _ = libc::kill(id, 9); }\n".to_owned(),
            2,
            "libc::kill",
        ),
        (
            "fn clean() {}\nfn run(id: u32) { let _ = nix::sys::signal::kill(id, 9); }\n"
                .to_owned(),
            2,
            "nix::sys::signal",
        ),
    ]
}

#[test]
fn signal_sites_reports_each_signal_in_code() {
    for (source, line, pattern) in signal_cases() {
        let files = [file("log", "src/lib.rs", source)];
        assert_eq!(
            signal_sites(&files),
            [format!("crates/log/src/lib.rs:{line}: {pattern}")],
            "{pattern}"
        );
    }
}

#[test]
fn signal_sites_reports_a_signal_elsewhere_in_tui() {
    for (source, line, pattern) in signal_cases() {
        let files = [file("tui", "src/app.rs", source)];
        let hits = signal_sites(&files);
        assert_eq!(
            hits,
            [format!("crates/tui/src/app.rs:{line}: {pattern}")],
            "{pattern}"
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
fn signal_sites_ignores_a_signal_spelling_inside_a_string() {
    // The pattern text is code-shaped, but it sits in a string literal:
    // strings are values, not signals.
    let files = [file(
        "log",
        "src/lib.rs",
        "fn clean() {}\nfn run() { let _ = \"killpg\"; }\n".to_owned(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_a_signal_inside_a_line_comment() {
    let files = [file(
        "log",
        "src/lib.rs",
        "// call killpg now\nfn clean() {}\n".to_owned(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_a_signal_inside_a_block_comment() {
    let files = [file(
        "log",
        "src/lib.rs",
        "/* kill_process_group(1); */\nfn clean() {}\n".to_owned(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_kill_without_an_operand() {
    // `kill` beside no operand is not a signal, wherever it sits.
    let files = [file(
        "log",
        "src/lib.rs",
        "fn clean() {}\nfn run() { let _ = (\"kill -9\", \"kill -TERM\"); }\n".to_owned(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_an_operand_without_kill() {
    // The operand beside no `kill` is another command's, even one string
    // down from a `kill` of its own. Built from parts so this file holds
    // no `kill` beside an operand.
    let source = [
        "fn a() { let _ = \"kill\"; }\n",
        "fn b() { let _ = run(\"-- -1\"); }\n",
    ]
    .concat();
    let files = [file("log", "src/lib.rs", source)];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_reports_a_shell_signal_across_a_string_newline() {
    // One string holds the whole shell fragment, so the operand joins its
    // `kill` across the newline; the hit sits where the string starts.
    // Built from parts so this file holds no `kill` beside an operand.
    let source = [
        "fn run() {\n    let _ = do_it(\"",
        "kill",
        " -TERM \n        -- -1\");\n}\n",
    ]
    .concat();
    let files = [file("log", "src/lib.rs", source)];
    assert_eq!(
        signal_sites(&files),
        ["crates/log/src/lib.rs:2: -- -".to_owned()]
    );
}

#[test]
fn signal_sites_ignores_near_miss_paths_and_names() {
    // Each guard needs every part: the wrong command, the wrong path, a
    // name without its call, an operand without its `kill`.
    let files = [file(
        "log",
        "src/lib.rs",
        [
            "fn clean() {}\n",
            "fn run(id: u32) {\n",
            "    let _ = Command::new(\"ps\");\n",
            "    let _ = libc::abort(1);\n",
            "    let _ = nix::sys::time::sleep(1);\n",
            "    let _ = test_kill_process;\n",
            "    let _ = do_it(\"-$1\");\n",
            "}\n",
        ]
        .concat(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_a_command_lookalike_without_colons() {
    // `Command::new("kill")` needs both `::` separators: `Command` beside
    // `new("kill")` with no colons between them is not a signal.
    let files = [file(
        "log",
        "src/lib.rs",
        "fn run() {\n    let _ = Command x y new(\"kill\");\n}\n".to_owned(),
    )];
    assert_eq!(signal_sites(&files), Vec::<String>::new());
}

#[test]
fn signal_sites_ignores_a_signal_in_an_allowlisted_file() {
    // Every allowlist entry may hold a real signal; the list is a trust
    // list, so each entry is covered.
    let source =
        "fn run(id: u32, sig: i32) { rustix::process::kill_process_group(id, sig); }\n".to_owned();
    for path in SIGNAL_ALLOWLIST {
        let rel = path.strip_prefix("crates/").unwrap_or(path);
        let krate = rel.split('/').next().unwrap_or_default();
        let rel = rel.split_once('/').map_or(rel, |(_, rest)| rest);
        let files = [file(krate, rel, source.clone())];
        assert_eq!(files[0].path, *path);
        assert_eq!(signal_sites(&files), Vec::<String>::new(), "{path}");
    }
}

#[test]
fn signal_sites_fails_closed_when_a_file_does_not_tokenise() {
    let files = [file("log", "src/lib.rs", "fn broken() { \\ }\n".to_owned())];
    let hits = signal_sites(&files);
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].starts_with("crates/log/src/lib.rs: does not tokenise as Rust: "),
        "{hits:?}"
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

/// The narrowed check (`-e features,no-build`) still fails when a normal
/// edge enables `preserve_order`: only build edges left the tree.
#[test]
fn preserve_order_still_fails_when_a_normal_edge_enables_it() {
    let tree = "0serde_json v1.0.151\n\
1serde_json feature \"preserve_order\"\n\
2tools v0.0.0 (/repo/crates/tools)\n\
1serde_json feature \"std\"\n\
2config v0.0.0 (/repo/crates/config)\n";
    assert_eq!(
        preserve_order(tree),
        [
            "tools: enables serde_json feature \"preserve_order\"; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)"
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
            "fn third() { let path = dir.join(\"events.jsonl\"); }\nfn fourth() { let path = File::open(\"./events.jsonl\"); }\nfn fifth() { let path = dir.join(\"id/events.jsonl\"); }\nfn sixth() { let path = dir.join(\"myevents.jsonl\"); }\nfn seventh() { let path = File::open(\"myevents.jsonl\"); }\n",
        ),
    ];
    assert_eq!(
        session_paths(&files),
        [
            "crates/hub/src/new.rs:4: session path outside log",
            "crates/hub/src/new.rs:8: session path outside log",
            "crates/main/src/other.rs:1: session path outside log",
            "crates/main/src/other.rs:2: session path outside log",
            "crates/main/src/other.rs:3: session path outside log",
        ]
    );
}

#[test]
fn session_paths_ignore_comments_descriptive_strings_and_segment_lookalikes() {
    let files = [source_file(
        "hub",
        "src/clean.rs",
        "// fs::read(\"sessions/old/events.jsonl\")\n/* .join(\"sessions\") */\nfn clean() {\n    let text = r#\"sessions/ is documentation\"#;\n    let description = \"sessions/story\"; let _ = read(\"notes.txt\");\n    let a = dir.join(\"sessions_extra\");\n    let b = dir.join(\"my_sessions\");\n    let c = dir.join(\"events.jsonl.old\");\n    let d = dir.join(\"id/events.jsonl.old\");\n    let e = dir.join(\"myevents.jsonl\");\n    let f = File::open(\"myevents.jsonl\");\n}\n",
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
    assert_eq!(production_files(&files).unwrap().len(), 1);
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
        home_sites(&files[0]).unwrap().len(),
        4,
        "{:?}",
        home_sites(&files[0]).unwrap()
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
fn fiber_home_paths_keep_nested_self_hub_and_inputs_roots() {
    let files = [source_file(
        "main",
        "src/nested_roots.rs",
        "fn f(&self, hub: &Hub, inputs: &Inputs) {\n    self.home.join(\"projects\").join(\"config.json\");\n    hub.home.join(\"run\");\n    inputs.home.join(\"cache\");\n}\n",
    )];
    assert_eq!(
        fiber_home_paths(&files),
        [
            "crates/main/src/nested_roots.rs:2: Fiber-home path outside config",
            "crates/main/src/nested_roots.rs:2: Fiber-home path outside config",
            "crates/main/src/nested_roots.rs:3: Fiber-home path outside config",
            "crates/main/src/nested_roots.rs:4: Fiber-home path outside config",
        ]
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
    let sites = home_sites(&file).unwrap();
    assert_eq!(sites.len(), 2);
    assert!(sites[0].context.contains("::let path"));
    assert!(sites[1].context.contains("::dirs . push"));
    assert_ne!(sites[0].context, sites[1].context);
    let one_site = source_file(
        "extensions",
        "src/installed.rs",
        "fn removal() {\n    let path = home.join(\"extensions\");\n}\n",
    );
    assert_eq!(home_sites(&one_site).unwrap()[0].context, sites[0].context);
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
    let names = derived_tool_names(&[tools, jobs]).unwrap();
    for name in REGISTERED_TOOLS {
        assert!(names.contains(name), "derived names: {names:?}");
    }
    assert!(names.contains("delegate_message"));
    assert!(!names.contains("pwd"));
    assert!(!names.contains("shellish"));
}

#[test]
fn loop_tool_names_rejects_label_shapes_in_the_wrong_function() {
    let tools = registrations(&REGISTERED_TOOLS);
    let files = [
        tools,
        file(
            "loop",
            "src/opening.rs",
            r#"fn other() { let row = ("shell", environment.shell.as_str()); }"#.to_owned(),
        ),
        file(
            "loop",
            "src/status.rs",
            r#"fn other() { let key = match state { SessionState::Jobs => ("jobs", None) }; }"#.to_owned(),
        ),
        file(
            "loop",
            "src/skills.rs",
            r#"fn other() { let tag = if skill.model_invocable { "skill" } else { "template" }; }"#.to_owned(),
        ),
        file(
            "loop",
            "src/handoff/carry.rs",
            r#"fn other() { let args = &[("jobs", listed.join("\n").as_str())]; }"#.to_owned(),
        ),
        file(
            "loop",
            "src/reviewer.rs",
            r#"fn other() { let sections = Sections { handoff: crate::prompt::section(md, "handoff") }; }"#.to_owned(),
        ),
    ];
    assert_eq!(
        loop_tool_names(&files),
        [
            "crates/loop/src/opening.rs:1: built-in tool name shell",
            "crates/loop/src/status.rs:1: built-in tool name jobs",
            "crates/loop/src/skills.rs:1: built-in tool name skill",
            "crates/loop/src/handoff/carry.rs:1: built-in tool name jobs",
            "crates/loop/src/reviewer.rs:1: built-in tool name handoff",
        ]
    );
}

#[test]
fn loop_tool_names_accepts_only_the_exact_label_allowance() {
    let tools = registrations(&REGISTERED_TOOLS);
    let exact = file(
        "loop",
        "src/opening.rs",
        "fn render() { let row = (\"shell\", environment.shell.as_str()); }".to_owned(),
    );
    assert!(loop_tool_names(&[tools.clone(), exact]).is_empty());

    let nearby = file(
        "loop",
        "src/opening.rs",
        "fn other() { let row = (\"shell\", environment.shell.as_str()); }".to_owned(),
    );
    assert_eq!(
        loop_tool_names(&[tools, nearby]),
        ["crates/loop/src/opening.rs:1: built-in tool name shell"]
    );
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
fn loop_sites_skip_tool_identities_in_non_loop_crates() {
    let file = source_file("tools", "src/path.rs", "let x = \"shell\";");
    let names = BTreeSet::from(["shell".to_owned()]);
    assert_eq!(loop_sites(&file, &names).unwrap(), (Vec::new(), Vec::new()));
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
fn loop_tool_label_exceptions_allow_zero_or_one_use_but_not_two() {
    let tools = registrations(&REGISTERED_TOOLS);
    let sites = [
        (
            "src/opening.rs",
            "render",
            "shell",
            "let row = (\"shell\", environment.shell.as_str());",
        ),
        (
            "src/status.rs",
            "key",
            "jobs",
            "let key = match state { SessionState::Jobs => (\"jobs\", None) };",
        ),
        (
            "src/skills.rs",
            "commands",
            "skill",
            "let tag = if skill.model_invocable { \"skill\" } else { \"template\" };",
        ),
        (
            "src/handoff/carry.rs",
            "restart",
            "jobs",
            "let args = &[(\"jobs\", listed.join(\"\\\\n\").as_str())];",
        ),
        (
            "src/reviewer.rs",
            "sections",
            "handoff",
            "let sections = Sections { handoff: crate::prompt::section(md, \"handoff\") };",
        ),
    ];

    for (path, function, name, label) in sites {
        for occurrences in 0..=2 {
            let body = std::iter::repeat_n(label, occurrences)
                .collect::<Vec<_>>()
                .join(" ");
            let source = format!("fn {function}() {{ {body} }}");
            let files = [tools.clone(), file("loop", path, source)];
            let failures = loop_tool_names(&files);
            if occurrences < 2 {
                assert!(failures.is_empty(), "{path}: {failures:?}");
            } else {
                assert_eq!(
                    failures,
                    [format!("crates/loop/{path}:1: built-in tool name {name}")],
                    "{path}"
                );
            }
        }
    }
}

#[test]
fn loop_tool_label_allowance_requires_each_context_field_to_match() {
    let allowance = LABEL_ALLOWANCES[0];
    let matches = |path, function, context, name, reason| {
        label_allowance_matches(&allowance, path, function, context, name, reason)
    };

    assert!(matches(
        allowance.path,
        allowance.function,
        allowance.context,
        allowance.name,
        allowance.reason
    ));
    assert!(!matches(
        "crates/loop/src/other.rs",
        allowance.function,
        allowance.context,
        allowance.name,
        allowance.reason
    ));
    assert!(!matches(
        allowance.path,
        "other",
        allowance.context,
        allowance.name,
        allowance.reason
    ));
    assert!(!matches(
        allowance.path,
        allowance.function,
        "other",
        allowance.name,
        allowance.reason
    ));
    assert!(!matches(
        allowance.path,
        allowance.function,
        allowance.context,
        "other",
        allowance.reason
    ));
    assert!(!matches(
        allowance.path,
        allowance.function,
        allowance.context,
        allowance.name,
        "other"
    ));
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
    assert_eq!(char_literal_end("'''", 0), None);
    assert_eq!(char_literal_end("'\n'", 0), None);
    for literal in [r"'x'", r"'\''", r"'\x41'", r"'\u{1F980}'", "'🦀'"] {
        let source = format!("fn f() {{ let c = {literal}; let _ = dir.join(\"sessions\"); }}");
        let expected =
            source_tokens("fn f() { let c = ; let _ = dir.join(\"sessions\"); }").unwrap();
        assert_eq!(source_tokens(&source).unwrap(), expected, "{literal}");
        let files = [source_file("hub", "src/chars.rs", &source)];
        assert_eq!(
            session_paths(&files),
            ["crates/hub/src/chars.rs:1: session path outside log"],
            "{literal}"
        );
    }

    let malformed_escape = r###"fn f() { let c = '\uX}'; let _ = dir.join("sessions"); }"###;
    assert!(
        source_tokens(malformed_escape)
            .unwrap()
            .iter()
            .any(|token| { matches!(&token.kind, SourceKind::Ident(value) if value == "uX") })
    );
    assert_eq!(
        session_paths(&[source_file(
            "hub",
            "src/malformed_char.rs",
            malformed_escape
        )]),
        ["crates/hub/src/malformed_char.rs:1: session path outside log"]
    );
}

const CURSOR_PROBE_ENV: &str = "XTASK_SOURCE_CURSOR_STALL_PROBE";
const SCANNER_PROBE_ENV: &str = "XTASK_SOURCE_SCANNER_BOUND_PROBE";

fn run_scanner_bound_probe(case: &str) {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "rules::tests::scanner_bound_probe",
            "--nocapture",
        ])
        .env(SCANNER_PROBE_ENV, case)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = finished(
        &format!("scanner bound probe {case}"),
        child,
        &[],
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert!(
        output.status.success(),
        "scanner bound probe {case} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn production_module_cycles_are_bounded() {
    run_scanner_bound_probe("module-cycle");
}

#[test]
fn nested_comment_scanning_is_bounded_by_the_source() {
    run_scanner_bound_probe("nested-comment");
}

#[test]
fn joined_literal_requires_a_complete_join_call() {
    run_scanner_bound_probe("joined-literal");
}

#[test]
fn loop_sites_allow_repetitions_up_to_their_input_token_count() {
    let names = BTreeSet::from(["shell".to_owned()]);
    let source = (0..1100).map(|_| "\"shell\";\n").collect::<String>();
    let file = source_file("loop", "src/many_sites.rs", &source);
    let tokens = source_tokens(&source).unwrap();
    let (sites, definitions) = loop_sites(&file, &names).unwrap();
    assert!(definitions.is_empty());
    assert_eq!(sites.len(), 1100);
    assert!(sites.len() <= tokens.len());
}

#[test]
fn loop_definitions_allow_repetitions_up_to_their_input_token_count() {
    let source = (0..1100)
        .map(|_| "name: String::from(\"custom\");\n")
        .collect::<String>();
    let file = source_file("tools", "src/many_definitions.rs", &source);
    let tokens = source_tokens(&source).unwrap();
    let (_, definitions) = loop_sites(&file, &BTreeSet::new()).unwrap();
    assert_eq!(definitions.len(), 1100);
    assert!(definitions.len() <= tokens.len());
}

#[test]
fn collected_outputs_fail_closed_at_the_input_token_count() {
    let file = source_file("loop", "src/many_sites.rs", "");
    let tokens = source_tokens("a b c").unwrap();
    assert_eq!(tokens.len(), 3);

    let mut sites = Vec::new();
    for token in &tokens {
        push_source_site(
            &file,
            &tokens,
            &mut sites,
            SourceSite {
                path: file.path.clone(),
                line: token.line,
                context: "fn f".to_owned(),
                operation: "tool:shell".to_owned(),
            },
        )
        .unwrap();
    }
    assert_eq!(sites.len(), tokens.len());
    assert_eq!(
        push_source_site(
            &file,
            &tokens,
            &mut sites,
            SourceSite {
                path: file.path.clone(),
                line: 7,
                context: "fn f".to_owned(),
                operation: "tool:shell".to_owned(),
            },
        ),
        Err("crates/loop/src/many_sites.rs:7: source cursor stopped advancing".to_owned())
    );
    assert_eq!(sites.len(), tokens.len());

    let mut failures = Vec::new();
    for token in &tokens {
        push_source_failure(
            &file,
            tokens.len(),
            &mut failures,
            token.line,
            "hit".to_owned(),
        )
        .unwrap();
    }
    assert_eq!(failures.len(), tokens.len());
    assert_eq!(
        push_source_failure(&file, tokens.len(), &mut failures, 7, "extra".to_owned()),
        Err("crates/loop/src/many_sites.rs:7: source cursor stopped advancing".to_owned())
    );
    assert_eq!(failures.len(), tokens.len());

    let mut names = Vec::new();
    for token in &tokens {
        push_bounded(&mut names, "shell".to_owned(), tokens.len(), token.line).unwrap();
    }
    assert_eq!(names.len(), tokens.len());
    assert_eq!(
        push_bounded(&mut names, "extra".to_owned(), tokens.len(), 7),
        Err(StalledCursor { line: 7 })
    );
    assert_eq!(names.len(), tokens.len());
}

#[test]
fn repeated_loop_sites_remain_bounded_and_exact() {
    run_scanner_bound_probe("loop-sites");
}

#[test]
fn loop_tool_scope_mutant_fails_within_a_bounded_workspace_scan() {
    run_scanner_bound_probe("loop-sites-scope");
}

#[test]
fn source_gate_outputs_stay_within_the_total_input_tokens() {
    run_scanner_bound_probe("source-failures");
}

#[test]
fn adversarial_scanner_inputs_complete_with_exact_findings() {
    run_scanner_bound_probe("adversarial");
}

#[test]
fn deep_home_join_chains_are_bounded_and_exact() {
    run_scanner_bound_probe("home-join");
}

#[test]
fn home_join_budget_rejects_at_and_above_each_injected_cap() {
    let limit = 4;
    for (amount, succeeds) in [(limit - 1, true), (limit, false), (limit + 1, false)] {
        let mut steps = HomeJoinBudget::new(limit, 8);
        let result = steps.spend_steps(amount, 7);
        assert_eq!(result.is_ok(), succeeds, "step amount: {amount}");
        if !succeeds {
            assert_eq!(result.unwrap_err(), StalledCursor { line: 7 });
        }

        let mut bytes = HomeJoinBudget::new(8, limit);
        let result = bytes.spend_bytes(amount, 9);
        assert_eq!(result.is_ok(), succeeds, "byte amount: {amount}");
        if !succeeds {
            assert_eq!(result.unwrap_err(), StalledCursor { line: 9 });
        }
    }
}

#[test]
fn home_join_scanner_fails_closed_when_a_budget_is_exhausted() {
    let tokens = source_tokens("home.join(\"a\").join(\"x\")").unwrap();
    let join_dot = tokens
        .iter()
        .enumerate()
        .filter_map(|(index, token)| {
            (punct(Some(token), '.') && ident(tokens.get(index + 1), "join")).then_some(index)
        })
        .next_back()
        .unwrap();

    let mut steps_at_limit = HomeJoinBudget::new(5, 8);
    assert_eq!(
        home_join_context_with_budget(&tokens, join_dot, &mut steps_at_limit),
        Err(StalledCursor { line: 1 })
    );

    let mut bytes_at_limit = HomeJoinBudget::new(6, 7);
    assert_eq!(
        home_join_context_with_budget(&tokens, join_dot, &mut bytes_at_limit),
        Err(StalledCursor { line: 1 })
    );

    let mut just_under_both_limits = HomeJoinBudget::new(6, 8);
    assert_eq!(
        home_join_context_with_budget(&tokens, join_dot, &mut just_under_both_limits),
        Ok(Some("home/a".to_owned()))
    );
}

#[test]
fn home_join_production_limits_stay_within_shard_safe_ceilings() {
    const {
        assert!(
            MAX_HOME_JOIN_WORK <= 16_384,
            "home-join work must stay within the shard-safe 16,384-step ceiling"
        );
        assert!(
            MAX_HOME_JOIN_CONTEXT_BYTES <= 1_048_576,
            "home-join context must stay within the shard-safe 1 MiB ceiling"
        );
    }
}

#[test]
fn joined_literal_false_positives_cannot_walk_unrelated_join_chains() {
    let mut source = String::from("fn f() { let other = path;\n");
    for _ in 0..12 {
        source.push_str("other");
        for _ in 0..512 {
            source.push_str(".join(\"segment\")");
        }
        source.push_str(".read(\"projects\");\n");
    }
    source.push('}');
    let file = source_file("main", "src/unrelated_chains.rs", &source);
    assert!(home_sites(&file).unwrap().is_empty());
}

#[test]
fn unrelated_join_contexts_fail_closed_before_revisiting_prior_calls() {
    let calls = (0..256)
        .map(|_| "other.join(\"projects\");")
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!("fn f() {{\n{calls}\n}}");
    let file = source_file("main", "src/unrelated_joins.rs", &source);
    assert!(home_sites(&file).unwrap().is_empty());
}

#[test]
fn label_context_exemptions_stay_narrow_and_bounded() {
    run_scanner_bound_probe("label-context");
}

#[test]
fn scanner_bound_probe() {
    let Some(case) = std::env::var_os(SCANNER_PROBE_ENV) else {
        return;
    };
    match case.to_str().unwrap() {
        "module-cycle" => {
            let files = [
                source_file("hub", "src/lib.rs", "mod looped;"),
                source_file("hub", "src/looped.rs", "#[path = \"looped.rs\"] mod again;"),
            ];
            let paths = production_files(&files)
                .unwrap()
                .into_iter()
                .map(|file| file.rel.as_str())
                .collect::<Vec<_>>();
            assert_eq!(paths, ["src/lib.rs", "src/looped.rs"]);
        }
        "nested-comment" => {
            let opens = "/*".repeat(2048);
            let closes = "*/".repeat(2048);
            let source = format!("{opens}{closes}\nfn f() {{ let _ = dir.join(\"sessions\"); }}");
            let files = [source_file("hub", "src/nested.rs", &source)];
            assert_eq!(
                session_paths(&files),
                ["crates/hub/src/nested.rs:2: session path outside log"]
            );
        }
        "joined-literal" => {
            let tokens = source_tokens(
                "let joined = home.join(\"sessions\"); let other = home.read(\"sessions\");",
            )
            .unwrap();
            let joined = token_with_string(&tokens, "sessions");
            assert!(joined_literal(&tokens, joined));
            let other = tokens
                .iter()
                .enumerate()
                .skip(joined + 1)
                .find_map(|(i, token)| (string(Some(token)) == Some("sessions")).then_some(i))
                .unwrap();
            assert!(!joined_literal(&tokens, other));
        }
        "loop-sites" => {
            let names = BTreeSet::from(["shell".to_owned()]);
            let source = (0..512)
                .map(|_| {
                    r#"let _ = "shell";
"#
                })
                .collect::<String>();
            let file = source_file("loop", "src/repeated.rs", &source);
            let (sites, definitions) = loop_sites(&file, &names).unwrap();
            assert!(definitions.is_empty());
            assert_eq!(sites.len(), 512);
            assert!(sites.iter().all(|site| site.operation == "tool:shell"));
            let filtered = source_file(
                "loop",
                "src/filtered.rs",
                "// let _ = \"shell\";\n#[cfg(test)] fn fake() { let _ = \"shell\"; }\nfn real() { let _ = \"shell\"; }",
            );
            let (sites, definitions) = loop_sites(&filtered, &names).unwrap();
            assert!(definitions.is_empty());
            assert_eq!(sites.len(), 1);
            assert_eq!(sites[0].line, 3);

            let cycle = [
                source_file("hub", "src/lib.rs", "mod looped;"),
                source_file("hub", "src/looped.rs", "#[path = \"looped.rs\"] mod again;"),
            ];
            assert_eq!(production_files(&cycle).unwrap().len(), 2);
        }
        "loop-sites-scope" => {
            let mut files = vec![registrations(&["shell"])];
            files.extend((0..1025).map(|index| {
                source_file(
                    "hub",
                    &format!("src/hit{index:04}.rs"),
                    "fn f() { let _ = \"shell\"; }",
                )
            }));
            let failures = loop_tool_names(&files);
            assert!(
                failures.is_empty(),
                "expected non-loop tool names to be ignored, got {}: {:?}",
                failures.len(),
                failures.first()
            );
        }
        "source-failures" => {
            let files = (0..1025)
                .map(|index| {
                    source_file(
                        "hub",
                        &format!("src/hit{index:04}.rs"),
                        "fn f() { let _ = dir.join(\"sessions\"); let _ = home.join(\"projects\"); }",
                    )
                })
                .collect::<Vec<_>>();
            let input_tokens = files
                .iter()
                .map(|file| source_tokens(&file.source).unwrap().len())
                .sum::<usize>();
            for (failures, message) in [
                (session_paths(&files), "session path outside log"),
                (fiber_home_paths(&files), "Fiber-home path outside config"),
            ] {
                assert_eq!(failures.len(), files.len());
                assert!(failures.len() <= input_tokens);
                assert!(failures.iter().all(|failure| failure.ends_with(message)));
            }
        }
        "adversarial" => {
            let opens = "/*".repeat(4096);
            let closes = "*/".repeat(4096);
            let deep = format!("{opens}{closes}\nfn f() {{ let _ = dir.join(\"sessions\"); }}");
            assert_eq!(
                session_paths(&[source_file("hub", "src/deep.rs", &deep)]),
                ["crates/hub/src/deep.rs:2: session path outside log"]
            );

            let nested_type = format!("fn({}u8{})", "[".repeat(512), "; 1]".repeat(512));
            let nested_source = format!(
                "struct Fake {{\n#[cfg(test)]\ncallback: {nested_type},\n}}\nfn production() {{\nlet _ = dir.join(\"sessions\");\n}}"
            );
            assert_eq!(
                session_paths(&[source_file("hub", "src/nested_type.rs", &nested_source)]),
                ["crates/hub/src/nested_type.rs:6: session path outside log"]
            );

            let repetitions = (0..1024)
                .map(|_| "fn f() { let _ = dir.join(\"sessions\"); }\n")
                .collect::<String>();
            let findings = session_paths(&[source_file("hub", "src/repetitions.rs", &repetitions)]);
            assert_eq!(findings.len(), 1024);
            assert_eq!(
                findings.first().map(String::as_str),
                Some("crates/hub/src/repetitions.rs:1: session path outside log")
            );
            assert_eq!(
                findings.last().map(String::as_str),
                Some("crates/hub/src/repetitions.rs:1024: session path outside log")
            );

            let pathological = [
                "fn f() {\n",
                r##"    let _ = r#"/* " sessions */"#;
"##,
                r#"    let _ = "escaped \" // sessions";
"#,
                "    /* outer /* inner */ still comment */\n",
                "    let _ = dir.join(\"sessions\");\n}",
            ]
            .concat();
            assert_eq!(
                session_paths(&[source_file("hub", "src/pathological.rs", &pathological)]),
                ["crates/hub/src/pathological.rs:5: session path outside log"]
            );
        }
        "home-join" => {
            let source = format!("home{}", r#".join("segment")"#.repeat(512));
            let tokens = source_tokens(&source).unwrap();
            let join_dot = tokens
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, token)| {
                    (punct(Some(token), '.') && ident(tokens.get(i + 1), "join")).then_some(i)
                })
                .unwrap();
            let expected = format!("home{}", "/segment".repeat(511));
            let actual = home_join_context(&tokens, join_dot).unwrap().unwrap();
            let first_difference = actual
                .bytes()
                .zip(expected.bytes())
                .position(|(actual, expected)| actual != expected)
                .unwrap_or_else(|| actual.len().min(expected.len()));
            if actual != expected {
                panic!(
                    "home join context differs at byte {first_difference}: actual {} bytes, expected {} bytes",
                    actual.len(),
                    expected.len()
                );
            }
        }
        "label-context" => {
            let tools = registrations(&REGISTERED_TOOLS);
            let labels = [
                file("loop", "src/opening.rs", r#"fn render() { let row = ("shell", environment.shell.as_str()); }"#.to_owned()),
                file("loop", "src/status.rs", r#"fn key() { let key = match state { SessionState::Jobs => ("jobs", None) }; }"#.to_owned()),
                file("loop", "src/skills.rs", r#"fn commands() { let tag = if skill.model_invocable { "skill" } else { "template" }; }"#.to_owned()),
                file("loop", "src/handoff/carry.rs", r#"fn restart() { let args = &[("jobs", listed.join("\n").as_str())]; }"#.to_owned()),
                file("loop", "src/reviewer.rs", r#"fn sections() { let sections = Sections { handoff: crate::prompt::section(md, "handoff") }; }"#.to_owned()),
            ];
            let mut files = vec![tools];
            files.extend(labels);
            assert!(loop_tool_names(&files).is_empty());
            files.push(file(
                "loop",
                "src/opening.rs",
                r#"fn render() { let row = ("shell", environment.shell.as_str()); let identity = "shell"; }"#.to_owned(),
            ));
            assert_eq!(
                loop_tool_names(&files),
                ["crates/loop/src/opening.rs:1: built-in tool name shell"]
            );
        }
        other => panic!("unknown scanner probe {other}"),
    }
}

#[test]
fn stalled_cursor_diagnostic_names_the_file_and_line() {
    let source = source_file("hub", "src/stalled.rs", "\n\nfn f() {}\n");
    assert_eq!(
        stalled_cursor_diagnostic(&source, StalledCursor { line: 3 }),
        "crates/hub/src/stalled.rs:3: source cursor stopped advancing"
    );
}

fn run_cursor_stall_probe(case: &str) {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "rules::tests::source_cursor_stall_probe",
            "--nocapture",
        ])
        .env(CURSOR_PROBE_ENV, case)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = finished(
        &format!("source cursor probe {case}"),
        child,
        &[],
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert!(
        output.status.success(),
        "source cursor probe {case} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stalled_peek_at_offset_zero_is_bounded() {
    run_cursor_stall_probe("peek-zero");
}

#[test]
fn stalled_peek_at_offset_one_is_bounded() {
    run_cursor_stall_probe("peek-one");
}

#[test]
fn stalled_next_returning_none_is_bounded() {
    run_cursor_stall_probe("next-none");
}

#[test]
fn stalled_next_returning_zero_is_bounded() {
    run_cursor_stall_probe("next-zero");
}

#[test]
fn stalled_next_returning_one_is_bounded() {
    run_cursor_stall_probe("next-one");
}

#[test]
fn source_cursor_stall_probe() {
    let Some(case) = std::env::var_os(CURSOR_PROBE_ENV) else {
        return;
    };
    let source = match case.to_str().unwrap() {
        "peek-zero" | "peek-one" => "",
        "next-none" => " ",
        "next-zero" | "next-one" => "f",
        other => panic!("unknown cursor probe {other}"),
    };
    let files = [source_file("hub", "src/cursor_probe.rs", source)];
    let failures = session_paths(&files);
    assert!(
        failures.is_empty(),
        "cursor probe scanner failures: {failures:?}"
    );
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

    let inner_test = [source_file(
        "hub",
        "src/inner_test.rs",
        "#[cfg(test)]\nfn only_test() {}\nfn production() { let _ = dir.join(\"sessions\"); }\n",
    )];
    assert_eq!(
        session_paths(&inner_test),
        ["crates/hub/src/inner_test.rs:3: session path outside log"]
    );
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
        "struct Fake {\n    #[cfg(test)]\n    bytes: [u8; 2, dir.join(\"sessions\")],\n}\nfn production() { let _ = dir.join(\"sessions\"); }\n",
    )];
    assert_eq!(
        session_paths(&files),
        ["crates/hub/src/array_field.rs:5: session path outside log"]
    );
}

#[test]
fn test_only_files_shared_with_a_production_module_edge_remain_production() {
    let files = [
        source_file("hub", "src/lib.rs", "#[cfg(test)] mod fake;\nmod shared;\n"),
        source_file(
            "hub",
            "src/fake.rs",
            "#[path = \"shared.rs\"] mod shared;\n",
        ),
        source_file(
            "hub",
            "src/shared.rs",
            "fn production() { let _ = dir.join(\"sessions\"); }\n",
        ),
    ];
    let paths = production_files(&files)
        .unwrap()
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
    let sites = home_sites(&file).unwrap();
    assert!(
        sites.iter().any(|site| {
            site.operation == "join:projects"
                && site.context == "fn read_one|root:home/name"
                && site.line == 5
        }),
        "{sites:?}"
    );
    assert!(
        sites.iter().any(|site| {
            site.operation == "join:prompt-name"
                && site.context == "fn read_one|root:home"
                && site.line == 4
        }),
        "{sites:?}"
    );

    let near_function_start = source_file(
        "main",
        "src/prompt_start.rs",
        "fn preceding() {}\nfn read_one(home: &Path, name: &str) {\n    home.join(name);\n}\n",
    );
    assert!(
        home_sites(&near_function_start)
            .unwrap()
            .iter()
            .any(|site| {
                site.operation == "join:prompt-name"
                    && site.context == "fn read_one|root:home"
                    && site.line == 3
            })
    );
}

#[test]
fn test_only_read_one_join_name_is_not_a_home_path() {
    let files = [source_file(
        "main",
        "src/prompt_fixture.rs",
        "#[cfg(test)]\nmod fake {\n    fn read_one(home: &Path, name: &str) {\n        let path = home.join(\"projects\").join(name);\n    }\n}\n",
    )];
    assert!(home_sites(&files[0]).unwrap().is_empty());
    assert!(fiber_home_paths(&files).is_empty());
}

#[test]
fn loop_tool_gate_ignores_owned_name_syntax_outside_tools_and_jobs() {
    let tools = registrations(&REGISTERED_TOOLS);
    let loop_source = source_file(
        "loop",
        "src/new.rs",
        "fn unrelated() { let tool = Tool { name: String::from(\"custom\") }; }\n",
    );
    assert!(loop_tool_names(&[tools, loop_source]).is_empty());
}

#[test]
fn individual_allowances_require_a_valid_ticket_after_matching_the_site() {
    let site = SourceSite {
        path: "crates/hub/src/feed.rs".to_owned(),
        line: 1,
        context: "fn feed".to_owned(),
        operation: "join:events.jsonl".to_owned(),
    };
    let allowance = |ticket| SourceAllowance {
        path: "crates/hub/src/feed.rs",
        context: "fn feed",
        operation: "join:events.jsonl",
        ticket,
    };

    assert!(consume_allowance(&[allowance("#1725")], &site));
    assert!(!consume_allowance(&[allowance("#")], &site));
    assert!(!consume_allowance(&[allowance("#not-a-number")], &site));
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
    let unticketed = SourceAllowance {
        ticket: "",
        ..allowance
    };
    assert!(!consume_allowance(&[unticketed], &site));
    let mut zero_sites = std::iter::empty();
    let mut unused = vec![allowance];
    assert!(!consume_next_allowance(&mut unused, &mut zero_sites));
    assert_eq!(unused.len(), 1);
    let mut two_sites = [site.clone(), site.clone()].into_iter();
    let mut remaining = vec![allowance];
    assert!(consume_next_allowance(&mut remaining, &mut two_sites));
    assert!(!consume_next_allowance(&mut remaining, &mut two_sites));
    assert!(!consume_allowance(
        &[allowance],
        &SourceSite {
            path: "crates/hub/src/other.rs".to_owned(),
            ..site.clone()
        }
    ));
    assert!(!consume_allowance(
        &[allowance],
        &SourceSite {
            context: "fn other".to_owned(),
            ..site.clone()
        }
    ));
    assert!(!consume_allowance(
        &[allowance],
        &SourceSite {
            operation: "join:sessions".to_owned(),
            ..site
        }
    ));
}

fn token_with_string(tokens: &[SourceToken], value: &str) -> usize {
    tokens
        .iter()
        .position(|token| string(Some(token)) == Some(value))
        .unwrap()
}

fn token_with_ident(tokens: &[SourceToken], value: &str) -> usize {
    tokens
        .iter()
        .position(|token| ident(Some(token), value))
        .unwrap()
}

fn reject_label_after_change(
    file: &RustFile,
    source: &str,
    name: &str,
    offset: isize,
    replacement: SourceKind,
) {
    let mut tokens = source_tokens(source).unwrap();
    let at = token_with_string(&tokens, name);
    let changed = if offset < 0 {
        at.checked_sub(offset.unsigned_abs()).unwrap()
    } else {
        at.saturating_add(offset.cast_unsigned())
    };
    tokens[changed].kind = replacement;
    assert_eq!(label_context(file, &tokens, at, name), None, "{source}");
}

#[test]
fn source_predicates_return_true_and_false_at_each_boundary() {
    let tokens = source_tokens("before , after").unwrap();
    let before = token_with_ident(&tokens, "before");
    let after = token_with_ident(&tokens, "after");
    assert!(next_punct(&tokens, before, 1, ','));
    assert!(!next_punct(&tokens, before, 1, ';'));
    assert!(!next_punct(&tokens, after, 1, ','));
    assert!(next_ident(&tokens, before, 2, "after"));
    assert!(!next_ident(&tokens, before, 2, "before"));
    assert!(!next_ident(&tokens, after, 1, "after"));
    assert!(previous_punct(&tokens, after, 1, ','));
    assert!(!previous_punct(&tokens, after, 1, ';'));
    assert!(!previous_punct(&tokens, before, 1, ','));
    assert!(previous_ident(&tokens, after, 2, "before"));
    assert!(!previous_ident(&tokens, after, 2, "after"));
    assert!(!previous_ident(&tokens, before, 1, "before"));
}

#[test]
fn function_and_scope_contexts_stay_inside_their_matching_braces() {
    let tokens =
        source_tokens("fn first() { let a = 1; } fn second() { let b = 2; } let top = 3;").unwrap();
    let a = token_with_ident(&tokens, "a");
    let b = token_with_ident(&tokens, "b");
    let top = token_with_ident(&tokens, "top");
    assert_eq!(function_context(&tokens, a), "fn first");
    assert_eq!(function_context(&tokens, b), "fn second");
    assert_eq!(function_context(&tokens, top), "module");
    assert_eq!(scope_context(&tokens, b), "fn second ( )::let b = 2");
    assert_eq!(scope_context(&tokens, top), "module");
}

#[test]
fn scope_context_starts_after_the_previous_item_boundary() {
    let tokens = source_tokens("fn prior() {} fn next() { let value = 1; }").unwrap();
    let value = token_with_ident(&tokens, "value");
    assert_eq!(scope_context(&tokens, value), "fn next ( )::let value = 1");
}

#[test]
fn struct_body_requires_a_matched_opening_brace_and_excludes_its_close() {
    let source = "struct S { #[cfg(test)] field: u8 } fn f() { #[cfg(test)] let x = 1; }";
    let tokens = source_tokens(source).unwrap();
    let first_cfg = tokens
        .iter()
        .position(|token| ident(Some(token), "cfg"))
        .unwrap();
    let close = tokens
        .iter()
        .position(|token| matches!(token.kind, SourceKind::Punct('}')))
        .unwrap();
    let function_cfg = tokens
        .iter()
        .rposition(|token| ident(Some(token), "cfg"))
        .unwrap();
    assert!(struct_body(&tokens, first_cfg));
    assert!(!struct_body(&tokens, close));
    assert!(!struct_body(&tokens, function_cfg));

    let unmatched = source_tokens("struct S { #[cfg(test)] field: u8").unwrap();
    let cfg = unmatched
        .iter()
        .position(|token| ident(Some(token), "cfg"))
        .unwrap();
    assert!(!struct_body(&unmatched, cfg));
}

#[test]
fn test_item_end_tracks_nested_delimiters_and_field_generics() {
    let function_type = source_tokens("callback: fn(A, B), next: Path").unwrap();
    let callback = token_with_ident(&function_type, "callback");
    let commas = function_type
        .iter()
        .enumerate()
        .filter_map(|(i, token)| punct(Some(token), ',').then_some(i))
        .collect::<Vec<_>>();
    assert_eq!(commas.len(), 2);
    assert_eq!(
        test_item_end(&function_type, callback, true).unwrap(),
        commas[1]
    );

    let generic = source_tokens("value: Map<A, B>, next: Path").unwrap();
    let value = token_with_ident(&generic, "value");
    let commas = generic
        .iter()
        .enumerate()
        .filter_map(|(i, token)| punct(Some(token), ',').then_some(i))
        .collect::<Vec<_>>();
    assert_eq!(test_item_end(&generic, value, true).unwrap(), commas[1]);
}

#[test]
fn test_item_end_does_not_pop_a_mismatched_delimiter() {
    let source = source_tokens("callback: fn(A], B), next: Path").unwrap();
    let callback = token_with_ident(&source, "callback");
    let commas = source
        .iter()
        .enumerate()
        .filter_map(|(i, token)| punct(Some(token), ',').then_some(i))
        .collect::<Vec<_>>();
    assert_eq!(test_item_end(&source, callback, true).unwrap(), commas[1]);

    let comparison = source_tokens("callback: fn(A > B, C), next: Path").unwrap();
    let callback = token_with_ident(&comparison, "callback");
    let commas = comparison
        .iter()
        .enumerate()
        .filter_map(|(i, token)| punct(Some(token), ',').then_some(i))
        .collect::<Vec<_>>();
    assert_eq!(
        test_item_end(&comparison, callback, true).unwrap(),
        commas[1]
    );
}

#[test]
fn test_item_end_returns_the_last_token_for_an_unclosed_body() {
    let tokens = source_tokens("item { body").unwrap();
    assert_eq!(test_item_end(&tokens, 0, false).unwrap(), tokens.len() - 1);
}

#[test]
fn test_item_end_ignores_angle_comparisons_in_non_field_items() {
    let tokens = source_tokens("const TEST: bool = LEFT < RIGHT; fn next() {}").unwrap();
    let start = token_with_ident(&tokens, "const");
    let semicolon = tokens
        .iter()
        .position(|token| punct(Some(token), ';'))
        .unwrap();
    assert_eq!(test_item_end(&tokens, start, false).unwrap(), semicolon);

    let tokens =
        source_tokens("#[cfg(test)] const ONLY: bool = LEFT < RIGHT; fn production() {}").unwrap();
    let marked = test_only_tokens(&tokens).unwrap();
    let production = token_with_ident(&tokens, "production");
    assert!(!marked[production]);
}

#[test]
fn cfg_test_attribute_requires_every_part_of_the_direct_attribute() {
    let direct = source_tokens("cfg ( test )").unwrap();
    assert!(cfg_test_attribute(&direct, 0, direct.len()));

    // Each near miss keeps the right-hand side of one conjunction true while
    // making its left-hand side false. Replacing any `&&` with `||` then
    // mistakes the near miss for a direct cfg(test) attribute.
    for source in [
        "cfg ( test ) extra", // The span has an extra token.
        "wrong ( test )",     // Not the cfg attribute.
        "cfg wrong test )",   // No opening parenthesis.
        "cfg ( other )",      // The condition is not test.
        "cfg ( test wrong",   // No closing parenthesis.
    ] {
        let tokens = source_tokens(source).unwrap();
        assert!(!cfg_test_attribute(&tokens, 0, tokens.len()), "{source}");
    }
}

#[test]
fn test_only_tokens_marks_direct_cfg_test_but_not_conditional_or_unattributed_cfg() {
    let tokens = source_tokens(
        "let production = 1; #[cfg(feature = \"x\")] fn feature() {} #[cfg_attr(test, cfg(test))] fn conditional() {} #[cfg_attr(test, cfg_attr(feature = \"x\", cfg(test)))] fn deeper() {} cfg(test); fn outside() {} #[cfg(test)] fn fixture() {} mod outer { #[cfg(test)] fn nested() {} }",
    )
    .unwrap();
    let marked = test_only_tokens(&tokens).unwrap();
    for name in ["production", "feature", "conditional", "deeper", "outside"] {
        assert!(!marked[token_with_ident(&tokens, name)], "{name}");
    }
    for name in ["fixture", "nested"] {
        assert!(marked[token_with_ident(&tokens, name)], "{name}");
    }
}

#[test]
fn conditional_cfg_attributes_remain_production_in_source_gates() {
    let files = [source_file(
        "hub",
        "src/cfg_attributes.rs",
        "#[cfg_attr(test, cfg(test))]\nfn conditional() { let _ = dir.join(\"sessions\"); }\n#[cfg_attr(test, cfg_attr(feature = \"x\", cfg(test)))]\nfn deeper() { let _ = dir.join(\"sessions\"); }\nfn plain_code() { cfg(test); let _ = dir.join(\"sessions\"); }\n#[cfg(test)]\nfn fixture() { let _ = dir.join(\"sessions\"); }\n",
    )];
    assert_eq!(
        session_paths(&files),
        [
            "crates/hub/src/cfg_attributes.rs:2: session path outside log",
            "crates/hub/src/cfg_attributes.rs:4: session path outside log",
            "crates/hub/src/cfg_attributes.rs:5: session path outside log",
        ]
    );
}

#[test]
fn path_segment_matches_complete_present_and_absent_segments() {
    assert!(path_segment("id/events.jsonl", "events.jsonl"));
    assert!(path_segment("sessions/id/events.jsonl", "sessions"));
    assert!(!path_segment("id/events.jsonl.old", "events.jsonl"));
    assert!(!path_segment("sessions_extra/id/events.jsonl", "sessions"));
}

#[test]
fn session_paths_match_each_side_of_the_path_and_operation_alternative() {
    let joined = [source_file(
        "hub",
        "src/joined.rs",
        "fn build() { let _path = dir.join(\"sessions/id\"); }\n",
    )];
    assert_eq!(
        session_paths(&joined),
        ["crates/hub/src/joined.rs:1: session path outside log"]
    );

    let read = [source_file(
        "hub",
        "src/read.rs",
        "fn read() { let _path = std::fs::read(\"sessions/id\"); }\n",
    )];
    assert_eq!(
        session_paths(&read),
        ["crates/hub/src/read.rs:1: session path outside log"]
    );

    let clean = [source_file(
        "hub",
        "src/lookalike.rs",
        "fn clean() { let _path = std::fs::read(\"sessions_extra/id\"); }\n",
    )];
    assert!(session_paths(&clean).is_empty());
}

#[test]
fn events_jsonl_segment_is_detected_for_joins_and_file_operations_only() {
    let files = [
        source_file(
            "hub",
            "src/joined_events.rs",
            "fn build() { let _path = dir.join(\"id/events.jsonl\"); }\n",
        ),
        source_file(
            "hub",
            "src/read_events.rs",
            "fn read() { let _path = std::fs::read(\"id/events.jsonl\"); }\n",
        ),
        source_file(
            "hub",
            "src/lookalike_events.rs",
            "fn clean() { let _path = dir.join(\"id/events.jsonl.old\"); let text = \"id/events.jsonl\"; }\n",
        ),
    ];
    assert_eq!(
        session_paths(&files),
        [
            "crates/hub/src/joined_events.rs:1: session path outside log",
            "crates/hub/src/read_events.rs:1: session path outside log",
        ]
    );
}

#[test]
fn module_target_resolves_explicit_and_default_module_paths() {
    let explicit = source_file("hub", "src/lib.rs", "#[path = \"fake/deep.rs\"] mod fake;");
    let tokens = source_tokens(&explicit.source).unwrap();
    let module = token_with_ident(&tokens, "mod");
    assert_eq!(
        module_target(&explicit, "fake", &tokens, module).as_deref(),
        Some("crates/hub/src/fake/deep.rs")
    );

    let default = source_file("hub", "src/parent.rs", "mod child;");
    let tokens = source_tokens(&default.source).unwrap();
    let module = token_with_ident(&tokens, "mod");
    assert_eq!(
        module_target(&default, "child", &tokens, module).as_deref(),
        Some("crates/hub/src/parent/child.rs")
    );

    for source in [
        "item [path = \"wrong.rs\"] mod child;",
        "#[other = \"wrong.rs\"] mod child;",
        "#[path != \"wrong.rs\"] mod child;",
    ] {
        let file = source_file("hub", "src/lib.rs", source);
        let tokens = source_tokens(source).unwrap();
        let module = token_with_ident(&tokens, "mod");
        assert_eq!(
            module_target(&file, "child", &tokens, module).as_deref(),
            Some("crates/hub/src/child.rs"),
            "{source}"
        );
    }
}

#[test]
fn production_file_lookup_tests_matching_and_nonmatching_paths() {
    let files = [
        source_file("hub", "src/lib.rs", "mod child;"),
        source_file("hub", "src/child.rs", "fn child() {}"),
    ];
    let candidates = files.iter().collect::<Vec<_>>();
    assert_eq!(
        production_file(&candidates, "crates/hub/src/child.rs").map(|file| file.rel.as_str()),
        Some("src/child.rs")
    );
    assert_eq!(
        production_file(&candidates, "crates/hub/src/other.rs"),
        None
    );
}

#[test]
fn production_files_does_not_treat_a_local_binding_as_a_module_edge() {
    let files = [
        source_file(
            "hub",
            "src/lib.rs",
            "#[cfg(test)] fn test_only() { let child; }",
        ),
        source_file("hub", "src/child.rs", "fn production() {}"),
    ];
    let paths = production_files(&files)
        .unwrap()
        .into_iter()
        .map(|file| file.rel.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["src/lib.rs", "src/child.rs"]);
}

#[test]
fn joined_literal_and_file_operation_context_require_the_complete_call() {
    let joined = source_tokens("home.join(\"projects\")").unwrap();
    let path = token_with_string(&joined, "projects");
    assert!(joined_literal(&joined, path));
    assert!(!joined_literal(&joined, path - 1));
    let other_method = source_tokens("home.read(\"projects\")").unwrap();
    let path = token_with_string(&other_method, "projects");
    assert!(!joined_literal(&other_method, path));

    let late_join = source_tokens("a b c d e f . join(\"projects\")").unwrap();
    let late_path = token_with_string(&late_join, "projects");
    assert!(joined_literal(&late_join, late_path));

    let calls = source_tokens(
        "read(\"SYSTEM.md\"); ordinary(\"SYSTEM.md\"); read(\"other\"); \"SYSTEM.md\"; home.read(\"projects\")",
    )
    .unwrap();
    let prompt = calls
        .iter()
        .enumerate()
        .filter_map(|(i, token)| (string(Some(token)) == Some("SYSTEM.md")).then_some(i))
        .collect::<Vec<_>>();
    assert!(file_operation_near(&calls, prompt[0]));
    assert!(!file_operation_near(&calls, prompt[1]));
    assert!(!file_operation_near(&calls, prompt[2]));
}

#[test]
fn home_join_context_and_fiber_root_exclusion_cover_both_outcomes() {
    let tokens = source_tokens(
        "home.join(\"projects\").join(\"config.json\"); home.join(\".agents\").join(\"skills\"); home.join(\".config\").join(\"config.json\"); other.home.join(\"projects\"); home.not_join(\"projects\").join(\"cache\");",
    )
    .unwrap();
    let dots = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, token)| {
            (punct(Some(token), '.') && ident(tokens.get(i + 1), "join")).then_some(i)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        home_join_context(&tokens, dots[0]),
        Ok(Some("home".to_owned()))
    );
    assert_eq!(
        home_join_context(&tokens, dots[1]),
        Ok(Some("home/projects".to_owned()))
    );
    assert_eq!(
        fiber_home_join(&tokens, dots[0]),
        Ok(Some("home".to_owned()))
    );
    assert_eq!(fiber_home_join(&tokens, dots[3]), Ok(None));
    assert_eq!(fiber_home_join(&tokens, dots[5]), Ok(None));
    assert_eq!(home_join_context(&tokens, dots[6]), Ok(None));
    assert_eq!(home_join_context(&tokens, dots[7]), Ok(None));
}

#[test]
fn home_scope_context_needs_the_matching_path_operation_and_scope_prefix() {
    let file = source_file(
        "extensions",
        "src/installed.rs",
        "fn removal() { let path = home.join(\"extensions\"); }",
    );
    let tokens = source_tokens(&file.source).unwrap();
    let at = token_with_string(&tokens, "extensions");
    let context = "fn removal|root:home".to_owned();
    let scoped = home_scope_context(&file, &tokens, at, context.clone(), "join:extensions");
    assert!(
        scoped.starts_with("fn removal|root:home|scope:"),
        "{scoped}"
    );
    assert_eq!(
        home_scope_context(&file, &tokens, at, context.clone(), "join:projects"),
        context
    );
    assert_eq!(
        home_scope_context(
            &file,
            &tokens,
            at,
            "fn other|root:home".to_owned(),
            "join:extensions"
        ),
        "fn other|root:home"
    );
    let other_file = source_file("main", "src/installed.rs", &file.source);
    assert_eq!(
        home_scope_context(&other_file, &tokens, at, context.clone(), "join:extensions"),
        context
    );
}

#[test]
fn home_sites_accepts_both_configuration_layout_paths_and_rejects_lookalikes() {
    let config_json = source_file(
        "main",
        "src/config_json.rs",
        "fn f() { read(\".fiber/config.json\"); }",
    );
    let config_dir = source_file(
        "main",
        "src/config_dir.rs",
        "fn f() { read(\".fiber/config\"); }",
    );
    assert_eq!(
        home_sites(&config_json).unwrap()[0].operation,
        "config-path:.fiber/config.json"
    );
    assert_eq!(
        home_sites(&config_dir).unwrap()[0].operation,
        "config-path:.fiber/config"
    );
    for source in [
        "fn f() { read(\"other/config.json\"); }",
        "fn f() { read(\".fiber/configurable\"); }",
        "fn f() { read(\".fiber/config.json.old\"); }",
    ] {
        assert!(
            home_sites(&source_file("main", "src/clean.rs", source))
                .unwrap()
                .is_empty(),
            "{source}"
        );
    }
}

#[test]
fn prompt_name_detection_requires_a_join_call_in_read_one() {
    let valid = source_file(
        "main",
        "src/prompt.rs",
        "fn read_one(home: &Path, name: &str) { home.join(name); }",
    );
    assert_eq!(home_sites(&valid).unwrap()[0].operation, "join:prompt-name");
    let wrong_method = source_file(
        "main",
        "src/prompt.rs",
        "fn read_one(home: &Path, name: &str) { home.other(name); }",
    );
    assert!(home_sites(&wrong_method).unwrap().is_empty());
}

#[test]
fn stalled_cursor_consume_returns_a_bounded_error() {
    let mut cursor = SourceCursor::new("");
    assert!(matches!(
        cursor.consume("/*"),
        Err(StalledCursor { line: 1 })
    ));
}

#[test]
fn string_start_counts_raw_delimiter_hashes() {
    assert_eq!(string_start(br###"r##"value"##"###, 0), Some((3, true, 2)));
    assert_eq!(string_start(br##"r#"value"#"##, 0), Some((2, true, 1)));
    assert_eq!(string_start(b"r\"value\"", 0), Some((1, true, 0)));
    assert_eq!(string_start(b"cr###\"value\"###", 0), Some((5, true, 3)));
    assert_eq!(string_start(b"r##missing-quote", 0), None);
}

#[test]
fn each_label_context_requires_every_neighbour_token() {
    let cases = [
        (
            file("loop", "src/opening.rs", String::new()),
            "fn render() { let row = (\"shell\", environment.shell.as_str()); }",
            "shell",
            "render",
            "fills the opening environment template",
            &[
                (-1, SourceKind::Punct('!')),
                (1, SourceKind::Punct('!')),
                (2, SourceKind::Ident("other".to_owned())),
                (3, SourceKind::Punct('!')),
                (4, SourceKind::Ident("other".to_owned())),
            ][..],
        ),
        (
            file("loop", "src/status.rs", String::new()),
            "fn key() { let key = match state { SessionState::Jobs => (\"jobs\", None) }; }",
            "jobs",
            "key",
            "names a session state key",
            &[
                (-1, SourceKind::Punct('!')),
                (-2, SourceKind::Punct('!')),
                (-3, SourceKind::Punct('!')),
                (1, SourceKind::Punct('!')),
                (2, SourceKind::Ident("other".to_owned())),
            ][..],
        ),
        (
            file("loop", "src/skills.rs", String::new()),
            "fn commands() { let tag = if skill.model_invocable { \"skill\" } else { \"template\" }; }",
            "skill",
            "commands",
            "labels a listed command",
            &[
                (-1, SourceKind::Punct('!')),
                (-2, SourceKind::Ident("other".to_owned())),
                (1, SourceKind::Punct('!')),
                (2, SourceKind::Ident("other".to_owned())),
            ][..],
        ),
        (
            file("loop", "src/handoff/carry.rs", String::new()),
            "fn restart() { let args = &[(\"jobs\", listed.join(\"\\n\").as_str())]; }",
            "jobs",
            "restart",
            "fills the handoff template",
            &[
                (-1, SourceKind::Punct('!')),
                (1, SourceKind::Punct('!')),
                (2, SourceKind::Ident("other".to_owned())),
                (3, SourceKind::Punct('!')),
                (4, SourceKind::Ident("other".to_owned())),
            ][..],
        ),
        (
            file("loop", "src/reviewer.rs", String::new()),
            "fn sections() { let sections = Sections { handoff: crate::prompt::section(md, \"handoff\") }; }",
            "handoff",
            "sections",
            "selects the handoff section",
            &[
                (-1, SourceKind::Punct('!')),
                (-2, SourceKind::Ident("other".to_owned())),
                (-4, SourceKind::Ident("other".to_owned())),
                (-7, SourceKind::Ident("other".to_owned())),
                (-10, SourceKind::Ident("other".to_owned())),
                (-12, SourceKind::Ident("other".to_owned())),
            ][..],
        ),
    ];

    for (file, source, name, function, reason, near_misses) in cases {
        let tokens = source_tokens(source).unwrap();
        let at = token_with_string(&tokens, name);
        assert_eq!(
            label_context(&file, &tokens, at, name),
            Some((function, reason)),
            "{source}"
        );
        for (offset, replacement) in near_misses {
            reject_label_after_change(&file, source, name, *offset, replacement.clone());
        }
    }
}

#[test]
fn derived_tool_names_scale_with_the_input_token_count() {
    let source = (0..1100)
        .map(|index| format!("name: \"tool_{index}\".to_owned();\n"))
        .collect::<String>();
    let file = source_file("tools", "src/many_names.rs", &source);
    let tokens = source_tokens(&source).unwrap();
    let names = derived_tool_names(&[file]).unwrap();
    assert_eq!(names.len(), 1100);
    assert!(names.len() <= tokens.len());
}

#[test]
fn derived_tool_names_are_exactly_the_owned_definitions() {
    let tools = registrations(&REGISTERED_TOOLS);
    let jobs = source_file(
        "jobs",
        "src/extra.rs",
        "Tool { name: \"delegate_message\".to_owned() }\nTool { name: \"pwd\" }\n",
    );
    let names = derived_tool_names(&[tools, jobs]).unwrap();
    assert_eq!(
        names,
        REGISTERED_TOOLS
            .into_iter()
            .chain(["delegate_message"])
            .map(str::to_owned)
            .collect()
    );
}

#[test]
fn owned_tool_definition_and_unrecognized_forms_need_name_and_colon() {
    let tokens = source_tokens(
        "name: \"known\".to_owned(); other: \"not-name\".to_owned(); name \"missing-colon\".to_owned(); name: String::from(\"custom\");",
    )
    .unwrap();
    let name_positions = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, token)| ident(Some(token), "name").then_some(i))
        .collect::<Vec<_>>();
    assert_eq!(
        owned_tool_definition(&tokens, name_positions[0]).as_deref(),
        Some("known")
    );
    let other = token_with_ident(&tokens, "other");
    assert_eq!(owned_tool_definition(&tokens, other), None);
    assert_eq!(owned_tool_definition(&tokens, name_positions[1]), None);
    assert_eq!(owned_tool_definition(&tokens, name_positions[2]), None);
    assert!(unrecognized_tool_definition(&tokens, name_positions[2]));
    assert!(!unrecognized_tool_definition(&tokens, other));
    assert!(!unrecognized_tool_definition(&tokens, name_positions[1]));
}

/// The committed maxima beside the `scripts/check` kinds-ratchet step: the
/// occurrence counts at the merge base. Lower them with the conversions,
/// never raise them for a new ordered site.
const COMMITTED_KINDS_MAX: usize = 246;
const COMMITTED_SUPPORT_KINDS_MAX: usize = 37;

fn kinds_tree(kinds: usize, support: usize) -> Vec<RustFile> {
    // Exactly `kinds` and `support` occurrences, with two literal sites
    // sharing one line whenever the count allows, so the shared-line case
    // is always exercised.
    let mut literal = String::new();
    let mut remaining = kinds;
    if remaining >= 2 {
        literal.push_str("assert_eq!(kinds(&a), A); assert_eq!(kinds(&b), B);\n");
        remaining -= 2;
    }
    for _ in 0..remaining {
        literal.push_str("assert_eq!(kinds(&lines), EXPECTED);\n");
    }
    let mut direct = String::new();
    let mut remaining = support;
    if remaining >= 2 {
        direct.push_str("assert_eq!(support::kinds(&a), A); assert_eq!(support::kinds(&b), B);\n");
        remaining -= 2;
    }
    for _ in 0..remaining {
        direct.push_str("assert_eq!(support::kinds(&lines), EXPECTED);\n");
    }
    vec![
        file("main", "tests/sites.rs", literal),
        file("main", "tests/more.rs", direct),
    ]
}

#[test]
fn the_kinds_ratchet_passes_at_and_below_both_maxima() {
    for (kinds, support) in [
        (COMMITTED_KINDS_MAX, COMMITTED_SUPPORT_KINDS_MAX),
        (COMMITTED_KINDS_MAX - 1, COMMITTED_SUPPORT_KINDS_MAX - 1),
    ] {
        let files = kinds_tree(kinds, support);
        assert_eq!(kinds_counts(&files), (kinds, support));
        assert!(kinds_ratchet(&files, COMMITTED_KINDS_MAX, COMMITTED_SUPPORT_KINDS_MAX).is_empty());
    }
}

#[test]
fn the_kinds_ratchet_fails_one_added_occurrence_of_each_literal() {
    // One added occurrence over the maximum fails, naming the count and the
    // maximum; two call sites on one line both count.
    let failures = kinds_ratchet(
        &kinds_tree(COMMITTED_KINDS_MAX + 1, COMMITTED_SUPPORT_KINDS_MAX),
        COMMITTED_KINDS_MAX,
        COMMITTED_SUPPORT_KINDS_MAX,
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains(&format!("{}", COMMITTED_KINDS_MAX + 1))
            && failures[0].contains(&format!("{COMMITTED_KINDS_MAX}")),
        "{}",
        failures[0]
    );
    let failures = kinds_ratchet(
        &kinds_tree(COMMITTED_KINDS_MAX, COMMITTED_SUPPORT_KINDS_MAX + 1),
        COMMITTED_KINDS_MAX,
        COMMITTED_SUPPORT_KINDS_MAX,
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains(&format!("{}", COMMITTED_SUPPORT_KINDS_MAX + 1))
            && failures[0].contains(&format!("{COMMITTED_SUPPORT_KINDS_MAX}")),
        "{}",
        failures[0]
    );
}

#[test]
fn the_kinds_ratchet_ignores_files_outside_crates() {
    let mut files = kinds_tree(COMMITTED_KINDS_MAX, COMMITTED_SUPPORT_KINDS_MAX);
    files.push(file(
        "xtask",
        "src/sites.rs",
        "assert_eq!(kinds(&lines), EXPECTED);\nassert_eq!(support::kinds(&lines), EXPECTED);\n"
            .to_owned(),
    ));
    assert!(kinds_ratchet(&files, COMMITTED_KINDS_MAX, COMMITTED_SUPPORT_KINDS_MAX).is_empty());
}

#[test]
fn unrecognized_tool_names_reject_nearby_owned_string_spellings() {
    let source = "name: Other::from(\"x\"); name: String X: from(\"x\"); name: String::to_owned(\"x\"); name: Other . to_string(); name: \"x\": to_string(); name String::from(\"wrong\"); other: String::from(\"wrong\");";
    let tokens = source_tokens(source).unwrap();
    let name_positions = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, token)| ident(Some(token), "name").then_some(i))
        .collect::<Vec<_>>();
    assert_eq!(name_positions.len(), 6);
    for at in name_positions {
        assert!(!unrecognized_tool_definition(&tokens, at), "{source}");
    }
    let other = token_with_ident(&tokens, "other");
    assert!(!unrecognized_tool_definition(&tokens, other), "{source}");
}
