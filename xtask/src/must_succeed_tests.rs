use super::*;

fn file(rel: &str, source: &str) -> RustFile {
    RustFile {
        krate: "x".to_owned(),
        path: format!("crates/x/{rel}"),
        rel: rel.to_owned(),
        source: source.to_owned(),
    }
}

/// The failures for one test file, with no allowances.
fn check(source: &str) -> Vec<String> {
    must_succeed(&[file("src/a_tests.rs", source)], &[])
}

fn flagged(source: &str) -> bool {
    !check(source).is_empty()
}

#[test]
fn a_const_is_flagged_from_one_second_up_to_just_below_the_floor() {
    assert!(flagged("const W: Duration = Duration::from_secs(1);"));
    assert!(flagged("const W: Duration = Duration::from_secs(9);"));
    assert!(!flagged("const W: Duration = Duration::from_secs(10);"));
    assert!(!flagged("const W: Duration = Duration::from_secs(11);"));
    assert!(!flagged("const W: Duration = Duration::from_secs(0);"));
}

#[test]
fn a_millis_const_is_flagged_between_999_and_the_floor() {
    assert!(flagged("const W: Duration = Duration::from_millis(1_000);"));
    assert!(flagged("const W: Duration = Duration::from_millis(9_999);"));
    assert!(!flagged("const W: Duration = Duration::from_millis(999);"));
    assert!(!flagged(
        "const W: Duration = Duration::from_millis(10_000);"
    ));
}

#[test]
fn a_const_with_a_path_visibility_or_a_function_body_is_flagged() {
    assert!(flagged(
        "const W: std::time::Duration = std::time::Duration::from_secs(5);"
    ));
    assert!(flagged(
        "const W: Duration = std::time::Duration::from_secs(5);"
    ));
    assert!(flagged(
        "pub(crate) const W: Duration = Duration::from_secs(5);"
    ));
    assert!(flagged(
        "fn a() { const W: Duration = Duration::from_secs(5); }"
    ));
}

#[test]
fn a_const_of_another_type_or_shape_is_not_flagged() {
    assert!(!flagged("const W: u64 = 5;"));
    assert!(!flagged(
        "const W: Option<Duration> = Some(Duration::from_secs(5));"
    ));
    assert!(!flagged("const W: Duration = Duration::from_secs(5) * 3;"));
    assert!(!flagged("const W: Duration = MUST_SUCCEED_WITHIN;"));
}

#[test]
fn a_const_failure_names_the_site_the_value_and_the_floor() {
    assert_eq!(
        check("\n\nconst WITHIN: Duration = Duration::from_secs(5);"),
        ["crates/x/src/a_tests.rs:3: const WITHIN is 5 s, below MUST_SUCCEED_WITHIN (10 s)"]
    );
    assert_eq!(
        check("const WITHIN: Duration = Duration::from_millis(1_500);"),
        ["crates/x/src/a_tests.rs:1: const WITHIN is 1500 ms, below MUST_SUCCEED_WITHIN (10 s)"]
    );
}

#[test]
fn a_deadline_after_a_short_literal_is_flagged_in_each_path_form() {
    assert!(flagged(
        "fn a() { Deadline::after(Duration::from_secs(5)); }"
    ));
    assert!(flagged(
        "fn a() { fakes::Deadline::after(Duration::from_secs(5)); }"
    ));
    assert!(flagged(
        "fn a() { fakes::Deadline::after(std::time::Duration::from_secs(5)); }"
    ));
    assert!(flagged(
        "fn a() { Deadline::after(\n    Duration::from_secs(5),\n); }"
    ));
    assert_eq!(
        check("fn a() {\n    Deadline::after(Duration::from_secs(3));\n}"),
        [
            "crates/x/src/a_tests.rs:2: Deadline::after(Duration::from_secs(3)) is below MUST_SUCCEED_WITHIN (10 s)"
        ]
    );
}

#[test]
fn a_deadline_after_a_name_an_expression_or_a_bound_at_the_floor_is_not_flagged() {
    assert!(!flagged("fn a() { Deadline::after(MUST_SUCCEED_WITHIN); }"));
    assert!(!flagged("fn a() { Deadline::after(WAIT); }"));
    assert!(!flagged(
        "fn a() { Deadline::after(MUST_SUCCEED_WITHIN + Duration::from_secs(5)); }"
    ));
    assert!(!flagged(
        "fn a() { Deadline::after(Duration::from_secs(10)); }"
    ));
    assert!(!flagged(
        "fn a() { Deadline::after(Duration::from_millis(999)); }"
    ));
    assert!(!flagged("fn a() { after(Duration::from_secs(5)); }"));
    assert!(!flagged("fn a() { other::after(Duration::from_secs(5)); }"));
    assert!(!flagged(
        "fn a() { Deadline::after(base, Duration::from_secs(5)); }"
    ));
}

