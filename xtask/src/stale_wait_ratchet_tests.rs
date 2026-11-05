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
    let split = "    until(\n        &client,\n        \"the resumed fiber_started\",\n        |line| line[\"kind\"] == \"fiber_started\" && line[\"payload\"][\"resumed\"] == true,\n    );\n";
    assert_eq!(
        count_stale(&format!("{prefix}{split}{tail}")),
        0,
        "multiline same-socket drain"
    );
}
