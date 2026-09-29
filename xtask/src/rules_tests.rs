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
fn the_line_cap_fails_a_source_file_over_800_lines() {
    let files = [
        file("log", "src/big.rs", "x\n".repeat(801)),
        file("log", "src/ok.rs", "x\n".repeat(800)),
    ];
    assert_eq!(
        over_cap(&files),
        ["crates/log/src/big.rs: 801 lines, over the 800-line cap"]
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
    assert!(uses_unsafe("fn f() { unsafe { g() } }"));
    assert!(uses_unsafe("unsafe fn f() {}"));
    assert!(uses_unsafe("let s = \"a\\\"b\"; unsafe {}"));
    assert!(uses_unsafe("let c = '\\''; let d = 'x'; unsafe {}"));
    assert!(uses_unsafe("fn f<'a>(x: &'a str) { unsafe {} }"));
    assert!(uses_unsafe("/* a */ unsafe {}"));
    assert!(uses_unsafe("let s = r#\"x\"#; unsafe {}"));
}

#[test]
fn unsafe_in_comments_strings_and_names_is_not_code() {
    assert!(!uses_unsafe(
        "#![deny(unsafe_code)]\n// unsafe here\n/* unsafe\n */ fn f() {}"
    ));
    assert!(!uses_unsafe("let s = \"unsafe\"; let t = \"\\\" unsafe\";"));
    assert!(!uses_unsafe("let s = r#\"unsafe \"quoted\" \"#;"));
    assert!(!uses_unsafe("let s = r\"unsafe\";"));
    assert!(!uses_unsafe("fn not_unsafe() {} fn unsafely() {}"));
    assert!(!uses_unsafe("// unsafe at the end"));
    assert!(!uses_unsafe("/* unsafe never closed"));
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

#[test]
fn code_only_keeps_code_and_blanks_the_rest() {
    let cases = [
        ("a // c\nb // d", "a \nb "),
        ("a /* c */ b", "a   b"),
        ("a / b * c", "a / b * c"),
        ("x \"s\\\"t\" y", "x \"\" y"),
        ("x \"a\\\\\" y", "x \"\" y"),
        ("x r#\"a\"b\"# z", "x \"\" z"),
        ("x r\"a\\\" z", "x \"\" z"),
        ("x r##\"a\"#b\"## z", "x \"\" z"),
        ("for\"a\" x", "for\"\" x"),
        ("r#type", "r#type"),
        ("a '\\n' b '\\'' c 'x' d", "a ' ' b ' ' c ' ' d"),
        ("a '\\u{1F600}' b", "a ' ' b"),
        ("f<'a>(x: &'a T)", "f<'a>(x: &'a T)"),
        ("\"unterminated unsafe", "\"\""),
        ("/* unterminated", " "),
        ("// only", ""),
        ("r\"open", "\"\""),
    ];
    for (source, code) in cases {
        assert_eq!(code_only(source), code, "{source:?}");
    }
}