#[test]
fn a_wait_call_with_a_short_literal_argument_is_flagged() {
    for call in [
        "await_requests(1, Duration::from_secs(5))",
        "wait_line(x, Duration::from_secs(2))",
        "recv_timeout(Duration::from_secs(3))",
        "script.wait_hang_waiting(1, Duration::from_secs(5))",
        "waiter.wait_hang_matched(1, Duration::from_secs(5))",
    ] {
        assert!(flagged(&format!("fn a() {{ {call}; }}")), "{call}");
    }
    assert_eq!(
        check("fn a() {\n    script.wait_hang_waiting(1, Duration::from_secs(5));\n}"),
        [
            "crates/x/src/a_tests.rs:2: wait_hang_waiting(.., Duration::from_secs(5)) is below MUST_SUCCEED_WITHIN (10 s)"
        ]
    );
}

#[test]
fn a_wait_call_with_a_long_zero_or_named_argument_is_not_flagged() {
    for call in [
        "wait_hang_waiting(1, Duration::ZERO)",
        "wait_hang_matched(1, Duration::from_secs(30))",
        "wait_hang_waiting(1, MUST_SUCCEED_WITHIN)",
        "wait_hang_waiting(1, Duration::from_secs(10))",
        "recv_timeout(Duration::from_millis(999))",
    ] {
        assert!(!flagged(&format!("fn a() {{ {call}; }}")), "{call}");
    }
}

#[test]
fn clock_arithmetic_a_step_and_a_let_are_not_flagged() {
    assert!(!flagged(
        "fn a() { clock.advance(Duration::from_secs(5)); }"
    ));
    assert!(!flagged(
        "fn a() { let end = start + Duration::from_secs(5); }"
    ));
    assert!(!flagged("fn a() { let wait = Duration::from_secs(5); }"));
}

#[test]
fn a_socket_timeout_setter_with_a_short_literal_is_flagged() {
    for call in [
        "tcp.set_read_timeout(Some(Duration::from_secs(2)))",
        "tcp.set_write_timeout(Some(Duration::from_secs(2)))",
        "stream\n        .set_read_timeout(Some(std::time::Duration::from_secs(5)))",
        "peer.set_write_timeout(Some(Duration::from_millis(1_000)))",
    ] {
        assert!(flagged(&format!("fn a() {{ {call}.unwrap(); }}")), "{call}");
    }
    assert_eq!(
        check("fn a() {\n    tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();\n}"),
        [
            "crates/x/src/a_tests.rs:2: set_write_timeout(Some(Duration::from_secs(2))) is below MUST_SUCCEED_WITHIN (10 s)"
        ]
    );
}

#[test]
fn a_socket_timeout_setter_with_another_argument_is_not_flagged() {
    for call in [
        "tcp.set_read_timeout(Some(MUST_SUCCEED_WITHIN))",
        "tcp.set_write_timeout(Some(Duration::from_secs(10)))",
        "tcp.set_read_timeout(Some(Duration::from_millis(200)))",
        "tcp.set_read_timeout(None)",
        "tcp.set_write_timeout(Some(deadline.left().max(Duration::from_millis(1))))",
        "tcp.set_read_timeout(Duration::from_secs(2))",
        "tcp.set_read_timeout(Other(Duration::from_secs(2)))",
    ] {
        assert!(
            !flagged(&format!("fn a() {{ {call}.unwrap(); }}")),
            "{call}"
        );
    }
}

#[test]
fn a_min_cap_with_a_short_literal_is_flagged() {
    assert!(flagged(
        "fn a() { deadline.left().min(Duration::from_secs(3)); }"
    ));
    assert!(flagged(
        "fn a() { let wait = deadline.left().min(Duration::from_secs(3)); Deadline::after(wait); }"
    ));
    assert_eq!(
        check("fn a() {\n    self.deadline.left().min(Duration::from_secs(5));\n}"),
        [
            "crates/x/src/a_tests.rs:2: .min(Duration::from_secs(5)) is below MUST_SUCCEED_WITHIN (10 s)"
        ]
    );
}

#[test]
fn a_min_with_another_argument_is_not_flagged() {
    assert!(!flagged(
        "fn a() { deadline.left().min(MUST_SUCCEED_WITHIN); }"
    ));
    assert!(!flagged(
        "fn a() { deadline.left().min(Duration::from_secs(10)); }"
    ));
    assert!(!flagged("fn a() { count.min(3); }"));
    assert!(!flagged("fn a() { min(Duration::from_secs(3)); }"));
}

#[test]
fn comments_and_strings_never_match() {
    assert!(!flagged(
        "// const W: Duration = Duration::from_secs(5);\nfn a() {}"
    ));
    assert!(!flagged(
        "/* Deadline::after(Duration::from_secs(5)) */ fn a() {}"
    ));
    assert!(!flagged(
        "fn a() { let s = \"const W: Duration = Duration::from_secs(5);\"; }"
    ));
    assert!(!flagged(
        "fn a() { let s = r#\"Deadline::after(Duration::from_secs(5))\"#; }"
    ));
}

