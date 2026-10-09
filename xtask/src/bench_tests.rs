use serde_json::{Value, json};

use super::*;

const DOC: &str = concat!(
    "# Performance\n\n## Budgets\n\n",
    "| Budget | Ceiling | Gated on | Basis |\n",
    "|---|---|---|---|\n",
    "| Session, idle, headless | 12 MiB peak RSS | Linux x86_64 | picked |\n",
    "| Terminal, idle | 9,048 KiB peak RSS | Linux x86_64 | from components |\n",
    "| Session, busy or resumed | 24 MiB peak RSS | Linux x86_64 | from components |\n",
    "| `web_fetch` converting a 10 MiB HTML page, the download cap | within the busy session's 24 MiB peak RSS | Linux x86_64 | from components |\n",
    "| Idle CPU, session and terminal | zero context switches in the idle window, on every thread | Linux x86_64 | exact |\n",
    "| Threads, idle headless session | 5, plus 2 per client, plus 1 per Lua extension in use | Linux x86_64 | exact |\n",
    "| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact |\n",
    "| Log bytes, 429-call turn | the turn's content plus 2 KiB per tool call | Linux x86_64 | exact |\n",
    "| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked |\n",
    "| Terminal to its first frame, new session | 50 ms | Linux x86_64 | picked |\n",
    "| Terminal to its first frame, attaching | 50 ms plus 10 ms per MiB of session log | Linux x86_64 | picked |\n",
    "| Listing 1,000 sessions in one project, warm cache | 50 ms | Linux x86_64 | picked |\n",
    "| `session_list`, waiting for every running session's status | 2 s for the whole call | Linux x86_64 | picked |\n",
    "| `paging` jig, its session at scale 1 and 160 by 48 | 21,224 KiB peak RSS | Linux x86_64 | picked |\n",
    "| `paging` jig, open pass and first frame | 616 ms | Linux x86_64 | picked |\n",
    "| `paging` jig, slowest frame that loaded pages | 3 ms | Linux x86_64 | picked |\n",
    "| `paging` jig, slowest jump frame | 6 ms | Linux x86_64 | picked |\n",
    "| `paging` jig, slowest re-count at a new width | 48 ms | Linux x86_64 | picked |\n",
    "| `paging` jig, slowest append frame | 4 ms | Linux x86_64 | picked |\n",
    "\n## Measuring\n\n| Not | A budget |\n|---|---|\n| x | y |\n",
);

/// `DOC` with `from` replaced by `to`; `from` must be present.
fn doc_with(from: &str, to: &str) -> String {
    assert!(DOC.contains(from), "{from:?} is not in DOC");
    DOC.replacen(from, to, 1)
}

fn threads_run(zero: u64, one: u64) -> Value {
    json!([{"clients": 0, "threads": zero}, {"clients": 1, "threads": one}])
}

fn quiet_run() -> Value {
    json!([
        {"tid": 4101, "voluntary": 0, "involuntary": 0},
        {"tid": 4102, "voluntary": 0, "involuntary": 0}
    ])
}

fn head() -> Value {
    json!({
        "schema": 1, "runs": 5, "idle_secs": 10,
        "metrics": {
            "session_idle_rss_kib": [10840, 10852, 10836, 10848, 10844],
            "terminal_idle_rss_kib": [7920, 7916, 7924, 7920, 7918],
            "session_idle_switches": [quiet_run(), quiet_run(), quiet_run(), quiet_run(), quiet_run()],
            "terminal_idle_switches": [quiet_run(), quiet_run(), quiet_run(), quiet_run(), quiet_run()],
            "session_threads": [threads_run(5, 7), threads_run(5, 7), threads_run(5, 7), threads_run(5, 7), threads_run(5, 7)],
            "session_start_ms": [6.1, 5.9, 6.0, 6.3, 6.0],
            "terminal_first_frame_ms": [18.2, 17.9, 18.0, 18.4, 18.1],
            "busy_turn_rss_kib": [20480, 20490, 20470, 20485, 20475],
            "resume_20k_rss_kib": [11000, 11010, 10990, 11005, 10995],
            "resume_2m_rss_kib": [14000, 14010, 13990, 14005, 13995],
            "web_fetch_rss_kib": [22000, 22010, 21990, 22005, 21995],
            "fsyncs": [fsync_run(430, 429, 1718)],
            "turn_log_bytes": vec![log_run(1_700_000, 1_520_000, 429); 5],
            "paging_rss_kib": [10612, 10604, 10620, 10612, 10608],
            "paging_open_ms": [301.2, 308.0, 305.5, 310.1, 307.7],
            "paging_load_ms": [1.3, 1.2, 1.3, 1.4, 1.3],
            "paging_jump_ms": [2.6, 2.5, 2.7, 2.6, 2.6],
            "paging_width_ms": [24.1, 23.8, 24.0, 24.4, 24.0],
            "paging_append_ms": [1.8, 1.7, 1.8, 1.9, 1.8],
            "paging_counts": vec![json!({"lines": 8074, "turns": 10, "calls": 1051, "pages": 120, "rows": 2915}); 5]
        },
        "failures": []
    })
}

