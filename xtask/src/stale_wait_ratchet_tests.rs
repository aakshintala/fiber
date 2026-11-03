use super::*;
use std::collections::BTreeMap;

// Rule sources: #3664's acceptance ("`recv_reply` has 0 uses. No local
// `fn subscribe` remains in `crates/main/tests`.") requires the check, and
// the ticket's acceptance ("The doc line, the shared helper and the check,
// with a test that the check fails on a new `recv_reply`.") requires the
// failing test; every count asserted below cites one of those sentences,
// and the `stale_wait_ratchet` module doc enforces the same rule.

fn file(path: &str, source: &str) -> RustFile {
    RustFile {
        krate: "main".to_owned(),
        path: path.to_owned(),
        rel: path.strip_prefix("crates/main/").unwrap_or(path).to_owned(),
        source: source.to_owned(),
    }
}

fn maxima(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
    entries
        .iter()
        .map(|(path, maximum)| ((*path).to_owned(), *maximum))
        .collect()
}

#[test]
fn a_new_recv_reply_call_fails_at_maximum_zero() {
    // The ticket's acceptance: "with a test that the check fails on a new
    // `recv_reply`".
    let files = [file(
        "crates/main/tests/a.rs",
        "    let line = recv_reply(&client, \"x\");\n",
    )];
    assert_eq!(
        check(&files, &maxima(&[])),
        ["crates/main/tests/a.rs holds 1 stale-wait occurrences (maximum 0)"]
    );
}

#[test]
fn a_file_passes_below_and_at_its_maximum_and_fails_above() {
    // The ticket's acceptance: "`recv_reply` has 0 uses." A file holding
    // 1 or 2 occurrences against a maximum of 2 passes, killing a `>=`
    // mutant; 3 fails, killing a `==` mutant.
    let one = "    let line = recv_reply(&client, \"x\");\n";
    let two = "    let a = recv_reply(&client, \"x\");\n    let b = recv_reply(&client, \"y\");\n";
    let three = "    let a = recv_reply(&client, \"x\");\n    let b = recv_reply(&client, \"y\");\n    let c = recv_reply(&client, \"z\");\n";
    assert!(
        check(
            &[file("crates/main/tests/a.rs", one)],
            &maxima(&[("crates/main/tests/a.rs", 2)])
        )
        .is_empty()
    );
    assert!(
        check(
            &[file("crates/main/tests/a.rs", two)],
            &maxima(&[("crates/main/tests/a.rs", 2)])
        )
        .is_empty()
    );
    assert_eq!(
        check(
            &[file("crates/main/tests/a.rs", three)],
            &maxima(&[("crates/main/tests/a.rs", 2)])
        ),
        ["crates/main/tests/a.rs holds 3 stale-wait occurrences (maximum 2)"]
    );
}

#[test]
fn two_occurrences_on_one_line_count_twice() {
    // The ticket's acceptance: "`recv_reply` has 0 uses." Both calls on
    // the line count, so a maximum of 1 still fails.
    let files = [file(
        "crates/main/tests/a.rs",
        "    let a = recv_reply(&x, \"x\"); let b = recv_reply(&y, \"y\");\n",
    )];
    assert_eq!(
        check(&files, &maxima(&[("crates/main/tests/a.rs", 1)])),
        ["crates/main/tests/a.rs holds 2 stale-wait occurrences (maximum 1)"]
    );
}

#[test]
fn identifier_edges_count_only_whole_recv_reply() {
    // The ticket's acceptance: "`recv_reply` has 0 uses." A longer
    // identifier never counts, killing a substring mutant; a use after
    // `(`, `,` or `{` does.
    let cases: &[(&str, usize)] = &[
        ("    recv_reply_x(&client, \"x\");\n", 0),
        ("    my_recv_reply(&client, \"x\");\n", 0),
        ("    recv_replyx(&client, \"x\");\n", 0),
        ("    (recv_reply(&client, \"x\"))\n", 1),
        ("    foo,recv_reply(&client, \"x\")\n", 1),
        ("    use support::{recv_reply, x};\n", 1),
        ("    let line = support::recv_reply(client, \"x\");\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_stale(source), *expected, "source: {source:?}");
    }
}

#[test]
fn a_multiline_use_import_counts_its_recv_reply_line() {
    // The ticket's acceptance: "`recv_reply` has 0 uses." The import's
    // own line counts even when the `use` spans lines.
    let files = [file(
        "crates/main/tests/a.rs",
        "use support::{\n    recv_reply,\n    x,\n};\n",
    )];
    assert_eq!(
        check(&files, &maxima(&[])),
        ["crates/main/tests/a.rs holds 1 stale-wait occurrences (maximum 0)"]
    );
}

#[test]
fn a_comment_line_counts_nothing() {
    // The ticket's acceptance: "No local `fn subscribe` remains in
    // `crates/main/tests`." A line comment and a doc-comment line never
    // count.
    let cases: &[(&str, usize)] = &[
        ("    // recv_reply(&client, \"x\");\n", 0),
        ("    /// [`recv_reply`] drops attention lines.\n", 0),
        ("//fn subscribe(client: &Socket, session: &str)\n", 0),
    ];
    for (source, expected) in cases {
        assert_eq!(count_stale(source), *expected, "source: {source:?}");
    }
}

#[test]
fn subscribe_text_counts_definitions_not_calls_or_renames() {
    // The ticket's acceptance: "No local `fn subscribe` remains in
    // `crates/main/tests`." A renamed builder and a call never count,
    // killing a `subscribe(`-substring mutant; a qualified definition and
    // a `subscribe_full` definition do.
    let cases: &[(&str, usize)] = &[
        ("    fn subscribe_line(id: &str) -> String {\n", 0),
        ("    subscribe(&client, \"s\");\n", 0),
        (
            "    pub(crate) fn subscribe(client: &Socket, session: &str) {\n",
            1,
        ),
        ("    fn subscribe_full(direct: &Socket) {\n", 1),
        ("    mod inner {\n        fn subscribe() {}\n    }\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_stale(source), *expected, "source: {source:?}");
    }
}

#[test]
fn support_and_other_crates_are_outside_the_rule() {
    // The ticket's acceptance: "No local `fn subscribe` remains in
    // `crates/main/tests`." The driver and its helpers live under
    // `support/`; other crates' tests are not this rule's scope.
    let files = [
        file(
            "crates/main/tests/support/mod.rs",
            "pub(crate) fn recv_reply(client: &Socket, what: &str) -> Value {\n    fn subscribe(client: &Socket, session: &str) {}\n}\n",
        ),
        file(
            "crates/doors/tests/a.rs",
            "    let line = recv_reply(&client, \"x\");\n",
        ),
    ];
    assert!(check(&files, &maxima(&[])).is_empty());
}

#[test]
fn a_file_missing_from_the_table_holds_zero() {
    // The ticket's acceptance: "No local `fn subscribe` remains in
    // `crates/main/tests`." A file the table does not name holds none,
    // so a re-added copy fails.
    let files = [file(
        "crates/main/tests/fresh.rs",
        "    fn subscribe_full(direct: &Socket) {}\n",
    )];
    assert_eq!(
        check(&files, &maxima(&[])),
        ["crates/main/tests/fresh.rs holds 1 stale-wait occurrences (maximum 0)"]
    );
}
