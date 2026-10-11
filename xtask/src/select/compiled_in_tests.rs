use super::*;
use crate::select::tests::{members, members_with_tools, package_members, strings};
use crate::select::{Selection, classify_with};

fn contract_src(source: &str) -> RustFile {
    RustFile {
        krate: "contract".to_owned(),
        path: "crates/contract/src/lib.rs".to_owned(),
        rel: "src/lib.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn loop_src(source: &str) -> RustFile {
    RustFile {
        krate: "loop".to_owned(),
        path: "crates/loop/src/reviewer.rs".to_owned(),
        rel: "src/reviewer.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn tools_src(source: &str) -> RustFile {
    RustFile {
        krate: "tools".to_owned(),
        path: "crates/tools/src/guidelines.rs".to_owned(),
        rel: "src/guidelines.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn tui_src(source: &str) -> RustFile {
    RustFile {
        krate: "tui".to_owned(),
        path: "crates/tui/src/theme_tests.rs".to_owned(),
        rel: "src/theme_tests.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn xtask_src(source: &str) -> RustFile {
    RustFile {
        krate: "xtask".to_owned(),
        path: "xtask/src/ci_needs_tests.rs".to_owned(),
        rel: "src/ci_needs_tests.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn main_src(source: &str) -> RustFile {
    RustFile {
        krate: "main".to_owned(),
        path: "crates/main/tests/ask.rs".to_owned(),
        rel: "tests/ask.rs".to_owned(),
        source: source.to_owned(),
    }
}

/// The listed includes of `krate`, each resolvable from its fixture source.
fn listed_includes(krate: &str) -> String {
    COMPILED_IN
        .iter()
        .filter(|(_, listed)| *listed == krate)
        .map(|(path, _)| format!("include_str!(\"../../../{path}\");\n"))
        .collect()
}

/// Every listed include, each in a source of the crate the list names.
fn listed_files() -> Vec<RustFile> {
    vec![
        contract_src(&listed_includes("contract")),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
        tui_src(&listed_includes("tui")),
        xtask_src(&listed_includes("xtask")),
        main_src(&listed_includes("main")),
    ]
}

/// `listed_files()` with the `contract` source replaced by `source`.
fn listed_files_with_contract(source: &str) -> Vec<RustFile> {
    let mut files = listed_files();
    for file in &mut files {
        if file.krate == "contract" {
            *file = contract_src(source);
        }
    }
    files
}

#[test]
fn a_listed_include_passes() {
    let files = listed_files();
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_unlisted_outside_include_fails() {
    let source = format!(
        "{}include_str!(\"../../../README.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        ["README.md: contract compiles it in, but the compiled-in list does not list it"]
    );
}

#[test]
fn an_unlisted_non_docs_outside_include_fails() {
    let source = format!(
        "{}include_str!(\"../../../LICENSE\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        ["LICENSE: contract compiles it in, but the compiled-in list does not list it"]
    );
}

#[test]
fn an_unlisted_markdown_inside_the_crate_dir_fails() {
    let source = format!(
        "{}include_str!(\"../prompt/system.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/prompt/system.md: contract compiles it in, but the compiled-in list does not list it"
        ]
    );
}

#[test]
fn a_listed_markdown_inside_the_crate_dir_runs_that_crate_alone() {
    let mut listed = COMPILED_IN.to_vec();
    listed.push(("crates/contract/prompt/system.md", "contract"));
    let selection = classify_with(
        &strings(&["crates/contract/prompt/system.md"]),
        &members(),
        &listed,
        &[],
    );
    assert_eq!(selection, Selection::Crates(strings(&["contract"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_non_docs_include_inside_the_crate_dir_is_unlisted() {
    let source = format!(
        "{}include_str!(\"owned.bin\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_raw_string_include_is_resolved() {
    for extra in [
        r#"include_str!(r"../../../README.md");"#,
        r##"include_str!(r#"../../../README.md"#);"##,
    ] {
        let source = format!("{}{extra}", listed_includes("contract"));
        let files = listed_files_with_contract(&source);
        assert_eq!(
            compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
            ["README.md: contract compiles it in, but the compiled-in list does not list it"],
            "{extra}"
        );
    }
}

#[test]
fn an_unresolvable_include_argument_fails() {
    let source = format!(
        "{}include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../README.md\"));",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: include_str! argument is not a string literal; the compiled-in check cannot resolve it"
        ]
    );
}

#[test]
fn another_macro_with_a_string_argument_yields_no_target() {
    let source = format!("{}my_macro!(\"../x.md\");", listed_includes("contract"));
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn include_bytes_yields_its_target() {
    let source = format!(
        "{}include_bytes!(\"../x.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/x.md: contract compiles it in, but the compiled-in list does not list it"
        ]
    );
}

#[test]
fn a_trailing_comma_on_an_include_is_accepted() {
    for extra in [
        r#"include_str!("../../../README.md",);"#,
        r#"include_bytes!("../../../README.md",);"#,
    ] {
        let source = format!("{}{extra}", listed_includes("contract"));
        let files = listed_files_with_contract(&source);
        assert_eq!(
            compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
            ["README.md: contract compiles it in, but the compiled-in list does not list it"],
            "{extra}"
        );
    }
}

#[test]
fn an_include_with_tokens_after_the_literal_fails() {
    let source = format!(
        r#"{}include_str!("a.md", "b");"#,
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: include_str! argument is not a string literal; the compiled-in check cannot resolve it"
        ]
    );
}

fn package_src(krate: &str, path: &str, source: &str) -> RustFile {
    RustFile {
        krate: krate.to_owned(),
        path: path.to_owned(),
        rel: "src/lib.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn config_reads_a_package() -> RustFile {
    package_src(
        "config",
        "crates/config/tests/credentials.rs",
        "fn dir() -> PathBuf {\n    std::path::PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers/opencode\")\n}\n",
    )
}

fn main_reads_a_package() -> RustFile {
    package_src(
        "main",
        "crates/main/tests/ask.rs",
        "fn package(name: &str) -> PathBuf {\n    Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers\").join(name)\n}\n",
    )
}

fn cli_reads_a_package() -> RustFile {
    package_src(
        "cli",
        "crates/cli/src/login/browser_tests.rs",
        "fn copy() -> PathBuf {\n    std::path::PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers/codex\")\n}\n",
    )
}

fn extensions_reads_a_package() -> RustFile {
    package_src(
        "extensions",
        "crates/extensions/build.rs",
        "let manifest = env!(\"CARGO_MANIFEST_DIR\");\nlet root = \"../../providers\";\n",
    )
}

fn xtask_reads_a_package() -> RustFile {
    package_src(
        "xtask",
        "xtask/tests/models_dev.rs",
        "let providers = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../providers\");\n",
    )
}

fn package_ok_files() -> Vec<RustFile> {
    vec![
        cli_reads_a_package(),
        config_reads_a_package(),
        extensions_reads_a_package(),
        main_reads_a_package(),
        xtask_reads_a_package(),
    ]
}

#[test]
fn the_package_reader_list_matches_its_sources() {
    assert_eq!(
        package_reader_mismatches(&package_ok_files(), &package_members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_unlisted_crate_that_reads_a_package_fails() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let root = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn a_listed_crate_with_no_reading_source_fails() {
    assert_eq!(
        package_reader_mismatches(&[config_reads_a_package()], &package_members()).unwrap(),
        [
            "cli: listed as reading a first-party package, but no source reads one",
            "extensions: listed as reading a first-party package, but no source reads one",
            "main: listed as reading a first-party package, but no source reads one",
            "xtask: listed as reading a first-party package, but no source reads one"
        ]
    );
}

#[test]
fn a_package_literal_in_a_comment_does_not_count() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let root = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\n// let old = \"../../providers/opencode\";\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_package_literal_without_the_manifest_does_not_count() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let dir = \"../../providers/opencode\";\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_joined_providers_segment_counts() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let root = PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"..\").join(\"..\").join(\"providers\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn a_raw_string_package_literal_counts() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let dir = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(r\"../../providers/opencode\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn xtask_sources_count_as_package_readers_like_any_crate() {
    let mut files = package_ok_files();
    files.push(package_src(
        "xtask",
        "xtask/src/select_tests.rs",
        "let dir = \"../../providers/opencode\";\nlet manifest = env!(\"CARGO_MANIFEST_DIR\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        Vec::<String>::new()
    );
    let without_xtask: Vec<RustFile> = package_ok_files()
        .into_iter()
        .filter(|file| file.krate != "xtask")
        .collect();
    assert_eq!(
        package_reader_mismatches(&without_xtask, &package_members()).unwrap(),
        ["xtask: listed as reading a first-party package, but no source reads one"]
    );
}

#[test]
fn a_crates_extensions_segment_counts() {
    // Fails safe: a `CARGO_MANIFEST_DIR` file naming `"crates/extensions"`
    // counts as reading a package, so the list carries it.
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let dir = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/extensions/foo\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn non_package_literals_with_the_manifest_do_not_count() {
    // Kills the `==` -> `!=` mutants on either segment comparison in
    // `reads_package::walk`: with `!=`, every one of these literals would
    // count as a package path. `"tests/pages"` is the real case
    // (`crates/tools/src/web_fetch/pages_tests.rs` joins it onto
    // `CARGO_MANIFEST_DIR` without reading a package).
    for literal in ["../../research", "tests/pages", "providersX/a"] {
        let mut files = package_ok_files();
        files.push(package_src(
            "tools",
            "crates/tools/src/search.rs",
            &format!("let dir = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"{literal}\");\n"),
        ));
        assert_eq!(
            package_reader_mismatches(&files, &package_members()).unwrap(),
            Vec::<String>::new(),
            "{literal}"
        );
    }
}

#[test]
fn a_file_that_does_not_tokenise_fails_the_package_check() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "fn broken( {\n",
    ));
    let failure = package_reader_mismatches(&files, &package_members()).unwrap_err();
    assert!(
        failure.starts_with("crates/tools/src/search.rs: does not tokenise as Rust: "),
        "{failure}"
    );
}

#[test]
fn an_include_str_in_a_comment_is_ignored() {
    let source = format!(
        "{}// include_str!(\"../../../README.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}