fn fsync_run(model_requests: u64, tool_calls: u64, fdatasync: u64) -> Value {
    json!({"model_requests": model_requests, "tool_calls": tool_calls, "fdatasync": fdatasync})
}

fn log_run(bytes: u64, content: u64, tool_calls: u64) -> Value {
    json!({"bytes": bytes, "content": content, "tool_calls": tool_calls})
}

fn base() -> Value {
    json!({
        "schema": 1, "runs": 5, "idle_secs": 10,
        "metrics": {
            "session_start_ms": [0.6, 0.6, 0.6, 0.6, 0.6],
            "terminal_first_frame_ms": [1.8, 1.8, 1.8, 1.8, 1.8],
            "paging_open_ms": [290.0, 290.0, 290.0, 290.0, 290.0],
            "paging_load_ms": [1.1, 1.1, 1.1, 1.1, 1.1],
            "paging_jump_ms": [2.2, 2.2, 2.2, 2.2, 2.2],
            "paging_width_ms": [20.5, 20.5, 20.5, 20.5, 20.5],
            "paging_append_ms": [1.5, 1.5, 1.5, 1.5, 1.5]
        },
        "failures": []
    })
}

fn with_metric(mut results: Value, id: &str, value: Value) -> Value {
    results["metrics"][id] = value;
    results
}

fn judge_doc(doc: &str, head: &Value, base: Option<&Value>, event: Event) -> Report {
    report(
        doc,
        &head.to_string(),
        base.map(|b| Ok(b.to_string())),
        event,
        "base0000",
    )
}

fn judge(head: &Value, base: Option<&Value>, event: Event) -> Report {
    judge_doc(DOC, head, base, event)
}

fn failures_of(head: &Value) -> Vec<String> {
    judge(head, Some(&base()), Event::PullRequest).failures
}

fn has(failures: &[String], text: &str) -> bool {
    failures.iter().any(|f| f.contains(text))
}

#[test]
fn ceilings_parse_to_kib_or_ms() {
    assert_eq!(ceiling("9,048 KiB peak RSS"), Ok(Quantity::Kib(9048.0)));
    assert_eq!(ceiling("12 MiB peak RSS"), Ok(Quantity::Kib(12288.0)));
    assert_eq!(ceiling("20 ms"), Ok(Quantity::Ms(20.0)));
    assert_eq!(ceiling("2 s for the whole call"), Ok(Quantity::Ms(2000.0)));
    assert_eq!(
        ceiling("within the busy session's 24 MiB peak RSS"),
        Ok(Quantity::Kib(24576.0))
    );
    assert_eq!(
        ceiling("50 ms plus 10 ms per MiB of session log"),
        Ok(Quantity::Ms(50.0))
    );
}

#[test]
fn a_ceiling_with_no_number_fails() {
    assert!(ceiling("zero context switches in the idle window").is_err());
    assert!(ceiling("NaN ms").is_err());
    assert!(ceiling("").is_err());
}

#[test]
fn medians_take_the_middle_of_five_runs() {
    assert_eq!(median(&[5.0, 1.0, 3.0, 2.0, 4.0]), Ok(3.0));
    assert!(median(&[1.0, 2.0, 3.0, 4.0]).is_err());
    assert!(median(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).is_err());
}

