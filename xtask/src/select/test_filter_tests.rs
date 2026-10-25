use super::*;
use crate::select::tests::{members, strings};

#[test]
fn test_files_map_to_their_tests() {
    let files = strings(&[
        "crates/log/src/writer_tests.rs",
        "crates/log/src/fold/tests.rs",
        "crates/log/src/tests.rs",
        "crates/loop/tests/turns.rs",
        "crates/loop/tests/support/mod.rs",
        "crates/log/src/fold/inner_tests.rs",
        "crates/log/src/lib_tests.rs",
        "crates/loop/src/main_tests.rs",
        "crates/log/src/writer.rs",
        "crates/log/build_tests.rs",
        "docs/events.md",
    ]);
    let (expression, packages) = test_filter(&files, &members(), &[]);
    assert_eq!(
        expression.split(" | ").collect::<Vec<_>>(),
        [
            "(package(log) & test(/^writer::tests::/))",
            "(package(log) & test(/^fold::tests::/))",
            "(package(log) & test(/^tests::/))",
            "binary_id(loop::turns)",
            "(package(log) & test(/^fold::inner::tests::/))",
            "(package(log) & test(/^tests::/))",
            "(package(loop) & test(/^tests::/))",
        ]
    );
    assert_eq!(packages, strings(&["log", "loop"]));
}

#[test]
fn an_example_test_file_maps_to_its_binary() {
    let files = strings(&[
        "crates/bench/examples/bench/run_tests.rs",
        "crates/bench/examples/bench/main_tests.rs",
        "crates/bench/examples/bench/fold/inner_tests.rs",
        "crates/bench/examples/bench/run.rs",
    ]);
    let (expression, packages) = test_filter(&files, &members(), &[]);
    assert_eq!(
        expression.split(" | ").collect::<Vec<_>>(),
        [
            "(binary_id(bench::example/bench) & test(/^run::tests::/))",
            "(binary_id(bench::example/bench) & test(/^tests::/))",
            "(binary_id(bench::example/bench) & test(/^fold::inner::tests::/))",
        ]
    );
    assert_eq!(packages, strings(&["bench"]));
}

#[test]
fn a_path_declared_example_test_file_maps_to_its_declaring_module() {
    let files = strings(&["crates/bench/examples/bench/run_tests.rs"]);
    let sources = vec![decl(
        "bench",
        "crates/bench/examples/bench/run.rs",
        "#[cfg(test)]\n#[path = \"run_tests.rs\"]\nmod tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(binary_id(bench::example/bench) & test(/^run::tests::/))"
    );
}

#[test]
fn only_a_crate_root_file_is_a_crate_root() {
    let (expression, _) = test_filter(
        &strings(&["crates/log/src/fold/lib_tests.rs"]),
        &members(),
        &[],
    );
    assert_eq!(expression, "(package(log) & test(/^fold::lib::tests::/))");
}

#[test]
fn no_test_files_give_an_empty_filter() {
    assert_eq!(
        test_filter(&strings(&["crates/log/src/writer.rs"]), &members(), &[]),
        (String::new(), vec![])
    );
}

fn decl(krate: &str, path: &str, source: &str) -> RustFile {
    RustFile {
        krate: krate.to_owned(),
        path: path.to_owned(),
        rel: path
            .strip_prefix(&format!("crates/{krate}/"))
            .unwrap_or(path)
            .to_owned(),
        source: source.to_owned(),
    }
}

#[test]
fn support_files_under_tests_add_no_filter_term() {
    let files = strings(&[
        "crates/loop/tests/support/mod.rs",
        "crates/loop/tests/support/fake.rs",
    ]);
    assert_eq!(
        test_filter(&files, &members(), &[]),
        (String::new(), vec![])
    );
}

#[test]
fn a_path_declared_test_file_maps_to_its_declaring_module() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[cfg(test)]\n#[path = \"idle_tests.rs\"]\nmod idle_tests;",
    )];
    let (expression, packages) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(package(main) & test(/^settings::idle_tests::/))"
    );
    assert_eq!(packages, strings(&["main"]));
}

#[test]
fn a_path_declared_test_file_inside_a_path_declared_module_resolves_recursively() {
    // The intermediate module's declared name (`background`) differs from
    // its file stem (`background_impl`), so the conventional answer
    // (`shell::background_impl::tests::`) differs: resolving only one level
    // would fail this test.
    let files = strings(&["crates/log/src/shell/background_tests.rs"]);
    let sources = vec![
        decl(
            "log",
            "crates/log/src/shell.rs",
            "#[path = \"shell/background_impl.rs\"] mod background;",
        ),
        decl(
            "log",
            "crates/log/src/shell/background_impl.rs",
            "#[cfg(test)] #[path = \"background_tests.rs\"] mod tests;",
        ),
    ];
    let (expression, packages) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(package(log) & test(/^shell::background::tests::/))"
    );
    assert_eq!(packages, strings(&["log"]));
}

#[test]
fn a_path_declaration_in_a_mod_rs_file_resolves_beside_it() {
    let files = strings(&["crates/log/src/fold/fold_extra_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/fold/mod.rs",
        "#[path = \"fold_extra_tests.rs\"] mod extra;",
    )];
    let (expression, packages) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^fold::extra::/))");
    assert_eq!(packages, strings(&["log"]));
}