#[test]
fn only_test_files_are_scanned() {
    let source = "const W: Duration = Duration::from_secs(5);";
    for rel in ["src/tests.rs", "src/a/b_tests.rs", "tests/c.rs"] {
        assert_eq!(must_succeed(&[file(rel, source)], &[]).len(), 1, "{rel}");
    }
    for rel in ["src/lib.rs", "src/tests_helper.rs"] {
        assert!(must_succeed(&[file(rel, source)], &[]).is_empty(), "{rel}");
    }
}

#[test]
fn failures_come_in_path_then_line_order() {
    let source =
        "const B: Duration = Duration::from_secs(5);\nconst A: Duration = Duration::from_secs(4);";
    let failures = must_succeed(
        &[
            file("src/z_tests.rs", source),
            file("src/a_tests.rs", source),
        ],
        &[],
    );
    let at: Vec<&str> = failures
        .iter()
        .map(|f| f.split(" is ").next().unwrap_or_default())
        .collect();
    assert_eq!(
        at,
        [
            "crates/x/src/a_tests.rs:1: const B",
            "crates/x/src/a_tests.rs:2: const A",
            "crates/x/src/z_tests.rs:1: const B",
            "crates/x/src/z_tests.rs:2: const A",
        ]
    );
}

const SHORT: &str = "const SHORT: Duration = Duration::from_secs(2);\nconst OTHER: Duration = Duration::from_secs(3);";

fn named(path: &'static str, name: &'static str) -> Allowance {
    kept(path, name, "nothing arrives")
}

#[test]
fn a_named_allowance_passes_its_own_file_and_name_only() {
    let allowances = [named("crates/x/src/a_tests.rs", "SHORT")];
    let failures = must_succeed(&[file("src/a_tests.rs", SHORT)], &allowances);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("const OTHER"), "{failures:?}");

    let elsewhere = [named("crates/x/src/b_tests.rs", "SHORT")];
    let failures = must_succeed(&[file("src/a_tests.rs", SHORT)], &elsewhere);
    assert_eq!(failures.len(), 2, "{failures:?}");
}

#[test]
fn a_whole_file_allowance_passes_every_site_in_its_file() {
    let allowances = [held("crates/x/src/a_tests.rs", "held by #1")];
    assert!(must_succeed(&[file("src/a_tests.rs", SHORT)], &allowances).is_empty());
    assert_eq!(
        must_succeed(&[file("src/b_tests.rs", SHORT)], &allowances).len(),
        2
    );
}

#[test]
fn a_named_allowance_that_matches_no_short_bound_is_stale() {
    let allowances = [named("crates/x/src/a_tests.rs", "GONE")];
    assert_eq!(
        must_succeed(&[file("src/a_tests.rs", "fn a() {}")], &allowances),
        ["allowance crates/x/src/a_tests.rs GONE (reason: nothing arrives) matches no short bound"]
    );
    let at_the_floor = "const GONE: Duration = Duration::from_secs(10);";
    assert_eq!(
        must_succeed(&[file("src/a_tests.rs", at_the_floor)], &allowances).len(),
        1,
        "a const at the floor is not a short bound, so its allowance is stale"
    );
}

#[test]
fn a_whole_file_allowance_that_matches_nothing_is_not_reported() {
    let allowances = [held("crates/x/src/a_tests.rs", "held by #1")];
    assert!(must_succeed(&[file("src/a_tests.rs", "fn a() {}")], &allowances).is_empty());
    assert!(must_succeed(&[], &allowances).is_empty());
}

#[test]
fn no_allowances_flags_every_site() {
    assert_eq!(must_succeed(&[file("src/a_tests.rs", SHORT)], &[]).len(), 2);
}

#[test]
fn a_test_file_that_does_not_tokenise_is_reported() {
    let failures = check("const W: Duration = \"unterminated;");
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with("crates/x/src/a_tests.rs: does not tokenise as Rust"),
        "{failures:?}"
    );
}

#[test]
fn the_floor_is_the_shared_bound() {
    assert_eq!(FLOOR, fakes::MUST_SUCCEED_WITHIN);
}

#[test]
fn a_wait_method_flags_its_second_argument_only() {
    for call in [
        "cv.wait_timeout(guard, Duration::from_secs(5))",
        "cv.wait_timeout_while(guard, Duration::from_secs(5), |_| false)",
    ] {
        assert!(flagged(&format!("fn a() {{ {call}; }}")), "{call}");
    }
    for call in [
        "cv.wait_timeout(guard, MUST_SUCCEED_WITHIN)",
        "cv.wait_timeout_while(guard, Duration::from_secs(10), |_| false)",
        "cv.wait_timeout(Duration::from_secs(5), guard)",
    ] {
        assert!(!flagged(&format!("fn a() {{ {call}; }}")), "{call}");
    }
}

#[test]
fn an_allowance_for_a_file_outside_the_scan_is_ignored() {
    let allowances = [named("crates/x/src/other_tests.rs", "SHORT")];
    let failures = must_succeed(&[file("src/a_tests.rs", SHORT)], &allowances);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures.iter().all(|f| !f.contains("allowance")),
        "{failures:?}"
    );
}