#[test]
fn every_budget_holding_passes_and_the_comment_shows_it() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert_eq!(out.comment.lines().next(), Some(MARKER));
    assert!(out.comment.contains("10844 KiB"), "{}", out.comment);
    assert!(out.comment.contains("| Base |"), "{}", out.comment);
}

#[test]
fn rows_not_measured_appear_with_their_ticket() {
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    for (budget, ticket) in [
        ("Terminal to its first frame, attaching", "#410"),
        ("Terminal to its first frame, attaching", "#668"),
        ("Listing 1,000 sessions in one project, warm cache", "#410"),
        ("`session_list`, waiting", "#581"),
    ] {
        assert!(
            comment
                .lines()
                .any(|l| l.contains(budget) && l.contains(ticket)),
            "{budget} with {ticket} missing from:\n{comment}"
        );
    }
}

#[test]
fn a_row_in_neither_list_fails() {
    let doc = doc_with(
        "| Session start,",
        "| Hub start | 5 ms | Linux x86_64 | picked |\n| Session start,",
    );
    let out = judge_doc(&doc, &head(), Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "Hub start"), "{:?}", out.failures);
}

#[test]
fn a_listed_row_missing_from_the_table_fails() {
    let doc = doc_with(
        "| Terminal, idle | 9,048 KiB peak RSS | Linux x86_64 | from components |\n",
        "",
    );
    let out = judge_doc(&doc, &head(), Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "Terminal, idle"), "{:?}", out.failures);

    let doc = doc_with(
        "| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact |\n",
        "",
    );
    let out = judge_doc(&doc, &head(), Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "fsyncs"), "{:?}", out.failures);
}