#[test]
fn a_source_that_does_not_tokenise_keeps_the_conventional_filter() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "fn broken( {\n",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn two_declarations_for_one_file_fall_back_to_convention() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![
        decl(
            "main",
            "crates/main/src/settings.rs",
            "#[path = \"idle_tests.rs\"] pub mod idle_tests;",
        ),
        decl(
            "main",
            "crates/main/src/other.rs",
            "#[path = \"idle_tests.rs\"] mod idle_tests;",
        ),
    ];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_path_attribute_on_a_non_mod_item_registers_nothing() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] use crate::idle;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn an_inline_module_after_a_path_attribute_registers_nothing() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] mod idle_tests {}",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_test_file_declared_in_lib_rs_maps_to_the_crate_root() {
    let files = strings(&["crates/log/src/foo_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/lib.rs",
        "#[path = \"foo_tests.rs\"] mod foo_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^foo_tests::/))");
}

#[test]
fn a_test_file_declared_in_main_rs_maps_to_the_crate_root() {
    let files = strings(&["crates/log/src/foo_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/main.rs",
        "#[path = \"foo_tests.rs\"] mod foo_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^foo_tests::/))");
}

#[test]
fn a_test_file_declared_in_a_nested_file_chains_through_it() {
    // The declarer `src/a/b.rs` is a non-`mod` nested file: `a::b`.
    let files = strings(&["crates/log/src/c_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/a/b.rs",
        "#[path = \"../c_tests.rs\"] mod c;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^a::b::c::/))");
}

#[test]
fn a_test_file_declared_in_a_mod_rs_chains_through_it() {
    let files = strings(&["crates/log/src/d_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/a/mod.rs",
        "#[path = \"../d_tests.rs\"] mod d;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^a::d::/))");
}

#[test]
fn a_test_file_declared_in_a_top_level_file_chains_through_it() {
    let files = strings(&["crates/log/src/bar_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/foo.rs",
        "#[path = \"bar_tests.rs\"] mod bar;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^foo::bar::/))");
}

#[test]
fn a_declaration_cycle_falls_back_to_convention() {
    // `a.rs` declares `b.rs` and `b.rs` declares `a.rs`: resolving the
    // test file through them must terminate at the conventional filter.
    let files = strings(&["crates/log/src/cycle_tests.rs"]);
    let sources = vec![
        decl(
            "log",
            "crates/log/src/a.rs",
            "#[path = \"b.rs\"] mod b;\n#[path = \"cycle_tests.rs\"] mod cycle_tests;",
        ),
        decl("log", "crates/log/src/b.rs", "#[path = \"a.rs\"] mod a;"),
    ];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^cycle::tests::/))");
}

#[test]
fn scope_of_names_a_tests_binary() {
    assert_eq!(
        scope_of("crates/loop/tests/turns.rs", &members(), &BTreeMap::new()),
        Some((
            "loop".to_owned(),
            Scope::Binary("turns".to_owned()),
            String::new(),
        ))
    );
}

#[test]
fn scope_of_names_a_src_package_with_its_test_prefix() {
    assert_eq!(
        scope_of(
            "crates/log/src/writer_tests.rs",
            &members(),
            &BTreeMap::new(),
        ),
        Some((
            "log".to_owned(),
            Scope::Package,
            "writer::tests::".to_owned(),
        ))
    );
}

#[test]
fn scope_of_names_an_example_binary_with_its_test_prefix() {
    assert_eq!(
        scope_of(
            "crates/bench/examples/bench/fold/inner_tests.rs",
            &members(),
            &BTreeMap::new(),
        ),
        Some((
            "bench".to_owned(),
            Scope::Example("bench".to_owned()),
            "fold::inner::tests::".to_owned(),
        ))
    );
}

#[test]
fn scope_of_ignores_a_file_in_a_subdirectory_of_tests() {
    assert_eq!(
        scope_of(
            "crates/loop/tests/support/mod.rs",
            &members(),
            &BTreeMap::new()
        ),
        None,
    );
    assert_eq!(
        scope_of(
            "crates/loop/tests/completion/bash.rs",
            &members(),
            &BTreeMap::new()
        ),
        None,
    );
}

#[test]
fn scope_of_ignores_a_non_test_file() {
    assert_eq!(
        scope_of("crates/log/src/writer.rs", &members(), &BTreeMap::new()),
        None,
    );
    assert_eq!(
        scope_of("crates/log/build_tests.rs", &members(), &BTreeMap::new()),
        None,
    );
    // A test-named path without the `.rs` suffix is no Rust file.
    assert_eq!(
        scope_of("crates/loop/tests/turns", &members(), &BTreeMap::new()),
        None,
    );
}

#[test]
fn scope_of_ignores_a_file_outside_every_member() {
    assert_eq!(
        scope_of("crates/unknown/tests/t.rs", &members(), &BTreeMap::new()),
        None,
    );
}

#[test]
fn test_files_are_named_or_under_tests() {
    assert!(is_test_file("src/tests.rs"));
    assert!(is_test_file("src/a/b_tests.rs"));
    assert!(is_test_file("tests/journey.rs"));
    assert!(!is_test_file("src/tests_helper.rs"));
    assert!(!is_test_file("src/a/tests/fixture.rs"));
}
