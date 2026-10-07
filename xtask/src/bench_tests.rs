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
    "| Threads, idle headless session | 4, plus 2 per client, plus 1 for `fiber ask`'s printer, plus 1 per Lua extension in use | Linux x86_64 | exact |\n",
    "| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact |\n",
    "| Log bytes, 429-call turn | the turn's content plus 1 KiB per tool call | Linux x86_64 | exact |\n",
    "| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked |\n",
    "| Terminal to its first frame, new session | 50 ms | Linux x86_64 | picked |\n",
    "| Terminal to its first frame, attaching | 50 ms plus 10 ms per MiB of session log | Linux x86_64 | picked |\n",
    "| Listing 1,000 sessions in one project, warm cache | 50 ms | Linux x86_64 | picked |\n",
    "| `session_list`, waiting for every running session's status | 2 s for the whole call | Linux x86_64 | picked |\n",
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
            "session_threads": [threads_run(4, 6), threads_run(4, 6), threads_run(4, 6), threads_run(4, 6), threads_run(4, 6)],
            "session_start_ms": [6.1, 5.9, 6.0, 6.3, 6.0],
            "terminal_first_frame_ms": [18.2, 17.9, 18.0, 18.4, 18.1]
        },
        "failures": []
    })
}

fn base() -> Value {
    json!({
        "schema": 1, "runs": 5, "idle_secs": 10,
        "metrics": {
            "session_start_ms": [0.6, 0.6, 0.6, 0.6, 0.6],
            "terminal_first_frame_ms": [1.8, 1.8, 1.8, 1.8, 1.8]
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
        ("Session, busy or resumed", "Part 2 of #217"),
        ("fsyncs", "Part 2 of #217"),
        ("Log bytes, 429-call turn", "Part 2 of #217"),
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
        json!([threads_run(4, 6), threads_run(4, 6)]),
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
fn threads_hold_four_plus_two_per_client_on_every_run() {
    for (zero, one) in [(4, 5), (4, 7), (5, 6), (3, 6)] {
        let mut runs = vec![threads_run(4, 6); 4];
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
    let mut runs = vec![threads_run(4, 6); 4];
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
    );
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(out.comment.contains("base failed: base.json: no such file"));

    let missing = json!({"schema": 1, "runs": 5, "idle_secs": 10, "metrics": {}, "failures": []});
    let out = judge(&head(), Some(&missing), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(out.comment.contains("base failed:"), "{}", out.comment);
}

#[test]
fn events_parse() {
    assert_eq!(Event::parse("pull_request"), Ok(Event::PullRequest));
    assert_eq!(Event::parse("push"), Ok(Event::Push));
    assert!(Event::parse("workflow_dispatch").is_err());
}