#[test]
fn a_missing_budget_table_fails() {
    let out = judge_doc(
        "# Performance\n",
        &head(),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(!out.failures.is_empty());
}

#[test]
fn an_exact_row_whose_text_changes_fails() {
    for (from, to) in [
        ("plus 2 per client", "plus 3 per client"),
        ("on every thread", "on any thread"),
    ] {
        let out = judge_doc(
            &doc_with(from, to),
            &head(),
            Some(&base()),
            Event::PullRequest,
        );
        assert!(
            has(&out.failures, "xtask/src/bench.rs"),
            "{to}: {:?}",
            out.failures
        );
    }
}

#[test]
fn a_ceiling_in_the_wrong_unit_fails() {
    let out = judge_doc(
        &doc_with("12 MiB peak RSS", "12 ms"),
        &head(),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        has(&out.failures, "Session, idle, headless"),
        "{:?}",
        out.failures
    );
    let out = judge_doc(
        &doc_with("| 20 ms |", "| 20 KiB |"),
        &head(),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(has(&out.failures, "Session start"), "{:?}", out.failures);
}

#[test]
fn memory_at_the_ceiling_passes_and_one_kib_over_fails() {
    let at = with_metric(head(), "session_idle_rss_kib", json!(vec![12288; 5]));
    assert_eq!(failures_of(&at), Vec::<String>::new());
    let over = with_metric(head(), "session_idle_rss_kib", json!(vec![12289; 5]));
    assert!(has(&failures_of(&over), "Session, idle, headless"));
    let over = with_metric(head(), "terminal_idle_rss_kib", json!(vec![9049; 5]));
    assert!(has(&failures_of(&over), "Terminal, idle"));
}

#[test]
fn a_run_count_other_than_five_fails() {
    let short = with_metric(head(), "session_idle_rss_kib", json!([1, 2, 3, 4]));
    assert!(has(&failures_of(&short), "session_idle_rss_kib"));
    let short = with_metric(
        head(),
        "session_threads",
        json!([threads_run(5, 7), threads_run(5, 7)]),
    );
    assert!(has(&failures_of(&short), "session_threads"));
    let short = with_metric(head(), "terminal_idle_switches", json!([quiet_run()]));
    assert!(has(&failures_of(&short), "terminal_idle_switches"));
    let long = with_metric(
        head(),
        "terminal_idle_switches",
        json!(vec![quiet_run(); 6]),
    );
    assert!(has(&failures_of(&long), "terminal_idle_switches"));
}

#[test]
fn threads_hold_five_plus_two_per_client_on_every_run() {
    for (zero, one) in [(5, 6), (5, 8), (4, 7), (6, 7)] {
        let mut runs = vec![threads_run(5, 7); 4];
        runs.push(threads_run(zero, one));
        let results = with_metric(head(), "session_threads", json!(runs));
        assert!(
            has(&failures_of(&results), "Threads"),
            "{zero} and {one} passed"
        );
    }
}

#[test]
fn a_run_with_no_threads_recorded_fails() {
    let mut runs = vec![threads_run(5, 7); 4];
    runs.push(json!([]));
    let results = with_metric(head(), "session_threads", json!(runs));
    assert!(has(&failures_of(&results), "session_threads"));

    let mut runs = vec![quiet_run(); 4];
    runs.push(json!([]));
    let results = with_metric(head(), "session_idle_switches", json!(runs));
    assert!(has(&failures_of(&results), "session_idle_switches"));
}

#[test]
fn one_context_switch_on_one_thread_fails() {
    for (metric, voluntary, involuntary) in [
        ("session_idle_switches", 1, 0),
        ("session_idle_switches", 0, 1),
        ("terminal_idle_switches", 1, 0),
    ] {
        let mut runs = vec![quiet_run(); 4];
        runs.push(json!([
            {"tid": 4101, "voluntary": 0, "involuntary": 0},
            {"tid": 4102, "voluntary": voluntary, "involuntary": involuntary}
        ]));
        let results = with_metric(head(), metric, json!(runs));
        let failures = failures_of(&results);
        assert!(has(&failures, "Idle CPU"), "{metric}: {failures:?}");
        assert!(has(&failures, "4102"), "{metric}: {failures:?}");
    }
}

#[test]
fn a_head_self_check_failure_fails() {
    let mut results = head();
    results["failures"] = json!(["hub did not exit"]);
    assert!(has(&failures_of(&results), "hub did not exit"));
}

#[test]
fn a_wrong_schema_fails() {
    let mut results = head();
    results["schema"] = json!(2);
    assert!(has(&failures_of(&results), "schema"));
}

#[test]
fn a_missing_metric_fails() {
    for id in [
        "terminal_idle_rss_kib",
        "session_threads",
        "terminal_first_frame_ms",
    ] {
        let mut results = head();
        results["metrics"].as_object_mut().unwrap().remove(id);
        assert!(has(&failures_of(&results), id), "{id}");
    }
}

#[test]
fn a_short_idle_window_fails() {
    let mut results = head();
    results["idle_secs"] = json!(9);
    assert!(has(&failures_of(&results), "idle"));
    results["idle_secs"] = json!(60);
    assert_eq!(failures_of(&results), Vec::<String>::new());
}

#[test]
fn malformed_head_json_fails_and_still_writes_the_comment() {
    let out = report(
        DOC,
        "{not json",
        Some(Ok(base().to_string())),
        Event::PullRequest,
        "base0000",
    );
    assert!(!out.failures.is_empty());
    assert_eq!(out.comment.lines().next(), Some(MARKER));
}

#[test]
fn a_slow_head_is_advisory_and_shows_both_medians() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let row = out
        .comment
        .lines()
        .find(|l| l.contains("Session start"))
        .unwrap();
    assert!(row.contains("6.0 ms") && row.contains("0.6 ms"), "{row}");

    let slow = with_metric(head(), "session_start_ms", json!(vec![60.0; 5]));
    assert_eq!(failures_of(&slow), Vec::<String>::new());
}

#[test]
fn a_pull_request_needs_a_base() {
    let out = judge(&head(), None, Event::PullRequest);
    assert!(has(&out.failures, "--base"), "{:?}", out.failures);
}

#[test]
fn a_push_needs_no_base_and_shows_no_base_column() {
    let out = judge(&head(), None, Event::Push);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(!out.comment.contains("| Base |"), "{}", out.comment);
}

#[test]
fn a_failed_base_passes_and_shows_why() {
    let mut failed = base();
    failed["failures"] = json!(["session start: no session_started line"]);
    let out = judge(&head(), Some(&failed), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        out.comment
            .contains("base failed: session start: no session_started line"),
        "{}",
        out.comment
    );

    let out = report(
        DOC,
        &head().to_string(),
        Some(Err("base.json: no such file".to_owned())),
        Event::PullRequest,
        "base0000",
    );
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(out.comment.contains("base failed: base.json: no such file"));

    // A base that predates every workload has nothing to compare, and
    // nothing failed.
    let missing = json!({"schema": 1, "runs": 5, "idle_secs": 10, "metrics": {}, "failures": []});
    let out = judge(&head(), Some(&missing), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("| unavailable |"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("base failed:"), "{}", out.comment);
}

#[test]
fn events_parse() {
    assert_eq!(Event::parse("pull_request"), Ok(Event::PullRequest));
    assert_eq!(Event::parse("push"), Ok(Event::Push));
    assert!(Event::parse("workflow_dispatch").is_err());
}

#[test]
fn a_thread_name_is_optional_and_named_in_the_failure() {
    let mut runs = vec![quiet_run(); 4];
    runs.push(json!([
        {"tid": 4101, "name": "loop", "voluntary": 0, "involuntary": 0},
        {"tid": 4102, "name": "status", "voluntary": 2, "involuntary": 0}
    ]));
    let failures = failures_of(&with_metric(head(), "session_idle_switches", json!(runs)));
    assert!(
        has(&failures, "thread 4102 (status): 2 voluntary"),
        "{failures:?}"
    );
    assert!(!has(&failures, "4101"), "{failures:?}");

    let named = json!([{"tid": 4101, "name": "loop", "voluntary": 0, "involuntary": 0}]);
    let results = with_metric(head(), "terminal_idle_switches", json!(vec![named; 5]));
    assert_eq!(failures_of(&results), Vec::<String>::new());

    let bad = json!([{"tid": 4101, "name": 7, "voluntary": 0, "involuntary": 0}]);
    let results = with_metric(head(), "terminal_idle_switches", json!(vec![bad; 5]));
    assert!(has(&failures_of(&results), "name"));
}

/// The comment line of the row whose budget starts with `budget`.
fn row<'a>(comment: &'a str, budget: &str) -> &'a str {
    comment
        .lines()
        .find(|l| l.starts_with(&format!("| {budget}")))
        .unwrap_or_else(|| panic!("no {budget} row in:\n{comment}"))
}

