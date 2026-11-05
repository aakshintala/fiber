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
fn identifier_edges_count_nothing_for_unicode_adjacency() {
    // The ticket's acceptance: "`recv_reply` has 0 uses." A Unicode
    // identifier character beside the needle (letter in any script) is
    // still a boundary, so neither adjacency counts.
    let cases: &[(&str, usize)] = &[
        ("    \u{3b1}recv_reply(&client, \"x\");\n", 0),
        ("    recv_reply\u{3b2}(&client, \"x\");\n", 0),
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
fn shape_renamed_helper_defect_counts() {
    // #4102's acceptance: "under any helper name". A hand-sent
    // `"subscribe"` command under a name other than `subscribe` or
    // `subscribe_full`, then the test's action, then a bare `until` for
    // the action's unfiltered `turn_completed` on a replay-exposed flow,
    // counts under the new check and holds nothing under the old one.
    let source = "fn send_sub(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_sub\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"subscribe\\\",\\\"args\\\":{{\\\"level\\\":\\\"full\\\"}}}}\"));\n}\nfn fire_prompt(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n}\nfn wait_turn(client: &Socket) {\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "renamed-helper defect");
}

#[test]
fn shape_cross_helper_defect_counts() {
    // #4102's acceptance: "under any helper name", with the triple in
    // file order in one file. The subscribe, the action and the wait
    // live in three helpers sharing one connection; the replay marker
    // stands between the subscribe and the wait.
    let source = "fn helper_sub(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n}\nfn helper_act(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n}\nfn helper_wait(client: &Socket) {\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "cross-helper defect");
}

#[test]
fn shape_two_defect_waits_count_twice() {
    // #4102's acceptance: one occurrence per stale wait. Two waits in
    // defect shape count two, so collapsing to a boolean fails.
    let source = "fn helper_sub(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n}\nfn helper_act(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n}\nfn helper_wait(client: &Socket) {\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n    until(&client, \"the resumed turn to complete again\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 2, "two defect waits");
}

#[test]
fn shape_aliased_subscribe_defect_counts() {
    // #4102's acceptance: "under any helper name", including a
    // `support::subscribe*` call renamed through `use ... as ...`.
    let source = "use support::{subscribe as my_sub};\nfn flow(client: &Socket, session: &str) {\n    my_sub(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "aliased subscribe defect");
}

#[test]
fn shape_renamed_wait_defect_counts() {
    // #4102's acceptance: "under any helper name" for the wait too: a
    // bare `until` renamed through `use ... as ...` waits the same way.
    let source = "use support::{until as wait_for};\nfn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    wait_for(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "renamed wait defect");
}

#[test]
fn shape_wrapped_wait_defect_counts() {
    // #4102's acceptance: "under any helper name", including a local
    // wrapper around the same unbounded wait, ordered after the action.
    let source = "fn helper_sub(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n}\nfn helper_act(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n}\nfn my_wait(client: &Socket) {\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "wrapped wait defect");
}

#[test]
fn shape_wrapped_action_defect_counts() {
    // #4102's shape is "a subscribe in this file, with a following
    // unbounded `until`": the test's action counts under any helper
    // name too, when the helper sends.
    let source = "fn helper_sub(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n}\nfn flow(client: &Socket, session: &str) {\n    fire_prompt(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\nfn fire_prompt(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n}\n";
    assert_eq!(count_stale(source), 1, "wrapped action defect");
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

#[test]
fn shape_fresh_and_misordered_flows_pass() {
    // The admission: a wait in a session that was never reopened is not
    // a defect, and the positive rule waits for an action's event only
    // after the action. None of these names a replay point between its
    // subscribe and its wait, or orders action between them.
    let fresh = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_prompt\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let action_before_subscribe = "fn flow(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_start\\\",\\\"command\\\":\\\"start\\\",\\\"args\\\":{{}}}}\"));\n    support::subscribe(&client, &session);\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let wait_before_action = "fn flow(client: &Socket, session: &str) {\n    support::subscribe_direct(&client);\n    until(&client, \"the first turn_completed\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n    client.send(\"{\\\"id\\\":\\\"c_notes\\\",\\\"command\\\":\\\"propose_notes\\\",\\\"args\\\":{}}\");\n}\n";
    let marker_on_wait_line_only = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_prompt\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (fresh, "fresh subscribe-action-turn"),
        (
            action_before_subscribe,
            "no action between subscribe and wait",
        ),
        (wait_before_action, "pre-action replay read"),
        (
            marker_on_wait_line_only,
            "marker only on the wait's own line",
        ),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn shape_legs_alone_pass() {
    // The shape needs all three legs: a subscribe with no wait, and a
    // wait with no subscribe, each hold nothing on their own.
    let subscribe_without_wait = "fn flow(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_sub\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"subscribe\\\",\\\"args\\\":{{\\\"level\\\":\\\"full\\\"}}}}\"));\n}\n";
    let wait_without_subscribe = "fn flow(client: &Socket) {\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (subscribe_without_wait, "subscribe without wait"),
        (wait_without_subscribe, "wait without subscribe"),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn shape_filtered_and_bound_waits_pass() {
    // The admission and the ruling: a wait carrying a reopen-derived
    // bound, or matching by command, turn or session, or delegating its
    // match to a helper, excludes the replay by predicate.
    let prefix = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n";
    let cases: &[(&str, &str)] = &[
        (
            "    let ack = recv_answer(&client, \"c_p2\", \"the prompt acknowledgement\");\n}\n",
            "ack-only",
        ),
        (
            "    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\" && line[\"payload\"][\"command_id\"] == \"c_p2\"\n    });\n}\n",
            "command-id-matched",
        ),
        (
            "    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\" && line[\"turn_id\"] == turn\n    });\n}\n",
            "turn-id-matched",
        ),
        (
            "    until(&client, \"the resumed run\", |line| {\n        line[\"kind\"] == \"fiber_started\" && line[\"payload\"][\"resumed\"] == true\n    });\n}\n",
            "resumed-filtered",
        ),
        (
            "    until(&client, \"the child turn to complete\", |line| {\n        line[\"session_id\"] == child && line[\"kind\"] == \"turn_completed\"\n    });\n}\n",
            "session-filtered",
        ),
        (
            "    until(&client, \"the turn to complete\", |line| {\n        is_session(line, &session) && line[\"kind\"] == \"turn_completed\"\n    });\n}\nfn is_session(line: &Value, id: &str) -> bool {\n    line.get(\"session_id\").and_then(Value::as_str) == Some(id)\n}\n",
            "helper-matched",
        ),
        (
            "    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"command_accepted\"\n    });\n}\n",
            "ack-kind",
        ),
        (
            "    client.send(&format!(\"{{\\\"id\\\":\\\"c_close\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"close\\\"}}\"));\n    until(&client, \"fiber_exited\", |line| {\n        line[\"kind\"] == \"fiber_exited\"\n    });\n}\n",
            "close wait",
        ),
        (
            "    let lines = until_after(&client, &sub, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n",
            "bound wait",
        ),
    ];
    for (tail, what) in cases {
        assert_eq!(count_stale(&format!("{prefix}{tail}")), 0, "{what}");
    }
}

#[test]
fn shape_bound_wait_under_renamed_helper_passes() {
    // The spelling-independence negative: a bound wait under a renamed
    // helper still carries its bound, so the replay cannot satisfy it.
    let source = "use support::{until_after as bounded};\nfn flow(client: &Socket, session: &str, sub: &Subscribed) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    bounded(&client, sub, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 0, "bound wait under renamed helper");
}

#[test]
fn shape_ephemeral_and_summary_pass() {
    // The contract: an ephemeral line is never in any fold, so waiting
    // for one cannot answer from the replay; a `summary` subscription
    // reads no log at all.
    let resumed_probe = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed probe\", |line| {\n        line[\"kind\"] == \"extension_ui\"\n    });\n}\n";
    let summary = "fn flow(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_sub\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"subscribe\\\",\\\"args\\\":{{\\\"level\\\":\\\"summary\\\"}}}}\"));\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (resumed_probe, "ephemeral-kind wait"),
        (summary, "summary subscribe"),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn shape_consumed_replay_passes() {
    // The core rule: a consuming wait positioned after the replay drains
    // it, through the resumed marker or through an ephemeral kind that
    // only arrives live, so the later event wait reads live lines.
    let marker_drain = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed fiber_started\", |line| {\n        line[\"kind\"] == \"fiber_started\" && line[\"payload\"][\"resumed\"] == true\n    });\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let ephemeral_drain = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    until(&client, \"the resumed run\", |line| {\n        line[\"kind\"] == \"session_status\"\n    });\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (marker_drain, "drain through resumed marker"),
        (ephemeral_drain, "drain through ephemeral kind"),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn shape_constructors_pass() {
    // The extra ruling: the four command constructors send the
    // `"subscribe"` command string but start no wait, so each body
    // holds no stale-wait occurrence without an allowance entry.
    let cases: &[(&str, &str)] = &[
        (
            "fn subscribe_line(id: &str, session: &str) -> String {\n    command(id, Some(session), \"subscribe\", json!({\"level\": \"full\"}))\n}\n",
            "change_workspace and rewind constructors",
        ),
        (
            "fn subscribe_line(id: &str, session: &str) -> Value {\n    command(id, session, \"subscribe\", json!({\"level\": \"full\"}))\n}\n",
            "hub_resume and tool_timing constructors",
        ),
        (
            "fn subscribe_line(id: &str, session: &str) -> String {\n    command(id, Some(session), \"subscribe\", json!({\"level\": \"full\"}))\n}\nfn prompt(id: &str, session: &str, text: &str) -> String {\n    command(\n        id,\n        Some(session),\n        \"prompt\",\n        json!({\"content\": [{\"type\": \"text\", \"text\": text}]}),\n    )\n}\n",
            "constructor beside a prompt builder",
        ),
        (
            "fn send_sub(client: &Socket, session: &str) {\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_sub\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"subscribe\\\",\\\"args\\\":{{\\\"level\\\":\\\"full\\\"}}}}\"));\n}\n",
            "hand-sent subscribe without wait",
        ),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn shape_reset_and_other_socket_pass() {
    // A fresh session start between the legs breaks the chain: the
    // later wait is on a session with no pre-subscribe history. A wait
    // on another connection reads that connection's stream, never the
    // subscribed one's replay.
    let fresh_start_between = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    let session = start_session(&client, &workspace, \"again\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let other_socket = "fn flow(client: &Socket, control: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&control, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (fresh_start_between, "fresh start between the legs"),
        (other_socket, "wait on another connection"),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn shape_scope_and_comments_pass() {
    // The scope never changes: the driver and other crates stay out,
    // and a defect shape inside line comments never counts. A file the
    // table does not name holds none, so a shape defect fails at zero.
    let defect = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{{}}}}\"));\n    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let commented: String = defect.lines().map(|line| format!("// {line}\n")).collect();
    assert_eq!(count_stale(&commented), 0, "commented defect");
    let files = [
        file("crates/main/tests/support/flow.rs", defect),
        file("crates/doors/tests/a.rs", defect),
    ];
    assert!(check(&files, &maxima(&[])).is_empty(), "scope exclusions");
    let files = [file("crates/main/tests/flow.rs", defect)];
    assert_eq!(
        check(&files, &maxima(&[])),
        ["crates/main/tests/flow.rs holds 1 stale-wait occurrences (maximum 0)"]
    );
}

#[test]
fn shape_comment_markers_establish_no_replay() {
    // ADMIT-4102's fresh-session exclusion: replay exposure comes from
    // executable flow, so a comment naming no reopening counts nothing,
    // even beside a subscribe, an action and an unbounded wait.
    let source = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    // no reopen here\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{}}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 0, "comment-only replay marker");
}

#[test]
fn shape_other_socket_bound_wait_drains_nothing() {
    // A bounded wait on another connection reads that connection's
    // stream, so the subscribed replay stays unread for the later wait.
    let prefix = "fn flow(client: &Socket, control: &Socket, session: &str, sub: &Subscribed) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{}}}\"));\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n";
    let tail = "    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (
            "    until_after(&control, &sub, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n",
            "other-socket until_after",
        ),
        (
            "    until_after_reopen(&control, &sub, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n",
            "other-socket until_after_reopen",
        ),
        ("    until_close(&control);\n", "other-socket until_close"),
    ];
    for (drain, what) in cases {
        assert_eq!(count_stale(&format!("{prefix}{drain}{tail}")), 1, "{what}");
    }
    // The same bound wait on the subscribed connection drains the
    // replay, including when the call spans lines.
    let same = "    until_after(&client, &sub, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n";
    assert_eq!(
        count_stale(&format!("{prefix}{same}{tail}")),
        0,
        "same-socket until_after"
    );
    let split = "    support::until(\n        &client,\n        \"the resumed fiber_started\",\n        |line| line[\"kind\"] == \"fiber_started\" && line[\"payload\"][\"resumed\"] == true,\n    );\n";
    assert_eq!(
        count_stale(&format!("{prefix}{split}{tail}")),
        0,
        "multiline same-socket drain"
    );
}

#[test]
fn shape_named_drain_reasons_pass() {
    // Each drain reason stands alone: a wait matching only through one
    // disjunct still consumes the replay, so dropping that disjunct
    // would count a stale wait here.
    let prefix = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{}}}\"));\n    let resumed_ack = recv_answer(&client, \"c_p2\", \"the resumed prompt acknowledgement\");\n";
    let tail = "    until(&client, \"the resumed turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (
            "    until(&client, \"the stream_reopened line\", |line| {\n        done(line)\n    });\n",
            "stream_reopened-only drain",
        ),
        (
            "    until(&client, \"the new session\", |line| {\n        line[\"payload\"][\"session_resumed\"] == true\n    });\n",
            "session_resumed-only drain",
        ),
        (
            "    until_resumed(&client, \"the marker\");\n",
            "until_resumed-only drain",
        ),
        (
            "    until(&client, \"the accepted turn\", |line| {\n        line[\"kind\"] == \"turn_completed\" && line[\"payload\"][\"command_id\"] == \"c_p2\"\n    });\n",
            "command_id-only drain",
        ),
    ];
    for (drain, what) in cases {
        assert_eq!(count_stale(&format!("{prefix}{drain}{tail}")), 0, "{what}");
    }
}

#[test]
fn shape_rewind_only_defect_counts() {
    // Each replay spelling stands alone: a flow replayed only through a
    // `rewind` command still counts, so dropping that disjunct would
    // miss a stale wait here.
    let source = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_rew\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"rewind\\\",\\\"args\\\":{}}}\"));\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{}}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "rewind-only replay");
}

#[test]
fn shape_definitions_and_imports_establish_no_replay() {
    // Like comments, a function definition and a `use` line never replay
    // anything, even when they name a resume: only executed lines do.
    let defined = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{}}}\"));\n    fn resume_helper() {}\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let imported = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    use support::{until_after_reopen};\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\",\\\"args\\\":{}}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    let cases: &[(&str, &str)] = &[
        (defined, "nested definition names a resume"),
        (imported, "import names a reopen"),
    ];
    for (source, what) in cases {
        assert_eq!(count_stale(source), 0, "{what}");
    }
}

#[test]
fn word_edges_span_scripts() {
    // A multibyte letter counts beside the needle in any script; a stray
    // continuation byte, a past-end index or a non-word byte never does.
    assert!(is_word_at("αrecv".as_bytes(), 0));
    assert!(is_word_at(&[0xE1, 0x80, 0x80], 1));
    assert!(!is_word_at(b"\x80", 0));
    assert!(!is_word_at(b"a", 1));
    assert!(!is_word_at(b"a b", 1));
    assert!(is_word_at(b"_", 0));
    assert!(is_word_at(b"9", 0));
    assert!(!is_word_at("…x".as_bytes(), 0));
    // A trailing invalid byte never promotes an ASCII letter, and a
    // continuation byte still asks the lead covering it.
    assert!(is_word_at(b"A\xff", 0));
    assert!(is_word_at("\u{400}".as_bytes(), 1));
}

#[test]
fn identifier_starts_need_word_edges() {
    // A match counts only between non-identifier bytes; a longer name on
    // either side never counts.
    assert_eq!(ident_ats("until(&client", "until"), vec![0]);
    assert_eq!(ident_ats("xuntil(&client", "until"), Vec::<usize>::new());
    assert_eq!(ident_ats("untilx(&client", "until"), Vec::<usize>::new());
    assert_eq!(ident_ats("a until b until", "until"), vec![2, 10]);
}

#[test]
fn function_names_parse_through_qualifiers() {
    // An optional `pub(...)`, `pub`, `async` or indent prefix still names
    // the function, while a keyword glued to an identifier never does.
    assert_eq!(fn_name("fn flow(client: &Socket) {"), Some("flow"));
    assert_eq!(fn_name("    fn indented() {}"), Some("indented"));
    assert_eq!(fn_name("pub(crate) fn q() {}"), Some("q"));
    assert_eq!(fn_name("pub fn p() {}"), Some("p"));
    assert_eq!(fn_name("async fn a() {}"), Some("a"));
    assert_eq!(fn_name("pubfn f() {}"), None);
    assert_eq!(fn_name("fnx foo()"), None);
    assert_eq!(fn_name("fn9()"), None);
    assert_eq!(fn_name("let x = 1;"), None);
}

#[test]
fn top_level_definitions_need_no_indent() {
    // Only an unindented definition opens a top-level function; an
    // indented one or a non-definition line never does.
    assert!(is_top_fn_def("fn f() {}"));
    assert!(!is_top_fn_def("    fn f() {}"));
    assert!(!is_top_fn_def("let x = 1;"));
}

#[test]
fn use_lines_name_their_keyword() {
    // Both `use` spellings count, even indented; anything else never does.
    assert!(is_use_line("use support::x;"));
    assert!(is_use_line("    use support::x;"));
    assert!(is_use_line("pub use a::b;"));
    assert!(!is_use_line("fn f() {}"));
    assert!(!is_use_line("usex;"));
    assert!(!is_use_line("// use a;"));
}

#[test]
fn first_arguments_stop_at_top_level() {
    // The argument ends at the first top-level comma or closer; nesting,
    // strings, raw strings and characters never end it early.
    assert_eq!(first_argument_from("&client, \"w\"", 0), "&client");
    assert_eq!(first_argument_from("&client)", 0), "&client");
    assert_eq!(first_argument_from("f(a, b), c)", 0), "f(a, b)");
    assert_eq!(first_argument_from("\"a)b\", c)", 0), "\"a)b\"");
    assert_eq!(first_argument_from("r#\"a)b\"#, c)", 0), "r#\"a)b\"#");
    assert_eq!(first_argument_from("r#\"a\"b\"#, c)", 0), "r#\"a\"b\"#");
    assert_eq!(first_argument_from("r\"a(b\", c)", 0), "r\"a(b\"");
    assert_eq!(first_argument_from("r##\"a)b\"##, c)", 0), "r##\"a)b\"##");
    assert_eq!(first_argument_from("r#\"a##b\"#, c)", 0), "r#\"a##b\"#");
    assert_eq!(first_argument_from("'(', c)", 0), "'('");
    assert_eq!(first_argument_from("'\\', c)", 0), "'\\'");
    assert_eq!(first_argument_from("&client", 0), "&client");
    assert_eq!(first_argument_from("", 0), "");
    assert_eq!(first_argument("until   (&client)", 0, 5), "&client");
    assert_eq!(first_argument("until &client)", 0, 5), "");
    assert_eq!(region_first_argument("(&client, x)"), "&client");
    assert_eq!(region_first_argument(""), "");
}

#[test]
fn borrows_strip_to_the_stream() {
    // Receivers shed `&`, `&mut` and padding, so kind-matched waits and
    // sends compare the same stream name.
    assert_eq!(strip_borrow("&client"), "client");
    assert_eq!(strip_borrow("&&mut client"), "client");
    assert_eq!(strip_borrow("  &mut  client  "), "client");
    assert_eq!(strip_borrow("client"), "client");
    assert_eq!(strip_borrow(""), "");
}

#[test]
fn sockets_read_receivers_then_first_arguments() {
    // A method call names its receiver; a helper call names its first
    // argument with borrows stripped.
    assert_eq!(socket_of_call("    client.send(x)", 11, 4), "client");
    assert_eq!(socket_of_call("    client. send(x)", 12, 4), "client");
    // A separator directly against the receiver still ends the run of
    // blanks: the name starts past it.
    assert_eq!(socket_of_call("(client.send(x)", 8, 4), "client");
    assert_eq!(socket_of_call("    until(&client, \"w\")", 4, 5), "client");
    assert_eq!(
        socket_of_call("    until(&mut client, \"w\")", 4, 5),
        "client"
    );
    assert_eq!(socket_of_call(".send(x)", 1, 4), "");
}

#[test]
fn regions_span_the_balanced_group() {
    // A single-line call stays on its line; a spanning call joins its
    // lines, and a call without a paren yields nothing.
    assert_eq!(
        call_region(&["    until(&client, \"w\", |line| true);"], 0, 4),
        "(&client, \"w\", |line| true)"
    );
    assert_eq!(
        call_region(
            &[
                "    support::until(",
                "        &client,",
                "        \"w\",",
                "    );"
            ],
            0,
            13
        ),
        "(\n        &client,\n        \"w\",\n    )"
    );
    // A closer before the call never moves the scan: the region still
    // starts at the call's own paren.
    assert_eq!(call_region(&["f(x) + g(y)"], 0, 7), "(y)");
    assert_eq!(call_region(&["    until &client;"], 0, 4), "");
    assert_eq!(call_region(&["no call here"], 0, 0), "");
}

#[test]
fn bodies_collect_helper_sends() {
    // Back-to-back helpers each keep their exact body, a one-line body
    // counts whole, and a helper after a blank line is a body too; a
    // nested definition inside a body never becomes one, and lines
    // outside any body never count.
    let lines = [
        "fn a() {",
        "    client.send(\"x\");",
        "    fn nested() {}",
        "}",
        "fn b() {",
        "    client.send(\"y\");",
        "}",
        "",
        "fn c() { client.send(\"z\"); }",
        "let x = 1;",
    ];
    let bodies = fn_bodies(&lines);
    assert_eq!(
        bodies.get("a").map(String::as_str),
        Some("fn a() {\n    client.send(\"x\");\n    fn nested() {}\n}\n")
    );
    assert_eq!(
        bodies.get("b").map(String::as_str),
        Some("fn b() {\n    client.send(\"y\");\n}\n")
    );
    assert_eq!(
        bodies.get("c").map(String::as_str),
        Some("fn c() { client.send(\"z\"); }\n")
    );
    assert_eq!(bodies.get("nested"), None);
    assert_eq!(bodies.get("x"), None);
    // An unclosed body swallows a trailing definition: with no closing
    // line the scan never resumes past the fallthrough line.
    let open = ["fn a() {", "    client.send(\"x\");", "fn b() {"];
    let open_bodies = fn_bodies(&open);
    assert!(open_bodies.get("a").is_some());
    assert_eq!(open_bodies.get("b"), None);
}

#[test]
fn kinds_come_from_kind_predicates() {
    // Only `line["kind"] == "..."` predicates name kinds, in order.
    let region = "(&client, \"w\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });";
    assert_eq!(wait_kinds(region), vec!["turn_completed".to_owned()]);
    let two = "(&client, \"w\", |line| line[\"kind\"] == \"a\" || line[\"kind\"] == \"b\")";
    assert_eq!(wait_kinds(two), vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(wait_kinds("(&client)"), Vec::<String>::new());
    // An empty kind never counts, even with its closing quote present.
    let empty = "(&client, \"w\", |line| line[\"kind\"] == \"\")";
    assert_eq!(wait_kinds(empty), Vec::<String>::new());
}

#[test]
fn delegation_needs_a_line_taking_helper() {
    // A helper call taking `line` delegates; a bare predicate, a quoted
    // "line", an opaque helper, a method call or a missing `line` never
    // does.
    assert!(delegates_match("(is_session(line, &session))"));
    assert!(delegates_match("(is_session( line, &session))"));
    assert!(!delegates_match("(line[\"kind\"] == \"x\")"));
    assert!(!delegates_match("(\"line\")"));
    assert!(!delegates_match("(helper(noline))"));
    assert!(!delegates_match("(if(line))"));
    assert!(!delegates_match("(client.is_session(line))"));
}

#[test]
fn wire_sockets_resolve_through_the_enclosing_send() {
    // A same-line sender wins; otherwise the enclosing top-level send
    // above the wire line names the socket, and a send in another
    // function never does.
    let direct = ["    client.send(&format!(\"subscribe {}\"));"];
    assert_eq!(wire_socket(&direct, 0), Some("client".to_owned()));
    let covered = [
        "fn flow(client: &Socket, session: &str) {",
        "    client.send(&format!(\"{{\\\"id\\\":\\\"c_sub\\\"\",",
        "        \"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"subscribe\\\"}}));",
        "}",
    ];
    assert_eq!(wire_socket(&covered, 2), Some("client".to_owned()));
    let bare = [
        "fn helper(control: &Socket) {",
        "    control.send(&format!(\"x\"));",
        "}",
        "fn flow(client: &Socket, session: &str) {",
        "    let payload = String::from(\"{\\\"command\\\":\\\"subscribe\\\"}\");",
        "}",
    ];
    assert_eq!(wire_socket(&bare, 4), None);
    // A comment naming a send never resolves the socket: the walk skips
    // it and keeps the enclosing send above it.
    let commented = [
        "fn flow(client: &Socket, session: &str) {",
        "    client.send(&format!(\"{{\\\"id\\\":\\\"c_sub\\\",",
        "    // client.send(\"old\");",
        "        \"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"subscribe\\\"}}));",
    ];
    assert_eq!(wire_socket(&commented, 3), Some("client".to_owned()));
    // A wire line strictly inside the send region resolves too: the
    // cover only needs to reach it, not end on it.
    let inside = [
        "fn flow(client: &Socket, session: &str) {",
        "    client.send(&format!(\"{{}}\",",
        "        \"session_id {session} command subscribe\",",
        "        \"extra arg\"));",
    ];
    assert_eq!(wire_socket(&inside, 2), Some("client".to_owned()));
}

#[test]
fn group_ends_span_lines_and_kinds() {
    // Every bracket kind opens depth, and the scan stops 200 lines past
    // its start when nothing closes the group.
    assert_eq!(scan_group_end(&["f(a, b)"], 0, 1, false), (0, 6));
    assert_eq!(scan_group_end(&["f(a, [b, {c}])"], 0, 1, false), (0, 13));
    // A closer with depth still open never ends the scan, at any depth.
    assert_eq!(scan_group_end(&["f((x))"], 0, 1, false), (0, 5));
    assert_eq!(scan_group_end(&["f((x)", "y))"], 0, 1, false), (1, 1));
    let open: Vec<&str> = vec!["f("];
    let mut tall = open;
    tall.extend(std::iter::repeat_n("(", 201));
    assert_eq!(scan_group_end(&tall, 0, 1, false), (tall.len() - 1, 0));
}

#[test]
fn group_comments_hide_brackets() {
    // A block comment hides brackets until its closer; a line comment
    // ends the scan of its line.
    assert_eq!(scan_group_end(&["/* ( */ ( )"], 0, 0, false), (0, 10));
    assert_eq!(scan_group_end(&["/* * x */ ( )"], 0, 0, false), (0, 12));
    assert_eq!(scan_group_end(&["/*x*//(Y)"], 0, 0, false), (0, 8));
    assert_eq!(scan_group_end(&["f(a // )", "b)"], 0, 1, false), (1, 1));
    assert_eq!(scan_group_end(&["f(a/b)"], 0, 1, false), (0, 5));
}

#[test]
fn group_strings_hide_brackets() {
    // A quoted, raw or byte string hides brackets; only the raw closer
    // with its hashes ends a raw string.
    assert_eq!(scan_group_end(&["f(\"a)b\", c)"], 0, 1, false), (0, 10));
    assert_eq!(scan_group_end(&["f(r#\"a)b\"#, c)"], 0, 1, false), (0, 13));
    assert_eq!(scan_group_end(&["f(r#\"a\"b\"#, c)"], 0, 1, false), (0, 13));
    // Content starting with the closer's hashes never ends the raw
    // string early: the opener skips its quote and hashes together.
    assert_eq!(scan_group_end(&["f(r#\"#x\"#, y)"], 0, 1, false), (0, 12));
    assert_eq!(scan_group_end(&["f(b\"a)b\", c)"], 0, 1, false), (0, 11));
    // An escaped quote never closes the string: the group runs past it
    // to the end of the line.
    assert_eq!(scan_group_end(&["f(\"a\\\\\"b\", c)"], 0, 1, false), (0, 0));
    // A double-hash raw string closes only on its own quote.
    assert_eq!(scan_group_end(&["f(r##\"a\"##, c)"], 0, 1, false), (0, 13));
}

#[test]
fn group_chars_hide_brackets() {
    // A plain or escaped character hides its bracket; a lone quote is
    // no character at all.
    assert_eq!(scan_group_end(&["f('(', c)"], 0, 1, false), (0, 8));
    assert_eq!(scan_group_end(&["f('\\', c)"], 0, 1, false), (0, 8));
    assert_eq!(scan_group_end(&["f(')"], 0, 1, false), (0, 3));
    // A quote glued to an identifier is no character, so the bracket
    // after it still opens depth and the group never closes.
    assert_eq!(scan_group_end(&["f('x(', c)"], 0, 1, false), (0, 0));
    // A character past the first columns still skips exactly itself: a
    // wider jump would land past the bracket it hides.
    assert_eq!(scan_group_end(&["abc('x'(, c)"], 0, 3, false), (0, 0));
}

#[test]
fn group_closers_need_an_opener() {
    // A closer with no opener and no start never ends the scan; the
    // search falls off the end instead.
    assert_eq!(scan_group_end(&[")", "x"], 0, 0, false), (1, 0));
    assert_eq!(scan_group_end(&["]", "x"], 0, 0, false), (1, 0));
    assert_eq!(scan_group_end(&["}", "x"], 0, 0, false), (1, 0));
    assert_eq!(scan_group_end(&["(", "x)"], 0, 0, false), (1, 1));
    assert_eq!(scan_group_end(&["[", "x]"], 0, 0, false), (1, 1));
    assert_eq!(scan_group_end(&["fn f() {", "}"], 0, 0, true), (1, 1));
}

#[test]
fn group_block_comments_hide_brackets() {
    // A block comment hides brackets while it runs; the closer after it
    // still ends the group. A group that never closes falls off the end
    // instead of hanging the scan.
    assert_eq!(scan_group_end(&["f(/*(*/ x)"], 0, 1, false), (0, 9));
    assert_eq!(scan_group_end(&["f(x"], 0, 1, false), (0, 0));
    // Adjacent block comments chain: the closer's slash never opens the
    // next one, and an unclosed comment hides the closer for good.
    assert_eq!(scan_group_end(&["/**//**/ ( )"], 0, 0, false), (0, 11));
    assert_eq!(scan_group_end(&["f(/*/ )"], 0, 0, false), (0, 0));
}

#[test]
fn drains_read_command_markers() {
    // A wait naming a command marker drains the same socket's replay,
    // with either marker spelling; a wait naming neither never does.
    let waited = "    until(&client, \"w\", |line| line[\"command_id\"] == cid);";
    assert!(is_drain(&[waited], 0, &Aliases::default(), "client"));
    let accepted = "    until(&client, \"w\", |line| line[\"command_accepted\"]);";
    assert!(is_drain(&[accepted], 0, &Aliases::default(), "client"));
    let plain = "    until(&client, \"w\", |line| line[\"kind\"] == \"turn_completed\");";
    assert!(!is_drain(&[plain], 0, &Aliases::default(), "client"));
}

#[test]
fn resets_find_fresh_starts() {
    // A fresh helper start and the wire `start` command both reset the
    // session; a `--resume` start never does.
    let lines = [
        "fn flow(client: &Socket, session: &str) {",
        "    start_session(\"fresh\");",
        "    start_session(\"old --resume\");",
        "    client.send(&format!(\"{{\\\"command\\\":\\\"start\\\"}}\"));",
    ];
    assert_eq!(reset_lines(&lines), vec![1, 3]);
}

#[test]
fn aliases_rename_each_flow_call() {
    // Every `ORIG as NAME` pair lands in its own set, wherever it sits
    // in the statement; a statement without `as` renames nothing.
    let lines = [
        "use support::{until as wait_until, subscribe as sub};",
        "use support::send as emit;",
        "use support::{x, y};",
    ];
    let aliases = use_aliases(&lines);
    assert_eq!(
        aliases.until,
        std::collections::BTreeSet::from(["wait_until".to_owned()])
    );
    assert_eq!(
        aliases.subscribe,
        std::collections::BTreeSet::from(["sub".to_owned()])
    );
    assert_eq!(
        aliases.send,
        std::collections::BTreeSet::from(["emit".to_owned()])
    );
}

#[test]
fn shape_reset_on_and_before_subscribe_passes() {
    // A reset on the subscribe line itself is a fresh start on that
    // same line, not a boundary before it, and a reset further above
    // predates the subscribe: neither breaks the chain, so the wait
    // still counts. Dropping either side of the guard would skip it.
    let source = "fn flow(client: &Socket, session: &str) {\n    start_session(\"s0\");\n    start_session(\"s1\"); subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_rew\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"rewind\\\"}}\"));\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\"}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "resets around the subscribe");
}

#[test]
fn shape_action_before_subscribe_passes() {
    // An action before the subscribe is outside the pair: the later
    // action still completes it, so the wait still counts. Only an
    // action strictly between the legs completes it.
    let source = "fn flow(client: &Socket, session: &str) {\n    client.send(&format!(\"early\"));\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_rew\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"rewind\\\"}}\"));\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\"}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "early action");
}

#[test]
fn shape_only_early_action_passes() {
    // With the only action before the subscribe, no action sits between
    // the legs, so the wait never counts. Treating any action anywhere
    // as completing the pair would count it: the replay leg here runs
    // through a helper call, never a send.
    let source = "fn flow(client: &Socket, session: &str) {\n    client.send(&format!(\"early\"));\n    support::subscribe(&client, &session);\n    let _ = rewind(&client);\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 0, "only an early action");
}

#[test]
fn shape_level_tagged_subscribe_counts() {
    // The summary exclusion needs both words: a subscribe tagged only
    // with a level still subscribes, so the wait still counts.
    // Loosening the guard to either word would skip it.
    let source = "fn flow(client: &Socket, session: &str) {\n    subscribe_line(&client, \"level\");\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_rew\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"rewind\\\"}}\"));\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\"}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "level-tagged subscribe");
}

#[test]
fn shape_reopen_observing_wait_on_same_socket_counts() {
    // A wait that also observes the reopened stream still waits on the
    // replay: only a drain strictly before it breaks the chain, so it
    // counts. Extending the drain scan onto the wait itself would find
    // the wait's own reopen marker and skip it.
    let source = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_rew\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"rewind\\\"}}\"));\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\"}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\" && stream_reopened\n    });\n}\n";
    assert_eq!(count_stale(source), 1, "same-socket reopen-observing wait");
}

#[test]
fn shape_wire_start_between_passes() {
    // The wire `start` command between the legs breaks the chain like a
    // fresh helper start: the later wait is on a session with no
    // pre-subscribe history.
    let source = "fn flow(client: &Socket, session: &str) {\n    support::subscribe(&client, &session);\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_rew\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"rewind\\\"}}\"));\n    client.send(&format!(\"{{\\\"command\\\":\\\"start\\\"}}\"));\n    client.send(&format!(\"{{\\\"id\\\":\\\"c_p2\\\",\\\"session_id\\\":\\\"{session}\\\",\\\"command\\\":\\\"prompt\\\"}}\"));\n    until(&client, \"the turn to complete\", |line| {\n        line[\"kind\"] == \"turn_completed\"\n    });\n}\n";
    assert_eq!(count_stale(source), 0, "wire start between the legs");
}