#[test]
fn the_head_column_sums_switches_and_shows_the_first_run_of_threads() {
    let mut runs = vec![quiet_run(); 4];
    runs.push(json!([
        {"tid": 4101, "voluntary": 4, "involuntary": 1},
        {"tid": 4102, "voluntary": 0, "involuntary": 1}
    ]));
    let out = judge(
        &with_metric(head(), "session_idle_switches", json!(runs)),
        Some(&base()),
        Event::PullRequest,
    );
    let idle = row(&out.comment, "Idle CPU");
    assert!(idle.contains("session_idle_switches: 6 switches"), "{idle}");
    assert!(
        idle.contains("terminal_idle_switches: 0 switches"),
        "{idle}"
    );

    let threads = row(&out.comment, "Threads");
    assert!(
        threads.contains("| 5 at 0 clients, 7 at 1 clients |"),
        "{threads}"
    );
}

#[test]
fn a_failing_row_says_fail_and_the_failures_are_listed() {
    let passing = judge(&head(), Some(&base()), Event::PullRequest).comment;
    assert!(row(&passing, "Session, idle").ends_with("| pass |"));
    assert!(!passing.contains("### Failures"), "{passing}");

    let over = with_metric(head(), "session_idle_rss_kib", json!(vec![12289; 5]));
    let failing = judge(&over, Some(&base()), Event::PullRequest).comment;
    assert!(
        row(&failing, "Session, idle").ends_with("| fail |"),
        "{failing}"
    );
    assert!(
        row(&failing, "Terminal, idle").ends_with("| pass |"),
        "{failing}"
    );
    let listed = failing
        .split("### Failures")
        .nth(1)
        .unwrap_or_else(|| panic!("no failures section in:\n{failing}"));
    assert!(
        listed.contains("- Session, idle, headless: median 12289 KiB"),
        "{failing}"
    );
}

#[test]
fn every_gated_row_but_three_is_measured() {
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let not_measured = comment.split("### Not measured").nth(1).unwrap();
    for budget in [
        "Session, busy or resumed",
        "`web_fetch` converting",
        "fsyncs",
        "Log bytes",
    ] {
        assert!(!not_measured.contains(budget), "{budget}:\n{comment}");
        assert!(row(&comment, budget).ends_with("| pass |"), "{comment}");
    }
}

#[test]
fn the_busy_ceiling_holds_on_each_of_its_three_workloads() {
    for id in [
        "busy_turn_rss_kib",
        "resume_20k_rss_kib",
        "resume_2m_rss_kib",
    ] {
        let at = with_metric(head(), id, json!(vec![24576; 5]));
        assert_eq!(failures_of(&at), Vec::<String>::new(), "{id}");
        let over = with_metric(head(), id, json!(vec![24577; 5]));
        let failures = failures_of(&over);
        assert!(
            has(&failures, "Session, busy or resumed"),
            "{id}: {failures:?}"
        );
        assert!(has(&failures, id), "{id}: {failures:?}");

        let mut missing = head();
        missing["metrics"].as_object_mut().unwrap().remove(id);
        assert!(has(&failures_of(&missing), id), "{id}");
    }
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let row = row(&comment, "Session, busy");
    assert!(row.contains("resume_2m_rss_kib: 14000 KiB"), "{row}");
}

#[test]
fn web_fetch_is_judged_against_the_busy_rows_ceiling() {
    let at = with_metric(head(), "web_fetch_rss_kib", json!(vec![24576; 5]));
    assert_eq!(failures_of(&at), Vec::<String>::new());
    let over = with_metric(head(), "web_fetch_rss_kib", json!(vec![24577; 5]));
    assert!(has(&failures_of(&over), "`web_fetch`"));

    // Raising both cells together moves the gate: the number is the busy
    // row's.
    let raised = DOC
        .replace("| 24 MiB peak RSS |", "| 30 MiB peak RSS |")
        .replace("busy session's 24 MiB", "busy session's 30 MiB");
    let out = judge_doc(&raised, &over, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());

    // A web_fetch cell that names another number than the busy row fails,
    // and its own number is never the ceiling.
    let own = doc_with("busy session's 24 MiB", "busy session's 30 MiB");
    let out = judge_doc(&own, &over, Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "`web_fetch`"), "{:?}", out.failures);
    assert!(has(&out.failures, "24 MiB peak RSS"), "{:?}", out.failures);
    let busy_only = doc_with("| 24 MiB peak RSS |", "| 30 MiB peak RSS |");
    let out = judge_doc(&busy_only, &head(), Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "`web_fetch`"), "{:?}", out.failures);

    let mut missing = head();
    missing["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("web_fetch_rss_kib");
    assert!(has(&failures_of(&missing), "web_fetch_rss_kib"));
}

#[test]
fn fsyncs_hold_two_per_model_request_and_two_per_tool_call() {
    assert_eq!(failures_of(&head()), Vec::<String>::new());
    for (requests, calls, fdatasync) in [
        (430, 429, 1719),
        (430, 429, 1717),
        // One more request than the formula's count, and one more call.
        (431, 429, 1718),
        (430, 430, 1718),
        // Nothing counted is a broken run, not a pass.
        (0, 0, 0),
    ] {
        let results = with_metric(
            head(),
            "fsyncs",
            json!([fsync_run(requests, calls, fdatasync)]),
        );
        assert!(
            has(&failures_of(&results), "fsyncs"),
            "{requests}, {calls}, {fdatasync} passed"
        );
    }
    // A single tool call and a single request: 2 + 2.
    let one = with_metric(head(), "fsyncs", json!([fsync_run(1, 1, 4)]));
    assert_eq!(failures_of(&one), Vec::<String>::new());
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let row = row(&comment, "fsyncs");
    assert!(row.contains("1718 fdatasync"), "{row}");
}

#[test]
fn the_fsync_count_is_one_untimed_pass() {
    for runs in [json!([]), json!(vec![fsync_run(430, 429, 1718); 2])] {
        let results = with_metric(head(), "fsyncs", runs);
        assert!(has(&failures_of(&results), "fsyncs"));
    }
    let results = with_metric(
        head(),
        "fsyncs",
        json!([{"model_requests": 430, "tool_calls": 429}]),
    );
    assert!(has(&failures_of(&results), "fdatasync"));
}

#[test]
fn log_bytes_hold_the_content_plus_two_kib_per_tool_call_on_every_run() {
    let limit = 1_520_000 + 2048 * 429;
    let at = with_metric(
        head(),
        "turn_log_bytes",
        json!(vec![log_run(limit, 1_520_000, 429); 5]),
    );
    assert_eq!(failures_of(&at), Vec::<String>::new());
    let mut runs = vec![log_run(limit, 1_520_000, 429); 4];
    runs.push(log_run(limit + 1, 1_520_000, 429));
    let over = with_metric(head(), "turn_log_bytes", json!(runs));
    let failures = failures_of(&over);
    assert!(has(&failures, "Log bytes"), "{failures:?}");
    assert!(has(&failures, "run 5"), "{failures:?}");

    let short = with_metric(head(), "turn_log_bytes", json!(vec![log_run(10, 1, 1); 4]));
    assert!(has(&failures_of(&short), "turn_log_bytes"));
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let row = row(&comment, "Log bytes");
    assert!(row.contains("1700000 bytes"), "{row}");
}

#[test]
fn an_exact_part_2_row_whose_text_changes_fails() {
    for (from, to) in [
        (
            "2 per model request, 2 per tool call",
            "2 per model request, 3 per tool call",
        ),
        ("plus 2 KiB per tool call", "plus 1 KiB per tool call"),
    ] {
        let out = judge_doc(
            &doc_with(from, to),
            &head(),
            Some(&base()),
            Event::PullRequest,
        );
        assert!(
            has(&out.failures, "xtask/src/bench.rs"),
            "{to}: {:?}",
            out.failures
        );
    }
}

/// The six paging rows, by Budget cell, and the metric each reads.
const PAGING: [(&str, &str); 6] = [
    ("`paging` jig, its session", "paging_rss_kib"),
    ("`paging` jig, open pass", "paging_open_ms"),
    ("`paging` jig, slowest frame that loaded", "paging_load_ms"),
    ("`paging` jig, slowest jump", "paging_jump_ms"),
    ("`paging` jig, slowest re-count", "paging_width_ms"),
    ("`paging` jig, slowest append", "paging_append_ms"),
];

#[test]
fn a_head_missing_a_paging_metric_fails_naming_it() {
    for (budget, id) in PAGING {
        let mut missing = head();
        missing["metrics"].as_object_mut().unwrap().remove(id);
        let failures = failures_of(&missing);
        assert!(has(&failures, id), "{id}: {failures:?}");
        assert!(has(&failures, budget), "{id}: {failures:?}");
    }
}

#[test]
fn paging_memory_at_its_ceiling_passes_and_one_kib_over_fails() {
    let at = with_metric(head(), "paging_rss_kib", json!(vec![21224; 5]));
    assert_eq!(failures_of(&at), Vec::<String>::new());
    let over = with_metric(head(), "paging_rss_kib", json!(vec![21225; 5]));
    let failures = failures_of(&over);
    assert!(has(&failures, "`paging` jig, its session"), "{failures:?}");
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let row = row(&comment, "`paging` jig, its session");
    assert!(row.contains("paging_rss_kib: 10612 KiB"), "{row}");
}

#[test]
fn a_slow_paging_timing_is_advisory_and_shows_both_medians() {
    let mut slow = head();
    for (_, id) in PAGING.iter().skip(1) {
        slow = with_metric(slow, id, json!(vec![9999.0; 5]));
    }
    let out = judge(&slow, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    for ((budget, _), shown) in PAGING
        .iter()
        .skip(1)
        .zip(["290.0 ms", "1.1 ms", "2.2 ms", "20.5 ms", "1.5 ms"])
    {
        let row = row(&out.comment, budget);
        assert!(row.contains("9999.0 ms"), "{row}");
        assert!(row.contains(shown), "{row}");
        assert!(row.ends_with("| advisory |"), "{row}");
    }
}

#[test]
fn a_base_with_no_paging_jig_shows_unavailable_and_keeps_its_other_medians() {
    let mut old = base();
    for (_, id) in PAGING.iter().skip(1) {
        old["metrics"].as_object_mut().unwrap().remove(*id);
    }
    let out = judge(&head(), Some(&old), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    for (budget, _) in PAGING.iter().skip(1) {
        let row = row(&out.comment, budget);
        assert!(row.contains("| unavailable |"), "{row}");
    }
    let start = row(&out.comment, "Session start");
    assert!(start.contains("| 0.6 ms |"), "{start}");
    assert!(!out.comment.contains("base failed:"), "{}", out.comment);
}

#[test]
fn a_malformed_base_paging_timing_shows_base_failed() {
    let bad = with_metric(base(), "paging_open_ms", json!("slow"));
    let out = judge(&head(), Some(&bad), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let row = row(&out.comment, "`paging` jig, open pass");
    assert!(row.contains("base failed: paging_open_ms"), "{row}");
}

/// The base with the head's memory and exact metrics, for the over-at-base
/// tests.
fn full_base() -> Value {
    let mut full = head();
    full["metrics"]["session_start_ms"] = json!([0.6, 0.6, 0.6, 0.6, 0.6]);
    full
}

fn noisy_run() -> Value {
    json!([{"tid": 4102, "voluntary": 1, "involuntary": 0}])
}

#[test]
fn a_budget_over_at_base_and_head_passes_marked_with_the_base() {
    let over = json!([
        quiet_run(),
        quiet_run(),
        quiet_run(),
        quiet_run(),
        noisy_run()
    ]);
    let over_head = with_metric(head(), "session_idle_switches", over.clone());
    let over_base = with_metric(full_base(), "session_idle_switches", over);
    let out = judge(&over_head, Some(&over_base), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let line = row(&out.comment, "Idle CPU");
    assert!(line.contains("over at base base0000"), "{line}");
    assert!(line.ends_with("| over at base |"), "{line}");

    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib.clone());
    let over_base = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = judge(&over_head, Some(&over_base), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
}

#[test]
fn a_budget_over_at_head_and_met_at_base_fails() {
    let over = json!([
        quiet_run(),
        quiet_run(),
        quiet_run(),
        quiet_run(),
        noisy_run()
    ]);
    let over_head = with_metric(head(), "session_idle_switches", over);
    let out = judge(&over_head, Some(&full_base()), Event::PullRequest);
    assert!(has(&out.failures, "4102"), "{:?}", out.failures);

    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib);
    let out = judge(&over_head, Some(&full_base()), Event::PullRequest);
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
}

#[test]
fn a_head_over_budget_with_no_base_binary_is_judged_alone() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib);
    let out = report(DOC, &over_head.to_string(), None, Event::PullRequest, "");
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
    let out = report(
        DOC,
        &over_head.to_string(),
        Some(Err("base.json: no such file".to_owned())),
        Event::PullRequest,
        "",
    );
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
}

#[test]
fn the_backstop_has_no_base_column_and_fails_on_any_budget() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over = with_metric(head(), "session_idle_rss_kib", kib);
    let out = report(
        DOC,
        &over.to_string(),
        Some(Ok(over.to_string())),
        Event::Push,
        "base0000",
    );
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
    assert!(!out.comment.contains("over at base"), "{}", out.comment);
}

#[test]
fn a_self_check_the_base_fails_too_does_not_fail_the_head() {
    let mut both = head();
    both["failures"] = json!(["hub did not exit"]);
    let mut base = full_base();
    base["failures"] = json!(["hub did not exit"]);
    let out = judge(&both, Some(&base), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(out.comment.contains("hub did not exit"), "{}", out.comment);
    let out = judge(&both, Some(&full_base()), Event::PullRequest);
    assert!(has(&out.failures, "hub did not exit"));
}
