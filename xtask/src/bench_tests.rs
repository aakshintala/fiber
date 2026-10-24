use serde_json::{Value, json};

use super::judge::{Check, Rule};
use super::*;

const DOC: &str = concat!(
    "# Performance\n\n## Budgets\n\n",
    "| Budget | Ceiling | Gated on | Basis | Fast ceiling | Middle ceiling | Slow ceiling | Fast tolerance | Slow tolerance |\n",
    "|---|---|---|---|---|---|---|---|---|\n",
    "| Session, idle, headless | 12 MiB peak RSS | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| Terminal, idle | 9,048 KiB peak RSS | Linux x86_64 | from components | — | — | — | — | — |\n",
    "| Session, busy or resumed | 24 MiB peak RSS | Linux x86_64 | from components | — | — | — | — | — |\n",
    "| `web_fetch` converting a 10 MiB HTML page, the download cap | within the busy session's 24 MiB peak RSS | Linux x86_64 | from components | — | — | — | — | — |\n",
    "| Idle CPU, session and terminal | zero context switches in the idle window, on every thread | Linux x86_64 | exact | — | — | — | — | — |\n",
    "| Threads, idle headless session | 5, plus 2 per client, plus 1 per Lua extension in use | Linux x86_64 | exact | — | — | — | — | — |\n",
    "| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact | — | — | — | — | — |\n",
    "| Log bytes, 429-call turn | the turn's content plus 2 KiB per tool call | Linux x86_64 | exact | — | — | — | — | — |\n",
    "| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked | 16 ms | 12 ms | 13 ms | 55 % | 9 % |\n",
    "| Terminal to its first frame, new session | 50 ms | Linux x86_64 | picked | 7 ms | 8 ms | 11 ms | 18 % | 8 % |\n",
    "| Terminal to its first frame, attaching | 50 ms plus 10 ms per MiB of session log | Linux x86_64 | picked | 1008 ms | 1101 ms | 1364 ms | 23 % | 4 % |\n",
    "| Terminal, attaching a 10 MiB session | 200 MiB peak RSS | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| Session, memory kept after the busy turn | 200 MiB RssAnon | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| Terminal, memory kept after attaching a 10 MiB session | 200 MiB RssAnon | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| Session, memory kept after 10 busy turns, each followed by its handoff | the memory kept after the first, plus 1 MiB RssAnon | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| Listing 1,000 sessions in one project, warm cache | 50 ms | Linux x86_64 | picked | 32 ms | 34 ms | 41 ms | 21 % | 13 % |\n",
    "| `session_list`, waiting for every running session's status | 2 s for the whole call, up to 1,000 running sessions | Linux x86_64 | picked | 1 ms | 1 ms | 72 ms | 0 % | 0 % |\n",
    "| `paging` jig, its session at scale 1 and 160 by 48 | 21,224 KiB peak RSS | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| `paging` jig, open pass and first frame | 616 ms | Linux x86_64 | picked | 725 ms | 828 ms | 1049 ms | 11 % | 4 % |\n",
    "| `paging` jig, slowest frame that loaded pages | 3 ms | Linux x86_64 | picked | 8 ms | 5 ms | 10 ms | 16 % | 4 % |\n",
    "| `paging` jig, slowest jump frame | 6 ms | Linux x86_64 | picked | 9 ms | 9 ms | 13 ms | 11 % | 5 % |\n",
    "| `paging` jig, slowest re-count at a new width | 48 ms | Linux x86_64 | picked | 67 ms | 77 ms | 99 ms | 16 % | 8 % |\n",
    "| `paging` jig at scale 16, slowest re-count at a new width | 24 times the same run's scale-1 re-count | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| `paging` jig at scale 16, a search of the whole session with no match | 24 times the same run's scale-1 search | Linux x86_64 | picked | — | — | — | — | — |\n",
    "| `paging` jig, slowest append frame | 4 ms | Linux x86_64 | picked | 7 ms | 4 ms | 6 ms | 56 % | 32 % |\n",
    "| Terminal, wheel to frame, the `paging` jig's session at scale 1, 160 by 48 | 16 ms | Linux x86_64 | picked | 7 ms | 6 ms | 9 ms | 21 % | 12 % |\n",
    "| Terminal, wheel to frame, one turn with an opened `read` of 300 lines, 160 by 48 | 16 ms | Linux x86_64 | picked | 5 ms | 4 ms | 7 ms | 40 % | 23 % |\n",
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
    let mut head = json!({
        "schema": 1, "runs": 5, "idle_secs": 10,
        "metrics": {
            "session_idle_rss_kib": [10840, 10852, 10836, 10848, 10844],
            "terminal_idle_rss_kib": [7920, 7916, 7924, 7920, 7918],
            "session_idle_switches": [quiet_run(), quiet_run(), quiet_run(), quiet_run(), quiet_run()],
            "terminal_idle_switches": [quiet_run(), quiet_run(), quiet_run(), quiet_run(), quiet_run()],
            "session_threads": [threads_run(5, 7), threads_run(5, 7), threads_run(5, 7), threads_run(5, 7), threads_run(5, 7)],
            "session_start_ms": [6.1, 5.9, 6.0, 6.3, 6.0],
            "terminal_first_frame_ms": [18.2, 17.9, 18.0, 18.4, 18.1],
            "sessions_list_ms": [12.1, 12.4, 11.9, 12.0, 12.2],
            "session_list_ms": [1, 1, 1, 1, 1],
            "terminal_attach_ms": attach_metric(),
            "terminal_attach_rss_kib": [51200, 51210, 51190, 51205, 51195],
            "terminal_attach_retained_kib": [48000, 48010, 47990, 48005, 47995],
            "busy_turn_retained_kib": [19000, 19010, 18990, 19005, 18995],
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
            "paging_search_ms": [5.4, 5.3, 5.5, 5.4, 5.2],
            "paging_scale16_width_ms": [380.0, 375.0, 379.0, 385.0, 378.0],
            "paging_scale16_search_ms": [86.0, 84.0, 87.0, 86.5, 83.0],
            "paging_append_ms": [1.8, 1.7, 1.8, 1.9, 1.8],
            "wheel_paging_ms": [3.9, 4.1, 3.8, 4.0, 4.2],
            "wheel_read_ms": [1.1, 1.0, 1.2, 1.1, 1.0]
        },
        "failures": []
    });
    // Hoisted out of the literal above: nested `json!` calls inside one
    // literal exceed the default macro recursion limit, so they are
    // assigned after; the values read exactly as before.
    head["metrics"]["ten_busy_turns_retained_kib"] = json!(vec![ten_turn_run(19000, 19005); 5]);
    head["metrics"]["paging_counts"] = json!(vec![
        json!({"lines": 8074, "turns": 10, "calls": 1051, "pages": 120, "rows": 2915});
        5
    ]);
    head["metrics"]["paging_scale16_counts"] = json!(vec![
        json!({"lines": 129184, "turns": 160, "calls": 16816, "pages": 1920, "rows": 46640});
        5
    ]);
    head
}

fn attach_entries(fixture: &str, log_bytes: u64, ms: &[f64]) -> Vec<Value> {
    ms.iter()
        .map(|m| json!({"fixture": fixture, "log_bytes": log_bytes, "ms": m}))
        .collect()
}

fn attach_metric() -> Value {
    let mut entries = attach_entries("1 MiB", 1_050_231, &[31.2, 30.8, 31.0, 31.5, 30.9]);
    entries.extend(attach_entries(
        "10 MiB",
        10_492_016,
        &[88.0, 88.5, 88.2, 87.9, 88.4],
    ));
    Value::Array(entries)
}

fn attach_base_metric() -> Value {
    let mut entries = attach_entries("1 MiB", 1_050_100, &[30.5, 30.5, 30.5, 30.5, 30.5]);
    entries.extend(attach_entries(
        "10 MiB",
        10_490_000,
        &[87.0, 87.0, 87.0, 87.0, 87.0],
    ));
    Value::Array(entries)
}

fn fsync_run(model_requests: u64, tool_calls: u64, fdatasync: u64) -> Value {
    json!({"model_requests": model_requests, "tool_calls": tool_calls, "fdatasync": fdatasync})
}

fn log_run(bytes: u64, content: u64, tool_calls: u64) -> Value {
    json!({"bytes": bytes, "content": content, "tool_calls": tool_calls})
}

fn ten_turn_run(first: u64, last: u64) -> Value {
    json!({"first": first, "last": last})
}

fn base() -> Value {
    json!({
        "schema": 1, "runs": 5, "idle_secs": 10,
        "metrics": {
            "session_start_ms": [0.6, 0.6, 0.6, 0.6, 0.6],
            "terminal_first_frame_ms": [1.8, 1.8, 1.8, 1.8, 1.8],
            "sessions_list_ms": [11.5, 11.5, 11.5, 11.5, 11.5],
            "session_list_ms": [1, 1, 1, 1, 1],
            "terminal_attach_ms": attach_base_metric(),
            "paging_open_ms": [290.0, 290.0, 290.0, 290.0, 290.0],
            "paging_load_ms": [1.1, 1.1, 1.1, 1.1, 1.1],
            "paging_jump_ms": [2.2, 2.2, 2.2, 2.2, 2.2],
            "paging_width_ms": [20.5, 20.5, 20.5, 20.5, 20.5],
            "paging_append_ms": [1.5, 1.5, 1.5, 1.5, 1.5],
            "wheel_paging_ms": [3.5, 3.5, 3.5, 3.5, 3.5],
            "wheel_read_ms": [0.9, 0.9, 0.9, 0.9, 0.9]
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
        None,
        event,
        "base0000",
    )
}

/// `judge_doc` with the backstop's stored results standing in for the base:
/// the stored commit is `stored0000`.
fn judge_stored(head: &Value, base: &Value, stored: Option<&Value>, event: Event) -> Report {
    report(
        DOC,
        &head.to_string(),
        Some(Ok(base.to_string())),
        stored.map(|s| Stored {
            commit: "stored0000".to_owned(),
            text: Ok(s.to_string()),
        }),
        event,
        "base0000",
    )
}

/// `results` with its `cpu` field set to `model`.
fn with_cpu(mut results: Value, model: &str) -> Value {
    results["cpu"] = json!(model);
    results
}

/// `results` with its `binary_size_bytes` field set to `bytes`.
fn with_size(mut results: Value, bytes: u64) -> Value {
    results["binary_size_bytes"] = json!(bytes);
    results
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
    assert_eq!(median(&[5.0, 1.0, 3.0, 2.0, 4.0], 5), Ok(3.0));
    assert!(median(&[1.0, 2.0, 3.0, 4.0], 5).is_err());
    assert!(median(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 5).is_err());
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
    let not_measured = comment.split("### Not measured").nth(1).unwrap();
    assert!(
        !not_measured.contains("`session_list`, waiting") && !not_measured.contains("#581"),
        "{not_measured}"
    );
    for budget in [
        "Terminal to its first frame, attaching",
        "Listing 1,000 sessions in one project, warm cache",
    ] {
        assert!(!not_measured.contains(budget), "{budget}:\n{comment}");
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
        "| Terminal, idle | 9,048 KiB peak RSS | Linux x86_64 | from components | — | — | — | — | — |\n",
        "",
    );
    let out = judge_doc(&doc, &head(), Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "Terminal, idle"), "{:?}", out.failures);

    let doc = doc_with(
        "| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact | — | — | — | — | — |\n",
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
fn the_idle_rows_show_their_rss_split_after_the_peak() {
    let headed = with_metric(
        with_metric(
            with_metric(
                with_metric(head(), "session_idle_rss_anon_kib", json!(vec![4100; 5])),
                "session_idle_rss_file_kib",
                json!(vec![4800; 5]),
            ),
            "terminal_idle_rss_anon_kib",
            json!(vec![3100; 5]),
        ),
        "terminal_idle_rss_file_kib",
        json!(vec![4820; 5]),
    );
    let out = judge(&headed, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let session = row(&out.comment, "Session, idle");
    assert!(
        session.contains("session_idle_rss_kib: 10844 KiB (anon 4100, file 4800)"),
        "{session}"
    );
    let terminal = row(&out.comment, "Terminal, idle");
    assert!(
        terminal.contains("terminal_idle_rss_kib: 7920 KiB (anon 3100, file 4820)"),
        "{terminal}"
    );
}

#[test]
fn a_missing_rss_split_leaves_the_cell_as_it_was_and_adds_no_failure() {
    // No diagnostics at all: the cells read as they do today.
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let session = row(&out.comment, "Session, idle");
    assert!(
        session.contains("session_idle_rss_kib: 10844 KiB"),
        "{session}"
    );
    assert!(!session.contains("anon"), "{session}");
    let terminal = row(&out.comment, "Terminal, idle");
    assert!(!terminal.contains("anon"), "{terminal}");

    // One half present, the other missing or malformed: the missing half
    // is omitted, and nothing fails.
    let headed = with_metric(
        with_metric(head(), "session_idle_rss_anon_kib", json!(vec![4100; 5])),
        "session_idle_rss_file_kib",
        json!([1, 2, 3, 4]),
    );
    let out = judge(&headed, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let session = row(&out.comment, "Session, idle");
    assert!(
        session.contains("session_idle_rss_kib: 10844 KiB (anon 4100)"),
        "{session}"
    );
    assert!(!session.contains("file"), "{session}");
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
        "sessions_list_ms",
        "session_list_ms",
        "terminal_attach_ms",
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
        None,
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
        None,
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
    assert!(
        row(&out.comment, "Terminal to its first frame, attaching").contains("| unavailable |"),
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
        listed.contains("- Session, idle, headless: over budget: median 12289 KiB"),
        "{failing}"
    );
}

#[test]
fn every_gated_row_but_one_is_measured() {
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
    for budget in [
        "Terminal to its first frame, attaching",
        "Listing 1,000 sessions in one project, warm cache",
    ] {
        let line = row(&comment, budget);
        assert!(line.ends_with("| advisory |"), "{line}");
    }
}

#[test]
fn the_attach_and_retained_memory_rows_gate_their_medians() {
    for (budget, id) in [
        (
            "Terminal, attaching a 10 MiB session",
            "terminal_attach_rss_kib",
        ),
        (
            "Session, memory kept after the busy turn",
            "busy_turn_retained_kib",
        ),
        (
            "Terminal, memory kept after attaching a 10 MiB session",
            "terminal_attach_retained_kib",
        ),
    ] {
        // At the 200 MiB ceiling passes, one KiB over fails naming the
        // row and the metric.
        let limit = 200 * 1024;
        let at = with_metric(head(), id, json!(vec![limit; 5]));
        assert_eq!(failures_of(&at), Vec::<String>::new(), "{id}");
        let over = with_metric(head(), id, json!(vec![limit + 1; 5]));
        let failures = failures_of(&over);
        assert!(has(&failures, budget), "{id}: {failures:?}");
        assert!(has(&failures, id), "{id}: {failures:?}");
        // Missing entirely fails naming the metric.
        let mut missing = head();
        missing["metrics"].as_object_mut().unwrap().remove(id);
        assert!(has(&failures_of(&missing), id), "{id}");
    }
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let attach = row(&comment, "Terminal, attaching a 10 MiB");
    assert!(
        attach.contains("terminal_attach_rss_kib: 51200 KiB"),
        "{attach}"
    );
    let busy = row(&comment, "Session, memory kept");
    assert!(busy.contains("busy_turn_retained_kib: 19000 KiB"), "{busy}");
    let retained = row(&comment, "Terminal, memory kept");
    assert!(
        retained.contains("terminal_attach_retained_kib: 48000 KiB"),
        "{retained}"
    );
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

/// Judges `results` against the ten-turns check directly and returns its
/// failures and comment line: the row is not measured until #2210, so the
/// report never judges it, and this keeps covering `Rule::RetainedGrowth`.
fn ten_turns_judged(results: &Value) -> (Vec<String>, Line) {
    let text = results.to_string();
    let parsed = super::results::parse_results(&text).expect("test head parses");
    let check = Check::Exact {
        pin: "the memory kept after the first, plus 1 MiB RssAnon",
        rule: Rule::RetainedGrowth("ten_busy_turns_retained_kib"),
    };
    super::judge::judge(
        "Session, memory kept after 10 busy turns, each followed by its handoff",
        "the memory kept after the first, plus 1 MiB RssAnon",
        check,
        (Some(&parsed), None, ""),
        &Base::None,
        &[],
    )
}

#[test]
fn the_tenth_turn_keeps_a_median_of_at_most_one_mib_past_the_first() {
    // A median at the MiB past the first passes, one KiB past it fails
    // naming the metric.
    let at = with_metric(
        head(),
        "ten_busy_turns_retained_kib",
        json!(vec![ten_turn_run(19000, 20024); 5]),
    );
    assert_eq!(ten_turns_judged(&at).0, Vec::<String>::new());
    let over = with_metric(
        head(),
        "ten_busy_turns_retained_kib",
        json!(vec![ten_turn_run(19000, 20025); 5]),
    );
    let (failures, _) = ten_turns_judged(&over);
    assert!(
        has(&failures, "ten_busy_turns_retained_kib"),
        "{failures:?}"
    );
    // One leaking pass does not move the median of five; three do.
    let mut runs = vec![ten_turn_run(19000, 19005); 4];
    runs.push(ten_turn_run(19000, 20025));
    let one = with_metric(head(), "ten_busy_turns_retained_kib", json!(runs));
    assert_eq!(ten_turns_judged(&one).0, Vec::<String>::new());
    let mut runs = vec![ten_turn_run(19000, 19005); 2];
    runs.extend(vec![ten_turn_run(19000, 20025); 3]);
    let three = with_metric(head(), "ten_busy_turns_retained_kib", json!(runs));
    let (failures, _) = ten_turns_judged(&three);
    assert!(
        has(&failures, "ten_busy_turns_retained_kib"),
        "{failures:?}"
    );
    // Less than the first is no leak, so it passes.
    let shrunk = with_metric(
        head(),
        "ten_busy_turns_retained_kib",
        json!(vec![ten_turn_run(19000, 17000); 5]),
    );
    assert_eq!(ten_turns_judged(&shrunk).0, Vec::<String>::new());
    // A half missing fails naming the metric.
    for entry in [json!({"first": 19000}), json!({"last": 19005}), json!({})] {
        let results = with_metric(head(), "ten_busy_turns_retained_kib", json!(vec![entry; 5]));
        assert!(
            has(&ten_turns_judged(&results).0, "ten_busy_turns_retained_kib"),
            "{results}"
        );
    }
    // Five passes per run: none, one, two, or six entries fail the count.
    for entries in [
        json!([]),
        json!(vec![ten_turn_run(19000, 19005); 1]),
        json!(vec![ten_turn_run(19000, 19005); 2]),
        json!(vec![ten_turn_run(19000, 19005); 6]),
    ] {
        let results = with_metric(head(), "ten_busy_turns_retained_kib", entries);
        assert!(has(
            &ten_turns_judged(&results).0,
            "ten_busy_turns_retained_kib"
        ));
    }
    // Missing entirely fails naming the metric.
    let mut missing = head();
    missing["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("ten_busy_turns_retained_kib");
    assert!(has(
        &ten_turns_judged(&missing).0,
        "ten_busy_turns_retained_kib"
    ));
    // The head cell shows the median growth and run 1's readings.
    let (_, line) = ten_turns_judged(&head());
    assert!(
        line.head.contains("median growth 5 KiB over 5 runs; run 1: 19005 KiB RssAnon after the tenth, 19000 KiB after the first"),
        "{}",
        line.head
    );
    // The report gates the row: an over-budget metric fails naming the
    // row, and the comment judges it in the table instead of listing it
    // under Not measured.
    let over = with_metric(
        head(),
        "ten_busy_turns_retained_kib",
        json!(vec![ten_turn_run(19000, 20025); 5]),
    );
    let failures = failures_of(&over);
    assert!(
        has(&failures, "memory kept after 10 busy turns"),
        "{failures:?}"
    );
    let comment = judge(&head(), Some(&base()), Event::PullRequest).comment;
    let not_measured = comment.split("### Not measured").nth(1).unwrap();
    assert!(
        !not_measured.contains("memory kept after 10 busy turns"),
        "{comment}"
    );
    let line = row(
        &comment,
        "Session, memory kept after 10 busy turns, each followed by its handoff",
    );
    assert!(line.ends_with("| pass |"), "{line}");
}

#[test]
fn a_pull_request_the_selection_did_not_choose_skips_the_ten_turns_row() {
    // No metric, but the record names the skipped workload: the direct
    // check reports it skipped instead of failing the missing metric.
    // The report judges the row in the table with that note.
    let mut skipped = head();
    skipped["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("ten_busy_turns_retained_kib");
    skipped["skipped"] = json!(["10 busy turns"]);
    let (failures, line) = ten_turns_judged(&skipped);
    assert_eq!(failures, Vec::<String>::new(), "{failures:?}");
    assert_eq!(line.result, "skipped");
    assert!(
        line.head
            .contains("the selection did not run the 10-busy-turns workload"),
        "{}",
        line.head
    );
    let out = judge(&skipped, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    // The skipped row does not gate, and the table says so
    // (`docs/ci.md`, "Selection"): it leaves Not measured, which now
    // holds only the unrelated rows.
    let not_measured = out.comment.split("### Not measured").nth(1).unwrap();
    assert!(
        !not_measured.contains("memory kept after 10 busy turns"),
        "{not_measured}"
    );
    let line = row(
        &out.comment,
        "Session, memory kept after 10 busy turns, each followed by its handoff",
    );
    assert!(line.ends_with("| skipped |"), "{line}");
    assert!(
        line.contains("the selection did not run the 10-busy-turns workload"),
        "{line}"
    );
    // A run the selection chose judges the row in the table: it passes
    // on the head's metric and never appears under Not measured.
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let not_measured = out.comment.split("### Not measured").nth(1).unwrap();
    assert!(
        !not_measured.contains("memory kept after 10 busy turns"),
        "{not_measured}"
    );
    let line = row(
        &out.comment,
        "Session, memory kept after 10 busy turns, each followed by its handoff",
    );
    assert!(line.ends_with("| pass |"), "{line}");
    // The skip records only the ten-turns workload: another Exact row
    // with the skip recorded is still judged, never skipped.
    let text = skipped.to_string();
    let parsed = super::results::parse_results(&text).expect("test head parses");
    let (failures, line) = super::judge::judge(
        "fsyncs",
        "2 per model request, 2 per tool call",
        Check::Exact {
            pin: "2 per model request, 2 per tool call",
            rule: Rule::Fsyncs("fsyncs"),
        },
        (Some(&parsed), None, ""),
        &Base::None,
        &[],
    );
    assert_eq!(failures, Vec::<String>::new(), "{failures:?}");
    assert_eq!(line.result, "pass", "{}", line.head);
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

const WHEEL: [(&str, &str); 2] = [
    (
        "Terminal, wheel to frame, the `paging` jig's session",
        "wheel_paging_ms",
    ),
    (
        "Terminal, wheel to frame, one turn with an opened `read`",
        "wheel_read_ms",
    ),
];

#[test]
fn a_head_missing_a_wheel_metric_fails_naming_it() {
    for (budget, id) in WHEEL {
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

#[test]
fn the_listing_row_is_an_advisory_timing_of_sessions_list_ms() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let line = row(&out.comment, "Listing 1,000 sessions");
    assert!(line.contains("12.1 ms"), "{line}");
    assert!(line.contains("11.5 ms"), "{line}");
    assert!(line.ends_with("| advisory |"), "{line}");

    // Over the ceiling still passes: a timing row never fails.
    let slow = with_metric(head(), "sessions_list_ms", json!(vec![60.0; 5]));
    assert_eq!(failures_of(&slow), Vec::<String>::new());
    let judged = judge(&slow, Some(&base()), Event::PullRequest);
    let line = row(&judged.comment, "Listing 1,000 sessions");
    assert!(line.contains("60.0 ms"), "{line}");
    assert!(line.ends_with("| advisory |"), "{line}");
}

#[test]
fn the_attaching_ceiling_is_50_ms_plus_10_ms_per_mib_of_each_fixtures_log() {
    let mut full = attach_entries("1 MiB", 1_048_576, &[10.0, 10.0, 10.0, 10.0, 10.0]);
    full.extend(attach_entries(
        "10 MiB",
        10_485_760,
        &[20.0, 20.0, 20.0, 20.0, 20.0],
    ));
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(full));
    let out = judge(&results, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    // Each fixture's median, not one median across both.
    assert!(
        line.contains("1 MiB (1,048,576 bytes): 10.0 ms of 60.0 ms"),
        "{line}"
    );
    assert!(
        line.contains("10 MiB (10,485,760 bytes): 20.0 ms of 150.0 ms"),
        "{line}"
    );
    assert!(line.ends_with("| advisory |"), "{line}");

    // A fixture's ceiling follows its largest log: 1.5 MiB is 65.0 ms,
    // which a `*` for `/`, a `+` for `-` or a swapped constant misses.
    let mut mixed = attach_entries("1 MiB", 1_048_576, &[10.0, 10.0, 10.0, 10.0]);
    mixed.extend(attach_entries("1 MiB", 1_572_864, &[10.0]));
    mixed.extend(attach_entries(
        "10 MiB",
        10_485_760,
        &[20.0, 20.0, 20.0, 20.0, 20.0],
    ));
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(mixed));
    let judged = judge(&results, Some(&base()), Event::PullRequest);
    let line = row(&judged.comment, "Terminal to its first frame, attaching");
    assert!(
        line.contains("1 MiB (1,572,864 bytes): 10.0 ms of 65.0 ms"),
        "{line}"
    );
}

#[test]
fn an_attaching_median_over_its_ceiling_is_advisory() {
    let mut entries = attach_entries("1 MiB", 1_048_576, &[61.0; 5]);
    entries.extend(attach_entries("10 MiB", 10_485_760, &[20.0; 5]));
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(entries));
    let out = judge(&results, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.contains("61.0 ms of 60.0 ms"), "{line}");
    assert!(line.ends_with("| advisory |"), "{line}");
}

#[test]
fn the_attaching_ceiling_text_is_pinned() {
    let out = judge_doc(
        &doc_with(
            "50 ms plus 10 ms per MiB of session log",
            "50 ms plus 20 ms per MiB of session log",
        ),
        &head(),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        has(&out.failures, "xtask/src/bench.rs"),
        "{:?}",
        out.failures
    );
    assert!(
        has(&out.failures, "Terminal to its first frame, attaching"),
        "{:?}",
        out.failures
    );
}

#[test]
fn an_attaching_fixture_with_four_runs_fails() {
    let mut entries = attach_entries("1 MiB", 1_048_576, &[10.0, 10.0, 10.0, 10.0]);
    entries.extend(attach_entries("10 MiB", 10_485_760, &[20.0; 5]));
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(entries));
    let failures = failures_of(&results);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(has(&failures, "terminal_attach_ms"), "{failures:?}");
}

#[test]
fn an_attaching_run_missing_its_ten_mib_fixture_fails() {
    let results = with_metric(
        head(),
        "terminal_attach_ms",
        Value::Array(attach_entries("1 MiB", 1_048_576, &[10.0; 5])),
    );
    let failures = failures_of(&results);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(has(&failures, "terminal_attach_ms"), "{failures:?}");
}

#[test]
fn an_unknown_metric_id_is_ignored() {
    let entry =
        json!({"fixture": "1 MiB", "log_bytes": 1_050_231, "stage": "terminal", "ms": 31.0});
    let headed = with_metric(head(), "attach_stage_ms", Value::Array(vec![entry; 5]));
    let plain = judge(&head(), Some(&base()), Event::PullRequest);
    let out = judge(&headed, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, plain.failures);
    assert_eq!(out.comment, plain.comment);
    assert!(!has(&out.failures, "attach_stage"), "{:?}", out.failures);
}

#[test]
fn an_unknown_attaching_fixture_fails() {
    let mut entries = attach_entries("1 MiB", 1_048_576, &[10.0; 5]);
    entries.extend(attach_entries("2 MiB", 2_097_152, &[15.0; 5]));
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(entries));
    let failures = failures_of(&results);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(has(&failures, "terminal_attach_ms"), "{failures:?}");
}

#[test]
fn an_attaching_entry_without_log_bytes_fails() {
    let mut entries = attach_entries("1 MiB", 1_048_576, &[10.0; 5]);
    entries.extend(vec![json!({"fixture": "10 MiB", "ms": 20.0}); 5]);
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(entries));
    let failures = failures_of(&results);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(has(&failures, "terminal_attach_ms"), "{failures:?}");
}

#[test]
fn a_base_without_the_attach_metric_shows_unavailable() {
    let mut old = base();
    old["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("terminal_attach_ms");
    let out = judge(&head(), Some(&old), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let attach = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(attach.contains("| unavailable |"), "{attach}");
    assert!(!out.comment.contains("base failed:"), "{}", out.comment);
    let listing = row(&out.comment, "Listing 1,000 sessions");
    assert!(listing.contains("| 11.5 ms |"), "{listing}");
}

#[test]
fn an_extra_attaching_fixture_fails() {
    let mut entries = attach_entries("1 MiB", 1_048_576, &[10.0; 5]);
    entries.extend(attach_entries("10 MiB", 10_485_760, &[20.0; 5]));
    entries.extend(attach_entries("2 MiB", 2_097_152, &[15.0; 5]));
    let results = with_metric(head(), "terminal_attach_ms", Value::Array(entries));
    let failures = failures_of(&results);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(has(&failures, "terminal_attach_ms"), "{failures:?}");
}

#[test]
fn a_base_with_the_attach_metric_shows_its_medians() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let attach = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(attach.contains("30.5 ms"), "{attach}");
    assert!(attach.contains("87.0 ms"), "{attach}");
    assert!(!attach.contains("unavailable"), "{attach}");
}

#[test]
fn commas_places_the_first_comma_at_exactly_1000_and_never_leads() {
    let metric = || {
        let mut entries = attach_entries("1 MiB", 1_000, &[10.0; 5]);
        entries.extend(attach_entries("10 MiB", 100_000, &[20.0; 5]));
        with_metric(head(), "terminal_attach_ms", Value::Array(entries))
    };
    let out = judge(&metric(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.contains("1 MiB (1,000 bytes)"), "{line}");
    // Six digits take one comma, never a leading one.
    assert!(line.contains("10 MiB (100,000 bytes)"), "{line}");
    assert!(!line.contains("(,"), "{line}");
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
    let over_stored = with_metric(full_base(), "session_idle_switches", over);
    let out = judge_stored(&over_head, &base(), Some(&over_stored), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let line = row(&out.comment, "Idle CPU");
    assert!(line.contains("over at base stored0000"), "{line}");
    assert!(line.ends_with("| over at base |"), "{line}");

    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib.clone());
    let over_stored = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = judge_stored(&over_head, &base(), Some(&over_stored), Event::PullRequest);
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
    let out = judge_stored(&over_head, &base(), Some(&full_base()), Event::PullRequest);
    assert!(has(&out.failures, "4102"), "{:?}", out.failures);

    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib);
    let out = judge_stored(&over_head, &base(), Some(&full_base()), Event::PullRequest);
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
    let out = report(
        DOC,
        &over_head.to_string(),
        None,
        None,
        Event::PullRequest,
        "",
    );
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
    let out = report(
        DOC,
        &over_head.to_string(),
        Some(Err("base.json: no such file".to_owned())),
        None,
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
        Some(Stored {
            commit: "stored0000".to_owned(),
            text: Ok(over.to_string()),
        }),
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
fn a_malformed_base_or_head_metric_excuses_nothing() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib.clone());
    // A stored metric that is not an array, or has too few samples, proves nothing.
    for bad in [json!("x"), json!([1, 2])] {
        let stored = with_metric(full_base(), "session_idle_rss_kib", bad);
        let out = judge_stored(&over_head, &base(), Some(&stored), Event::PullRequest);
        assert!(
            has(&out.failures, "session_idle_rss_kib"),
            "{:?}",
            out.failures
        );
    }
    // A head metric that is missing still fails when the stored results are over.
    let mut missing = head();
    missing["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("session_idle_rss_kib");
    let over_stored = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = judge_stored(&missing, &base(), Some(&over_stored), Event::PullRequest);
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
}

#[test]
fn a_self_check_the_base_fails_too_still_fails_the_head() {
    let mut both = head();
    both["failures"] = json!(["hub did not exit"]);
    let mut failed = full_base();
    failed["failures"] = json!(["hub did not exit"]);
    let out = judge_stored(&both, &failed, Some(&full_base()), Event::PullRequest);
    assert!(has(&out.failures, "hub did not exit"), "{:?}", out.failures);
}

#[test]
fn a_wrong_unit_ceiling_fails_even_when_the_base_is_over() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib.clone());
    let over_stored = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = report(
        &doc_with("12 MiB peak RSS", "12 ms"),
        &over_head.to_string(),
        Some(Ok(base().to_string())),
        Some(Stored {
            commit: "stored0000".to_owned(),
            text: Ok(over_stored.to_string()),
        }),
        Event::PullRequest,
        "base0000",
    );
    assert!(
        has(&out.failures, "not a memory ceiling"),
        "{:?}",
        out.failures
    );
}

/// `results` with `runs` and `timing_runs` set.
fn with_counts(mut results: Value, runs: u64, timing_runs: u64) -> Value {
    results["runs"] = json!(runs);
    results["timing_runs"] = json!(timing_runs);
    results
}

/// `results` with `metric_runs` declaring each of `counts`.
fn with_metric_runs(mut results: Value, counts: &[(&str, u64)]) -> Value {
    let mut declared = serde_json::Map::new();
    for (id, runs) in counts {
        declared.insert((*id).to_owned(), json!(runs));
    }
    results["metric_runs"] = Value::Object(declared);
    results
}

/// `results` with metric `id`'s array cut to its first `n` entries.
fn trim_metric(results: &Value, id: &str, n: usize) -> Value {
    let mut out = results.clone();
    let entries = out["metrics"][id].as_array().unwrap()[..n].to_vec();
    out["metrics"][id] = Value::Array(entries);
    out
}

/// `results` with each `terminal_attach_ms` fixture cut to its first `n`
/// entries.
fn trim_attach(results: &Value, n: usize) -> Value {
    let mut out = results.clone();
    let mut kept: Vec<Value> = Vec::new();
    for fixture in ["1 MiB", "10 MiB"] {
        kept.extend(
            out["metrics"]["terminal_attach_ms"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["fixture"] == fixture)
                .take(n)
                .cloned(),
        );
    }
    out["metrics"]["terminal_attach_ms"] = Value::Array(kept);
    out
}

#[test]
fn a_pull_request_file_with_three_timing_runs_passes() {
    let mut three = with_counts(head(), 5, 3);
    three = with_metric_runs(
        three,
        &[
            ("session_start_ms", 3),
            ("terminal_first_frame_ms", 3),
            ("sessions_list_ms", 3),
            ("wheel_paging_ms", 3),
            ("wheel_read_ms", 3),
        ],
    );
    // Three medians no five-run median of the fixture equals.
    three = with_metric(three, "session_start_ms", json!([9.0, 9.4, 9.2]));
    three = with_metric(three, "terminal_first_frame_ms", json!([19.0, 19.4, 19.2]));
    three = with_metric(three, "sessions_list_ms", json!([13.0, 13.4, 13.2]));
    three = with_metric(three, "wheel_paging_ms", json!([4.9, 5.3, 5.1]));
    three = with_metric(three, "wheel_read_ms", json!([2.9, 3.3, 3.1]));
    let out = judge(&three, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let line = row(&out.comment, "Session start, the internal session command");
    assert!(line.contains("9.2 ms"), "{line}");
}

#[test]
fn a_metric_whose_count_differs_from_its_declared_runs_fails() {
    let declared = with_metric_runs(head(), &[("session_start_ms", 3)]);
    for (values, runs) in [(json!([6.0, 6.1]), 2), (json!([6.0, 6.1, 6.2, 6.3]), 4)] {
        let out = judge(
            &with_metric(declared.clone(), "session_start_ms", values),
            Some(&base()),
            Event::PullRequest,
        );
        assert!(
            out.failures
                .iter()
                .any(|f| f.contains("session_start_ms") && f.contains("expected 3")),
            "{runs}: {:?}",
            out.failures
        );
    }
    let out = judge(
        &with_metric(declared.clone(), "session_start_ms", json!([6.0, 6.1, 6.2])),
        Some(&base()),
        Event::PullRequest,
    );
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    assert_eq!(median(&[3.0, 1.0, 2.0], 3), Ok(2.0));
    assert!(median(&[1.0, 2.0], 3).is_err());
}

#[test]
fn a_file_without_metric_runs_expects_runs_for_every_metric() {
    // Every array cut to its first 3 entries, the attach fixtures to 3
    // each; `fsyncs` keeps its one pass.
    let mut three = with_counts(head(), 3, 3);
    for id in [
        "session_idle_rss_kib",
        "terminal_idle_rss_kib",
        "session_idle_switches",
        "terminal_idle_switches",
        "session_threads",
        "session_start_ms",
        "terminal_first_frame_ms",
        "sessions_list_ms",
        "session_list_ms",
        "terminal_attach_rss_kib",
        "terminal_attach_retained_kib",
        "busy_turn_retained_kib",
        "ten_busy_turns_retained_kib",
        "busy_turn_rss_kib",
        "resume_20k_rss_kib",
        "resume_2m_rss_kib",
        "web_fetch_rss_kib",
        "paging_rss_kib",
        "paging_open_ms",
        "paging_load_ms",
        "paging_jump_ms",
        "paging_width_ms",
        "paging_search_ms",
        "paging_scale16_width_ms",
        "paging_scale16_search_ms",
        "paging_append_ms",
        "wheel_paging_ms",
        "wheel_read_ms",
        "paging_counts",
    ] {
        three = trim_metric(&three, id, 3);
    }
    three = trim_metric(&three, "turn_log_bytes", 3);
    three = trim_attach(&three, 3);
    let out = judge(&three, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);

    // A declared count overrides the file's for its own metric.
    let five = with_metric_runs(head(), &[("turn_log_bytes", 3)]);
    let out = judge(
        &trim_metric(&five, "turn_log_bytes", 3),
        Some(&base()),
        Event::PullRequest,
    );
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let out = judge(&five, Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "turn_log_bytes"), "{:?}", out.failures);
    let attach = with_metric_runs(head(), &[("terminal_attach_ms", 3)]);
    let out = judge(&trim_attach(&attach, 3), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let out = judge(&attach, Some(&base()), Event::PullRequest);
    assert!(
        has(&out.failures, "terminal_attach_ms"),
        "{:?}",
        out.failures
    );
}

#[test]
fn the_results_file_declares_valid_counts() {
    let mut no_runs = head();
    no_runs.as_object_mut().unwrap().remove("runs");
    let out = report(
        DOC,
        &no_runs.to_string(),
        Some(Ok(base().to_string())),
        None,
        Event::PullRequest,
        "base0000",
    );
    assert!(has(&out.failures, "no runs"), "{:?}", out.failures);

    let out = report(
        DOC,
        &with_counts(head(), 0, 0).to_string(),
        Some(Ok(base().to_string())),
        None,
        Event::PullRequest,
        "base0000",
    );
    assert!(!out.failures.is_empty(), "{:?}", out.failures);

    // Each malformed `metric_runs` value names it.
    for bad in [json!(0), json!("3"), json!(-1)] {
        let mut malformed = with_metric_runs(head(), &[("session_start_ms", 1)]);
        malformed["metric_runs"]["session_start_ms"] = bad;
        let out = report(
            DOC,
            &malformed.to_string(),
            Some(Ok(base().to_string())),
            None,
            Event::PullRequest,
            "base0000",
        );
        assert!(has(&out.failures, "metric_runs"), "{:?}", out.failures);
    }

    let one = with_metric_runs(head(), &[("session_start_ms", 1)]);
    let one = with_metric(one, "session_start_ms", json!([6.0]));
    let out = judge(&one, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
}

#[test]
fn a_memory_or_exact_budget_the_stored_results_fail_too_passes_naming_their_commit() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib.clone());
    let over_stored = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = judge_stored(&over_head, &base(), Some(&over_stored), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let line = row(&out.comment, "Session, idle, headless");
    assert!(line.contains("over at base stored0000"), "{line}");
    assert!(line.ends_with("| over at base |"), "{line}");
    assert!(
        out.comment.contains(
            "Memory and exact rows compare against the backstop's stored results for stored0000."
        ),
        "{}",
        out.comment
    );

    let over = json!([
        quiet_run(),
        quiet_run(),
        quiet_run(),
        quiet_run(),
        noisy_run()
    ]);
    let over_head = with_metric(head(), "session_idle_switches", over.clone());
    let over_stored = with_metric(full_base(), "session_idle_switches", over);
    let out = judge_stored(&over_head, &base(), Some(&over_stored), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let line = row(&out.comment, "Idle CPU");
    assert!(line.contains("over at base stored0000"), "{line}");
}

#[test]
fn a_live_base_over_budget_excuses_nothing() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib.clone());
    let over_base = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = judge(&over_head, Some(&over_base), Event::PullRequest);
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
    assert!(
        out.comment.contains(
            "No stored results for the base or its 9 nearest first-parent ancestors: \
             memory and exact rows are judged against the table alone."
        ),
        "{}",
        out.comment
    );
}

#[test]
fn unreadable_stored_results_are_reported_and_excuse_nothing() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_metric(head(), "session_idle_rss_kib", kib);
    for text in [
        Err("head.json: no such file".to_owned()),
        Ok("not json".to_owned()),
    ] {
        let out = report(
            DOC,
            &over_head.to_string(),
            Some(Ok(base().to_string())),
            Some(Stored {
                commit: "stored0000".to_owned(),
                text,
            }),
            Event::PullRequest,
            "base0000",
        );
        assert!(
            has(&out.failures, "session_idle_rss_kib"),
            "{:?}",
            out.failures
        );
        assert!(
            out.comment
                .contains("The stored results for stored0000 could not be read ("),
            "{}",
            out.comment
        );
    }
}

#[test]
fn the_comment_lists_each_files_cpu() {
    let headed = with_cpu(head(), "CPU-H");
    let based = with_cpu(base(), "CPU-B");
    let stored = with_cpu(full_base(), "CPU-S");
    let out = report(
        DOC,
        &headed.to_string(),
        Some(Ok(based.to_string())),
        Some(Stored {
            commit: "stored0000".to_owned(),
            text: Ok(stored.to_string()),
        }),
        Event::PullRequest,
        "base0000",
    );
    assert!(out.comment.contains("- Head CPU: CPU-H"), "{}", out.comment);
    assert!(
        out.comment.contains("- Base CPU (base0000): CPU-B"),
        "{}",
        out.comment
    );
    assert!(
        out.comment
            .contains("- Stored results CPU (stored0000): CPU-S"),
        "{}",
        out.comment
    );

    // A file without a `cpu` field shows `not recorded`.
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert!(
        out.comment.contains("- Head CPU: not recorded"),
        "{}",
        out.comment
    );

    // With no live base there is no base line.
    let out = report(
        DOC,
        &headed.to_string(),
        Some(Err("base.json: no such file".to_owned())),
        Some(Stored {
            commit: "stored0000".to_owned(),
            text: Ok(stored.to_string()),
        }),
        Event::PullRequest,
        "base0000",
    );
    assert!(!out.comment.contains("Base CPU"), "{}", out.comment);
}

#[test]
fn a_push_shows_the_head_cpu_and_ignores_stored_results() {
    let kib = json!([30000, 30000, 30000, 30000, 30000]);
    let over_head = with_cpu(
        with_metric(head(), "session_idle_rss_kib", kib.clone()),
        "CPU-H",
    );
    let over_stored = with_metric(full_base(), "session_idle_rss_kib", kib);
    let out = report(
        DOC,
        &over_head.to_string(),
        Some(Ok(base().to_string())),
        Some(Stored {
            commit: "stored0000".to_owned(),
            text: Ok(over_stored.to_string()),
        }),
        Event::Push,
        "base0000",
    );
    assert!(
        has(&out.failures, "session_idle_rss_kib"),
        "{:?}",
        out.failures
    );
    assert!(out.comment.contains("- Head CPU:"), "{}", out.comment);
    assert!(!out.comment.contains("Base CPU"), "{}", out.comment);
    assert!(!out.comment.contains("Stored results"), "{}", out.comment);
    assert!(!out.comment.contains("stored0000"), "{}", out.comment);
}

#[test]
fn timings_compare_against_the_live_base_not_the_stored_results() {
    let stored = with_metric(
        full_base(),
        "session_start_ms",
        json!([9.9, 9.9, 9.9, 9.9, 9.9]),
    );
    let out = judge_stored(&head(), &base(), Some(&stored), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    let line = row(&out.comment, "Session start, the internal session command");
    assert!(line.contains("0.6 ms"), "{line}");
    assert!(!line.contains("9.9 ms"), "{line}");
}

#[test]
fn the_run_count_sentence_follows_the_head_file() {
    let out = judge(
        &with_counts(head(), 5, 3),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        out.comment.contains(
            "Each number is the median of 5 runs on Linux x86_64, \
             a timing-only workload's or the paging jig's of 3 (`docs/performance.md`)."
        ),
        "{}",
        out.comment
    );
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert!(
        out.comment.contains(
            "Each number is the median of 5 runs on Linux x86_64 (`docs/performance.md`)."
        ),
        "{}",
        out.comment
    );
    let out = report(
        DOC,
        "{not json",
        Some(Ok(base().to_string())),
        None,
        Event::PullRequest,
        "base0000",
    );
    assert!(
        out.comment.contains(
            "Each number is the median of its runs on Linux x86_64 (`docs/performance.md`)."
        ),
        "{}",
        out.comment
    );
}

#[test]
fn a_base_that_skipped_rows_names_them_in_the_summary() {
    let mut unselected = base();
    for id in ["session_start_ms", "sessions_list_ms"] {
        unselected["metrics"].as_object_mut().unwrap().remove(id);
    }
    let unselected = with_skipped(unselected, &["session start", "sessions list"]);
    let out = judge(&head(), Some(&unselected), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        out.comment.contains(
            "The base pass skipped session start, sessions list: \
             the selection did not choose them (`docs/ci.md`, \"Selection\")."
        ),
        "{}",
        out.comment
    );
    // An unselected row still runs on head and is reported with no ratio
    // and no gate: the base cell reads unavailable and the row stays
    // advisory, through the existing missing-metric arm.
    for budget in ["Session start", "Listing 1,000 sessions"] {
        let line = row(&out.comment, budget);
        assert!(line.contains("| unavailable |"), "{line}");
        assert!(line.ends_with("| advisory |"), "{line}");
    }
}

#[test]
fn the_summary_names_no_skip_without_a_skipped_base() {
    // No sentence when `skipped` is empty or absent, ...
    let out = judge(
        &head(),
        Some(&with_skipped(base(), &[])),
        Event::PullRequest,
    );
    assert!(
        !out.comment.contains("The base pass skipped"),
        "{}",
        out.comment
    );
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert!(
        !out.comment.contains("The base pass skipped"),
        "{}",
        out.comment
    );
    // ... on a push, which has no base column, ...
    let out = judge(
        &head(),
        Some(&with_skipped(base(), &["session start"])),
        Event::Push,
    );
    assert!(
        !out.comment.contains("The base pass skipped"),
        "{}",
        out.comment
    );
    // ... or when the base failed.
    let mut failed = with_skipped(base(), &["session start"]);
    failed["failures"] = json!(["base broke"]);
    let out = judge(&head(), Some(&failed), Event::PullRequest);
    assert!(
        !out.comment.contains("The base pass skipped"),
        "{}",
        out.comment
    );
}

/// The six selectable timing rows with the budgets each feeds and the
/// metric ids those budgets read: the harness's own mapping, written
/// literally so the test below fails if `ROW_METRICS` drifts from it.
const HEAD_SKIP_ROWS: &[(&str, &[&str], &[&str])] = &[
    (
        "session start",
        &["Session start, the internal session command to its first line, no hub"],
        &["session_start_ms"],
    ),
    (
        "sessions list",
        &["Listing 1,000 sessions in one project, warm cache"],
        &["sessions_list_ms"],
    ),
    (
        "terminal first frame",
        &["Terminal to its first frame, new session"],
        &["terminal_first_frame_ms"],
    ),
    (
        "terminal attach",
        &[
            "Terminal to its first frame, attaching",
            "Terminal, attaching a 10 MiB session",
            "Terminal, memory kept after attaching a 10 MiB session",
        ],
        &[
            "terminal_attach_ms",
            "terminal_attach_rss_kib",
            "terminal_attach_retained_kib",
        ],
    ),
    (
        "terminal wheel",
        &[
            "Terminal, wheel to frame, the `paging` jig's session at scale 1, 160 by 48",
            "Terminal, wheel to frame, one turn with an opened `read` of 300 lines, 160 by 48",
        ],
        &["wheel_paging_ms", "wheel_read_ms"],
    ),
    (
        "paging",
        &[
            "`paging` jig, its session at scale 1 and 160 by 48",
            "`paging` jig, open pass and first frame",
            "`paging` jig, slowest frame that loaded pages",
            "`paging` jig, slowest jump frame",
            "`paging` jig, slowest re-count at a new width",
            "`paging` jig, slowest append frame",
            "`paging` jig at scale 16, slowest re-count at a new width",
            "`paging` jig at scale 16, a search of the whole session with no match",
        ],
        &[
            "paging_rss_kib",
            "paging_open_ms",
            "paging_load_ms",
            "paging_jump_ms",
            "paging_width_ms",
            "paging_append_ms",
            "paging_search_ms",
            "paging_scale16_width_ms",
            "paging_scale16_search_ms",
        ],
    ),
];

/// The comment's budget rows that read skipped: every table row whose
/// result cell is `skipped`, by budget.
fn skipped_budgets(comment: &str) -> Vec<String> {
    comment
        .lines()
        .filter(|line| line.starts_with("| ") && line.ends_with("| skipped |"))
        .map(|line| {
            line.strip_prefix("| ")
                .and_then(|rest| rest.split(" |").next())
                .unwrap_or("")
                .to_owned()
        })
        .filter(|budget| budget != "Budget")
        .collect()
}

#[test]
fn each_head_skipped_row_skips_exactly_the_budgets_it_feeds() {
    // A mutant that skips the wrong budgets, or judges a skipped budget
    // as before, fails the rows below.
    for (row_name, budgets, ids) in HEAD_SKIP_ROWS {
        let mut head = head();
        for id in *ids {
            head = without_metric(&head, id);
        }
        let head = with_skipped(head, &[*row_name]);
        let out = judge(&head, Some(&base()), Event::PullRequest);
        assert_eq!(out.failures, Vec::<String>::new(), "{row_name}");
        for budget in *budgets {
            let line = row(&out.comment, budget);
            assert!(line.ends_with("| skipped |"), "{row_name} {budget}: {line}");
            assert!(
                line.contains(&format!("skipped: the selection did not run {row_name}")),
                "{row_name} {budget}: {line}"
            );
        }
        // No other budget reads skipped: the ten-turns row aside, which
        // holds its metrics here and stays passed.
        let skipped = skipped_budgets(&out.comment);
        assert_eq!(skipped.len(), budgets.len(), "{row_name}: {skipped:?}");
        for budget in skipped {
            assert!(budgets.contains(&budget.as_str()), "{row_name}: {budget}");
        }
    }
}

#[test]
fn a_head_skip_forgives_only_the_budgets_it_feeds() {
    // The head skips `paging` but still owes `session_start_ms`: the
    // missing metric fails while the paging budgets read skipped.
    let mut head = head();
    for id in [
        "paging_rss_kib",
        "paging_open_ms",
        "paging_load_ms",
        "paging_jump_ms",
        "paging_width_ms",
        "paging_append_ms",
        "paging_search_ms",
        "paging_scale16_width_ms",
        "paging_scale16_search_ms",
    ] {
        head = without_metric(&head, id);
    }
    let head = without_metric(&with_skipped(head, &["paging"]), "session_start_ms");
    let out = judge(&head, Some(&base()), Event::PullRequest);
    assert!(!out.failures.is_empty());
    assert!(has(&out.failures, "session_start_ms"), "{:?}", out.failures);
    let line = row(&out.comment, "`paging` jig, open pass and first frame");
    assert!(line.ends_with("| skipped |"), "{line}");
}

#[test]
fn a_skipped_budget_still_checks_its_table_row() {
    // A mutant that skips the table checks for a skipped budget fails
    // each case below.
    let paging_head = || {
        let mut head = head();
        for id in [
            "paging_rss_kib",
            "paging_open_ms",
            "paging_load_ms",
            "paging_jump_ms",
            "paging_width_ms",
            "paging_append_ms",
            "paging_search_ms",
            "paging_scale16_width_ms",
            "paging_scale16_search_ms",
        ] {
            head = without_metric(&head, id);
        }
        with_skipped(head, &["paging"])
    };
    // A changed pin on a skipped scale-16 ratio row fails.
    let out = judge_doc(
        &doc_with(
            "24 times the same run's scale-1 re-count",
            "25 times the same run's scale-1 re-count",
        ),
        &paging_head(),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        has(
            &out.failures,
            "`paging` jig at scale 16, slowest re-count at a new width"
        ),
        "{:?}",
        out.failures
    );
    // A ceiling in the wrong unit on a skipped memory budget fails.
    let mut attach_head = head();
    for id in [
        "terminal_attach_ms",
        "terminal_attach_rss_kib",
        "terminal_attach_retained_kib",
    ] {
        attach_head = without_metric(&attach_head, id);
    }
    let out = judge_doc(
        &doc_with("200 MiB peak RSS", "200 ms peak RSS"),
        &with_skipped(attach_head, &["terminal attach"]),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        has(&out.failures, "Terminal, attaching a 10 MiB session"),
        "{:?}",
        out.failures
    );
    // A malformed class cell on a skipped timing budget fails.
    let out = judge_doc(
        &doc_with("| 725 ms |", "| fast |"),
        &paging_head(),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        has(&out.failures, "`paging` jig, open pass and first frame"),
        "{:?}",
        out.failures
    );
    // A changed attaching pin fails when the attach row is skipped.
    let mut pin_head = head();
    for id in [
        "terminal_attach_ms",
        "terminal_attach_rss_kib",
        "terminal_attach_retained_kib",
    ] {
        pin_head = without_metric(&pin_head, id);
    }
    let out = judge_doc(
        &doc_with(
            "50 ms plus 10 ms per MiB of session log",
            "50 ms plus 11 ms per MiB of session log",
        ),
        &with_skipped(pin_head, &["terminal attach"]),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        has(&out.failures, "Terminal to its first frame, attaching"),
        "{:?}",
        out.failures
    );
}

#[test]
fn row_metrics_names_match_the_selectors_rows() {
    // A docs-only diff selects no row, so the selector returns every row
    // in table order: the judge's row table must name the same rows.
    let selected = crate::select::base_rows_skipped("pull_request", &["README.md".to_owned()]);
    let names: Vec<&str> = super::judge::ROW_METRICS
        .iter()
        .map(|(row, _)| *row)
        .collect();
    assert_eq!(names, selected);
}

#[test]
fn a_row_the_head_did_not_skip_is_judged_as_before() {
    // The head skips `session start` yet holds an over-ceiling paging
    // median: the paging budget fails while session start reads skipped.
    // A mutant scoping the skip to the whole head file fails the second
    // assert; one scoping it to the wrong rows fails the first.
    let head = with_metric(
        with_skipped(head(), &["session start"]),
        "paging_rss_kib",
        json!(vec![999999; 5]),
    );
    let out = judge(&head, Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "paging_rss_kib"), "{:?}", out.failures);
    let line = row(&out.comment, "Session start, the internal");
    assert!(line.ends_with("| skipped |"), "{line}");
}

#[test]
fn the_summary_names_the_rows_the_head_pass_skipped() {
    // A mutant that drops the head sentence, or judges a skipped row as
    // before, fails the asserts below.
    let mut full = head();
    for id in [
        "session_start_ms",
        "paging_rss_kib",
        "paging_open_ms",
        "paging_load_ms",
        "paging_jump_ms",
        "paging_width_ms",
        "paging_append_ms",
        "paging_search_ms",
        "paging_scale16_width_ms",
        "paging_scale16_search_ms",
    ] {
        full = without_metric(&full, id);
    }
    let unmeasured = with_skipped(full, &["session start", "paging"]);
    let out = judge(&unmeasured, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        out.comment.contains(
            "The head pass skipped session start, paging: the selection did not \
             choose them; the budgets they feed read skipped, and the `main` backstop runs \
             every gating row (`docs/ci.md`, \"Selection\")."
        ),
        "{}",
        out.comment
    );
    for budget in [
        "Session start, the internal",
        "`paging` jig, open pass and first frame",
    ] {
        assert!(
            row(&out.comment, budget).ends_with("| skipped |"),
            "{}",
            out.comment
        );
    }
    // The 10-busy-turns row keeps its own note and adds no head sentence.
    let out = judge(
        &with_skipped(head(), &["10 busy turns"]),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        !out.comment.contains("The head pass skipped"),
        "{}",
        out.comment
    );
    // A head and a base that skipped the same rows name both sentences,
    // the head's first.
    let mut unselected = base();
    for id in [
        "session_start_ms",
        "paging_rss_kib",
        "paging_open_ms",
        "paging_load_ms",
        "paging_jump_ms",
        "paging_width_ms",
        "paging_append_ms",
        "paging_search_ms",
        "paging_scale16_width_ms",
        "paging_scale16_search_ms",
    ] {
        unselected = without_metric(&unselected, id);
    }
    let out = judge(
        &unmeasured,
        Some(&with_skipped(unselected, &["session start", "paging"])),
        Event::PullRequest,
    );
    let head_at = out
        .comment
        .find("The head pass skipped session start, paging:")
        .unwrap_or_else(|| panic!("no head sentence in:\n{}", out.comment));
    let base_at = out
        .comment
        .find("The base pass skipped session start, paging:")
        .unwrap_or_else(|| panic!("no base sentence in:\n{}", out.comment));
    assert!(head_at < base_at, "{}", out.comment);
}

#[test]
fn the_summary_names_no_head_skip_when_none() {
    // No sentence when `skipped` is empty or absent, ...
    let out = judge(
        &head(),
        Some(&with_skipped(base(), &[])),
        Event::PullRequest,
    );
    assert!(
        !out.comment.contains("The head pass skipped"),
        "{}",
        out.comment
    );
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert!(
        !out.comment.contains("The head pass skipped"),
        "{}",
        out.comment
    );
    // ... or on a push, which judges the head alone even if its file
    // carries a skip.
    let mut head = head();
    for id in ["session_start_ms"] {
        head = without_metric(&head, id);
    }
    let out = judge(&with_skipped(head, &["session start"]), None, Event::Push);
    assert!(
        !out.comment.contains("The head pass skipped"),
        "{}",
        out.comment
    );
}

#[test]
fn a_skipped_base_leaves_the_head_ten_turns_skip_alone() {
    let headed = with_skipped(head(), &["10 busy turns"]);
    let unselected = with_skipped(base(), &["session start"]);
    let out = judge(&headed, Some(&unselected), Event::PullRequest);
    assert!(
        out.comment.contains(super::judge::TEN_TURNS_SKIP_NOTE),
        "{}",
        out.comment
    );
    assert!(
        out.comment.contains("The base pass skipped session start:"),
        "{}",
        out.comment
    );
}

/// `results` with its `skipped` list set to `names`.
fn with_skipped(mut results: Value, names: &[&str]) -> Value {
    results["skipped"] = json!(names);
    results
}

#[test]
fn the_comment_shows_head_and_base_size_and_the_change() {
    let out = report(
        DOC,
        &with_size(head(), 1000).to_string(),
        Some(Ok(with_size(base(), 900).to_string())),
        None,
        Event::PullRequest,
        "anc0000",
    );
    assert!(
        out.comment.contains("- Head size: 1000 bytes"),
        "{}",
        out.comment
    );
    assert!(
        out.comment.contains("- Base size (anc0000): 900 bytes"),
        "{}",
        out.comment
    );
    assert!(
        out.comment
            .contains("- Size change, head - base: +100 bytes"),
        "{}",
        out.comment
    );
    assert_eq!(out.failures, Vec::<String>::new());

    let out = report(
        DOC,
        &with_size(head(), 900).to_string(),
        Some(Ok(with_size(base(), 1000).to_string())),
        None,
        Event::PullRequest,
        "anc0000",
    );
    assert!(
        out.comment
            .contains("- Size change, head - base: -100 bytes"),
        "{}",
        out.comment
    );

    let out = report(
        DOC,
        &with_size(head(), 1000).to_string(),
        Some(Ok(with_size(base(), 1000).to_string())),
        None,
        Event::PullRequest,
        "anc0000",
    );
    assert!(
        out.comment.contains("- Size change, head - base: +0 bytes"),
        "{}",
        out.comment
    );
}

#[test]
fn a_file_without_a_size_shows_not_recorded() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert!(
        out.comment.contains("- Head size: not recorded"),
        "{}",
        out.comment
    );
    assert!(out.comment.contains("- Base size ("), "{}", out.comment);
    assert!(out.comment.contains(": not recorded"), "{}", out.comment);
    assert!(!out.comment.contains("Size change"), "{}", out.comment);
    assert!(
        !out.failures.iter().any(|f| f.contains("binary_size")),
        "{:?}",
        out.failures
    );

    let out = judge(&with_size(head(), 1000), Some(&base()), Event::PullRequest);
    assert!(
        out.comment.contains("- Head size: 1000 bytes"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("Size change"), "{}", out.comment);

    let out = judge(&head(), Some(&with_size(base(), 900)), Event::PullRequest);
    assert!(
        out.comment.contains("- Head size: not recorded"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("Size change"), "{}", out.comment);
}

#[test]
fn the_base_size_line_names_no_commit_when_none_is_given() {
    let out = report(
        DOC,
        &with_size(head(), 1000).to_string(),
        Some(Ok(with_size(base(), 900).to_string())),
        None,
        Event::PullRequest,
        "",
    );
    assert!(
        out.comment.contains("- Base size: 900 bytes"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("- Base size ("), "{}", out.comment);
}

#[test]
fn a_push_or_a_failed_base_shows_the_head_size_alone() {
    let headed = with_size(head(), 1000);
    let based = with_size(base(), 900);
    let out = report(
        DOC,
        &headed.to_string(),
        Some(Ok(based.to_string())),
        None,
        Event::Push,
        "base0000",
    );
    assert!(
        out.comment.contains("- Head size: 1000 bytes"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("Base size"), "{}", out.comment);
    assert!(!out.comment.contains("Size change"), "{}", out.comment);

    let out = report(
        DOC,
        &headed.to_string(),
        Some(Err("base.json: no such file".to_owned())),
        None,
        Event::PullRequest,
        "base0000",
    );
    assert!(
        out.comment.contains("- Head size: 1000 bytes"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("Base size"), "{}", out.comment);
    assert!(!out.comment.contains("Size change"), "{}", out.comment);
}

#[test]
fn a_malformed_binary_size_fails_the_head() {
    for size in [json!("1000"), json!(-1), json!(1.5), Value::Null] {
        let mut headed = head();
        headed["binary_size_bytes"] = size;
        let out = judge(&headed, None, Event::Push);
        assert!(
            has(
                &out.failures,
                "head results: binary_size_bytes is not a whole number of bytes"
            ),
            "{:?}",
            out.failures
        );
    }
}

/// The two scale-16 paging budgets, by table row, with their small and
/// large metric ids. The row name doubles as the comment lookup: it names
/// the full Budget cell, since the scale-1 rows share its tail.
const RATIO_ROWS: [(&str, &str, &str); 2] = [
    (
        "`paging` jig at scale 16, slowest re-count at a new width",
        "paging_width_ms",
        "paging_scale16_width_ms",
    ),
    (
        "`paging` jig at scale 16, a search of the whole session with no match",
        "paging_search_ms",
        "paging_scale16_search_ms",
    ),
];

/// The head judged with `small`/`large` holding `small_values`/
/// `large_values` for one ratio row.
fn judge_ratio(small: &str, large: &str, small_values: Value, large_values: Value) -> Report {
    let head = with_metric(head(), small, small_values);
    let head = with_metric(head, large, large_values);
    judge(&head, Some(&base()), Event::PullRequest)
}

#[test]
fn paired_ratios_pass_below_and_at_twenty_four_times_and_fail_above() {
    for (budget, small, large) in RATIO_ROWS {
        // Just below, exactly at, and just above: the median pair
        // decides, so constant arrays pin the edge precisely.
        let below = judge_ratio(
            small,
            large,
            json!([10.0, 10.0, 10.0, 10.0, 10.0]),
            json!([239.0, 239.0, 239.0, 239.0, 239.0]),
        );
        assert_eq!(below.failures, Vec::<String>::new(), "{budget}");
        let at = judge_ratio(
            small,
            large,
            json!([10.0, 10.0, 10.0, 10.0, 10.0]),
            json!([240.0, 240.0, 240.0, 240.0, 240.0]),
        );
        assert_eq!(at.failures, Vec::<String>::new(), "{budget} at the ceiling");
        let above = judge_ratio(
            small,
            large,
            json!([10.0, 10.0, 10.0, 10.0, 10.0]),
            json!([241.0, 241.0, 241.0, 241.0, 241.0]),
        );
        assert_eq!(above.failures.len(), 1, "{budget}");
        assert!(
            has(&above.failures, budget) && has(&above.failures, "over"),
            "{budget}: {:?}",
            above.failures
        );
        let line = row(&above.comment, budget);
        assert!(line.ends_with("| fail |"), "{line}");
    }
}

#[test]
fn paired_medians_are_not_ratios_of_medians() {
    // Paired median 2 passes where independent medians read 100.
    let (_, small, large) = RATIO_ROWS[0];
    let out = judge_ratio(
        small,
        large,
        json!([1.0, 2.0, 100.0]),
        json!([1.0, 200.0, 200.0]),
    );
    // Three runs need their declared count: the file declares five.
    assert!(has(&out.failures, "expected 5"), "{:?}", out.failures);
    let three = with_metric_runs(head(), &[(small, 3), (large, 3)]);
    let three = with_metric(three, small, json!([1.0, 2.0, 100.0]));
    let three = with_metric(three, large, json!([1.0, 200.0, 200.0]));
    let out = judge(&three, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    // Paired median over where independent medians read about 1.
    let three = with_metric_runs(head(), &[(small, 3), (large, 3)]);
    let three = with_metric(three, small, json!([1.0, 50.0, 51.0]));
    let three = with_metric(three, large, json!([51.0, 50.0, 1300.0]));
    let out = judge(&three, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(has(&out.failures, "over"), "{:?}", out.failures);
}

#[test]
fn a_changed_ceiling_pin_or_multiplier_fails_naming_the_budget() {
    for (budget, _, _) in RATIO_ROWS {
        let prefix = format!("| {budget} | ");
        let line = DOC
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no {budget} row"));
        let cell = line.split('|').nth(2).unwrap_or_default().trim().to_owned();
        for wrong in [
            cell.replace("24 times", "23 times"),
            cell.replace("24 times", "25 times"),
            cell.replace("same run's", "the run's"),
        ] {
            let doc = doc_with(&cell, &wrong);
            let out = judge_doc(&doc, &head(), Some(&base()), Event::PullRequest);
            assert_eq!(out.failures.len(), 1, "{budget} with {wrong:?}");
            assert!(has(&out.failures, budget), "{:?}", out.failures);
            assert!(has(&out.failures, &wrong), "{:?}", out.failures);
            assert!(has(&out.failures, &cell), "{:?}", out.failures);
            assert!(
                has(&out.failures, "judge.rs"),
                "the failure tells where the formula lives: {:?}",
                out.failures
            );
        }
    }
}

#[test]
fn malformed_paired_samples_are_configuration_failures() {
    let (_, small, large) = RATIO_ROWS[0];
    // Missing, wrong-typed, negative and non-finite samples fail.
    let missing = judge(
        &without_metric(&head(), large),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(has(&missing.failures, large), "{:?}", missing.failures);
    for bad in [
        json!("twelve"),
        json!([10.0, 10.0, 10.0, 10.0, "twelve"]),
        json!([-1.0, 10.0, 10.0, 10.0, 10.0]),
        json!([null, null, null, null, null]),
    ] {
        let out = judge_ratio(small, large, json!([10.0, 10.0, 10.0, 10.0, 10.0]), bad);
        assert!(has(&out.failures, large), "{:?}", out.failures);
    }
    // Unequal lengths fail even when each matches its declared count.
    let uneven = with_metric_runs(head(), &[(large, 3)]);
    let uneven = with_metric(uneven, large, json!([160.0, 160.0, 160.0]));
    let out = judge(&uneven, Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "differ"), "{:?}", out.failures);
    let out = judge_ratio(
        small,
        large,
        json!([10.0, 10.0, 10.0, 10.0]),
        json!([160.0, 160.0, 160.0, 160.0, 160.0]),
    );
    assert!(has(&out.failures, "expected 5"), "{:?}", out.failures);
    // Declared run counts rule: three samples with three declared pass
    // their count validation (and fail here only on the ratio itself).
    let head = with_metric_runs(head(), &[(small, 3), (large, 3)]);
    let head = with_metric(head, small, json!([10.0, 10.0, 10.0]));
    let head = with_metric(head, large, json!([250.0, 250.0, 250.0]));
    let out = judge(&head, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(has(&out.failures, "over"), "{:?}", out.failures);
}

#[test]
fn zero_growth_passes_and_infinite_growth_fails() {
    let (_, small, large) = RATIO_ROWS[0];
    let zeros = judge_ratio(
        small,
        large,
        json!([0.0, 0.0, 0.0, 0.0, 0.0]),
        json!([0.0, 0.0, 0.0, 0.0, 0.0]),
    );
    assert_eq!(zeros.failures, Vec::<String>::new(), "{:?}", zeros.failures);
    let infinite = judge_ratio(
        small,
        large,
        json!([0.0, 0.0, 0.0, 0.0, 0.0]),
        json!([1.0, 1.0, 1.0, 1.0, 1.0]),
    );
    assert_eq!(infinite.failures.len(), 1, "{:?}", infinite.failures);
    assert!(has(&infinite.failures, "over"), "{:?}", infinite.failures);
    // An overflowing bound is a configuration failure, not a pass.
    let overflow = judge_ratio(
        small,
        large,
        json!([1e308, 1e308, 1e308, 1e308, 1e308]),
        json!([1e308, 1e308, 1e308, 1e308, 1e308]),
    );
    assert_eq!(overflow.failures.len(), 1, "{:?}", overflow.failures);
    assert!(
        has(&overflow.failures, "overflow"),
        "{:?}",
        overflow.failures
    );
}

/// The head judged with three runs of `small`/`large`, so run order
/// can disagree with growth order and the median proves the sort.
fn ratio_three(small: &str, large: &str, small_values: Value, large_values: Value) -> Report {
    let head = with_metric_runs(head(), &[(small, 3), (large, 3)]);
    let head = with_metric(head, small, small_values);
    let head = with_metric(head, large, large_values);
    judge(&head, Some(&base()), Event::PullRequest)
}

#[test]
fn ratio_columns_show_the_median_pair_not_just_its_verdict() {
    let (budget, small, large) = RATIO_ROWS[0];
    // 240 over ten, at the ceiling, names its growth in the head column.
    let at = judge_ratio(
        small,
        large,
        json!([10.0, 10.0, 10.0, 10.0, 10.0]),
        json!([240.0, 240.0, 240.0, 240.0, 240.0]),
    );
    assert_eq!(at.failures, Vec::<String>::new(), "{:?}", at.failures);
    let line = row(&at.comment, budget);
    assert!(line.contains("24.0x of 24x allowed"), "{line}");
    // Zero over zero names zero growth, not infinite growth or NaN.
    let zeros = judge_ratio(
        small,
        large,
        json!([0.0, 0.0, 0.0, 0.0, 0.0]),
        json!([0.0, 0.0, 0.0, 0.0, 0.0]),
    );
    assert_eq!(zeros.failures, Vec::<String>::new(), "{:?}", zeros.failures);
    let line = row(&zeros.comment, budget);
    assert!(line.contains("0.0x of 24x allowed"), "{line}");
    // Positive over zero names infinite growth.
    let infinite = judge_ratio(
        small,
        large,
        json!([0.0, 0.0, 0.0, 0.0, 0.0]),
        json!([1.0, 1.0, 1.0, 1.0, 1.0]),
    );
    assert_eq!(infinite.failures.len(), 1, "{:?}", infinite.failures);
    let line = row(&infinite.comment, budget);
    assert!(line.contains("infinite of 24x allowed"), "{line}");
}

#[test]
fn a_present_base_shows_its_ratio_not_unavailable() {
    let (budget, small, large) = RATIO_ROWS[0];
    // A base that ran the workload shows its own median pair, ten over
    // one, in the base column: forcing "unavailable" hides it.
    let base = with_metric(base(), small, json!([1.0, 1.0, 1.0, 1.0, 1.0]));
    let base = with_metric(base, large, json!([10.0, 10.0, 10.0, 10.0, 10.0]));
    let out = judge(&head(), Some(&base), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, budget);
    assert!(line.contains("10.0x of 24x allowed"), "{line}");
    assert!(!line.contains("unavailable"), "{line}");
}

#[test]
fn a_base_missing_one_scale_shows_unavailable() {
    let (budget, small, large) = RATIO_ROWS[0];
    let rich = with_metric(base(), small, json!([1.0, 1.0, 1.0, 1.0, 1.0]));
    let rich = with_metric(rich, large, json!([10.0, 10.0, 10.0, 10.0, 10.0]));
    // Missing either side alone still reads unavailable: judging half a
    // pair would fail the base instead.
    for missing in [small, large] {
        let base = without_metric(&rich, missing);
        let out = judge(&head(), Some(&base), Event::PullRequest);
        assert_eq!(
            out.failures,
            Vec::<String>::new(),
            "{missing}: {:?}",
            out.failures
        );
        let line = row(&out.comment, budget);
        assert!(line.contains("unavailable"), "{missing}: {line}");
    }
}

#[test]
fn paired_ordering_sorts_finite_pairs_by_growth() {
    let (budget, small, large) = RATIO_ROWS[0];
    // Run order disagrees with growth order: the column names the
    // sorted middle (two over one), never the run middle (one over one).
    let out = ratio_three(
        small,
        large,
        json!([1.0, 1.0, 1.0]),
        json!([20.0, 1.0, 2.0]),
    );
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, budget);
    assert!(line.contains("2.0x of 24x allowed"), "{line}");
}

#[test]
fn paired_ordering_ranks_zero_growth_first_and_infinite_last() {
    let (budget, small, large) = RATIO_ROWS[0];
    // An infinite pair sorts after the finite ones whatever the run
    // order: the middle stays the finite pair at three over one.
    let out = ratio_three(small, large, json!([0.0, 1.0, 1.0]), json!([5.0, 2.0, 3.0]));
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, budget);
    assert!(line.contains("3.0x of 24x allowed"), "{line}");
    // A zero pair sorts before the finite ones: the middle stays the
    // finite pair at two over one.
    let out = ratio_three(small, large, json!([0.0, 1.0, 1.0]), json!([0.0, 2.0, 3.0]));
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, budget);
    assert!(line.contains("2.0x of 24x allowed"), "{line}");
}

#[test]
fn overflowed_cross_products_fall_back_to_the_quotient() {
    let (budget, small, large) = RATIO_ROWS[0];
    // Exact powers of two: every cross product overflows, so the median
    // comes from the quotients (two over one). Exact zero remainders or
    // infinite products would instead tie at the run middle (one over one).
    let unit = 2f64.powi(600);
    let out = ratio_three(
        small,
        large,
        json!([unit, unit, unit]),
        json!([4.0 * unit, unit, 2.0 * unit]),
    );
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, budget);
    assert!(line.contains("2.0x of 24x allowed"), "{line}");
}

#[test]
fn a_head_ratio_over_twenty_four_times_fails_whatever_the_base_does() {
    let (_, small, large) = RATIO_ROWS[0];
    let over = |ratio: f64| {
        let head = with_metric(head(), small, json!([10.0, 10.0, 10.0, 10.0, 10.0]));
        with_metric(head, large, json!(vec![ratio * 10.0; 5]))
    };
    // Base below, at and above the ceiling: the head still fails, never
    // marked over at the base.
    for base_ratio in [10.0, 24.0, 30.0] {
        let base = with_metric(base(), small, json!([10.0, 10.0, 10.0, 10.0, 10.0]));
        let base = with_metric(base, large, json!(vec![base_ratio * 10.0; 5]));
        let out = judge(&over(25.0), Some(&base), Event::PullRequest);
        assert_eq!(out.failures.len(), 1, "base at {base_ratio}x");
        assert!(has(&out.failures, "over"), "{:?}", out.failures);
        assert!(!out.comment.contains("over at base"), "{}", out.comment);
    }
    // No base at all, and a stored base over too.
    let out = judge(&over(25.0), None, Event::Push);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    let stored = with_metric(full_base(), small, json!([10.0, 10.0, 10.0, 10.0, 10.0]));
    let stored = with_metric(stored, large, json!(vec![300.0; 5]));
    let out = judge_stored(&over(25.0), &base(), Some(&stored), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(!out.comment.contains("over at base"), "{}", out.comment);
    // A missing head sample fails even with the stored base over: a
    // configuration failure is never suppressed.
    let out = judge_stored(
        &without_metric(&head(), large),
        &base(),
        Some(&stored),
        Event::PullRequest,
    );
    assert!(has(&out.failures, large), "{:?}", out.failures);
}

#[test]
fn a_legacy_base_reports_new_comparisons_unavailable() {
    // The base predates the workload: its original paging timings still
    // compare, while the new ratios show unavailable.
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    for (budget, _, _) in RATIO_ROWS {
        let line = row(&out.comment, budget);
        assert!(line.contains("unavailable"), "{line}");
        assert!(line.ends_with("| pass |"), "{line}");
    }
    let line = row(
        &out.comment,
        "`paging` jig, slowest re-count at a new width",
    );
    assert!(
        line.contains("24.0 ms") && line.contains("20.5 ms"),
        "{line}"
    );
}

#[test]
fn a_legacy_shaped_head_fails_naming_its_missing_metrics() {
    // The same legacy shape as head: without the scale-1 search the
    // search row names it, and without the scale-16 samples both rows
    // name theirs.
    let bare = without_metric(&head(), "paging_search_ms");
    let out = judge(&bare, Some(&base()), Event::PullRequest);
    assert!(has(&out.failures, "paging_search_ms"), "{:?}", out.failures);
    let bare = without_metric(&head(), "paging_scale16_width_ms");
    let bare = without_metric(&bare, "paging_scale16_search_ms");
    let out = judge(&bare, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures.len(), 2, "{:?}", out.failures);
    assert!(
        has(&out.failures, "paging_scale16_width_ms"),
        "{:?}",
        out.failures
    );
    assert!(
        has(&out.failures, "paging_scale16_search_ms"),
        "{:?}",
        out.failures
    );
}

#[test]
fn the_report_names_the_hard_ratio_exception() {
    // Passing ratios carry the new sentence, never the old blanket one;
    // ungated timings stay advisory.
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{}", out.comment);
    assert!(
        out.comment.contains(
            "Exact, memory, paging growth-ratio and gated timing budgets fail the run; timings without an eligible class gate stay advisory with their reason."
        ),
        "{}",
        out.comment
    );
    assert!(
        !out.comment
            .contains("Exact and memory budgets fail the run;"),
        "{}",
        out.comment
    );
    for budget in [
        "`paging` jig, open pass and first frame",
        "`paging` jig, slowest frame that loaded pages",
        "`paging` jig, slowest jump frame",
        "`paging` jig, slowest re-count at a new width",
        "`paging` jig, slowest append frame",
    ] {
        let line = row(&out.comment, budget);
        assert!(line.ends_with("| advisory |"), "{line}");
    }
    // A failing ratio carries the same sentence, and its row fails.
    let (_, small, large) = RATIO_ROWS[0];
    let head = with_metric(head(), small, json!([10.0, 10.0, 10.0, 10.0, 10.0]));
    let head = with_metric(head, large, json!([250.0, 250.0, 250.0, 250.0, 250.0]));
    let out = judge(&head, Some(&base()), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(
        out.comment.contains(
            "Exact, memory, paging growth-ratio and gated timing budgets fail the run; timings without an eligible class gate stay advisory with their reason."
        ),
        "{}",
        out.comment
    );
    let line = row(&out.comment, RATIO_ROWS[0].0);
    assert!(line.ends_with("| fail |"), "{line}");
}

#[test]
fn both_paging_ratio_rows_are_measured_and_scale_one_stays() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    let not_measured = out.comment.split("### Not measured").nth(1).unwrap();
    // Neither scale-16 paging row waits on a ticket any more.
    for (budget, _, _) in RATIO_ROWS {
        assert!(!not_measured.contains(budget), "{not_measured}");
        let line = row(&out.comment, budget);
        assert!(line.ends_with("| pass |"), "{line}");
    }
    // All six scale-1 paging metrics still judge their rows.
    assert!(!not_measured.contains("#581"), "{not_measured}");
    assert!(!not_measured.contains("#2210"), "{not_measured}");
    for budget in [
        "`paging` jig, its session at scale 1 and 160 by 48",
        "`paging` jig, open pass and first frame",
        "`paging` jig, slowest frame that loaded pages",
        "`paging` jig, slowest jump frame",
        "`paging` jig, slowest re-count at a new width",
        "`paging` jig, slowest append frame",
    ] {
        assert!(out.comment.contains(budget), "{budget}:\n{}", out.comment);
    }
}

/// `results` without `id` in its metrics.
fn without_metric(results: &Value, id: &str) -> Value {
    let mut results = results.clone();
    if let Some(metrics) = results.get_mut("metrics").and_then(Value::as_object_mut) {
        metrics.remove(id);
    }
    results
}

#[test]
fn the_median_pair_is_the_middle_pair_not_the_second_run() {
    // Five distinct growths: the middle pair grows 25x and fails while
    // the second pair grows 2x and passes. Indexing the median with `% 2`
    // would read the passing pair and let the over-budget head through:
    // this pins the `/ 2` the median index divides by.
    let (budget, small, large) = RATIO_ROWS[0];
    let out = judge_ratio(
        small,
        large,
        json!([1.0, 1.0, 1.0, 1.0, 1.0]),
        json!([1.0, 2.0, 25.0, 26.0, 27.0]),
    );
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(has(&out.failures, budget), "{:?}", out.failures);
    let line = row(&out.comment, budget);
    assert!(line.contains("25.0x of 24x allowed"), "{line}");
    assert!(line.ends_with("| fail |"), "{line}");
}

// --- #2039 timing gates: calibration, conjunction, fixtures ---

const FAST_MODEL: &str = "AMD EPYC 9V45 96-Core Processor";
const MIDDLE_MODEL: &str = "Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz";
const SLOW_MODEL: &str = "AMD EPYC 7763 64-Core Processor";
const SLOW_ADVISORY_MODEL: &str = "AMD EPYC 9V74 80-Core Processor";
const UNCLASSIFIED_MODEL: &str = "INTEL(R) XEON(R) PLATINUM 8573C";

/// Every scalar timing row, by comment lookup prefix and metric id, with
/// its ruled class ceilings (ms) and fast/slow tolerances (%).
const SCALAR_ROWS: [(&str, &str, f64, f64, f64, f64, f64); 11] = [
    (
        "Session start, the internal session command",
        "session_start_ms",
        16.0,
        12.0,
        13.0,
        55.0,
        9.0,
    ),
    (
        "Terminal to its first frame, new session",
        "terminal_first_frame_ms",
        7.0,
        8.0,
        11.0,
        18.0,
        8.0,
    ),
    (
        "Listing 1,000 sessions",
        "sessions_list_ms",
        32.0,
        34.0,
        41.0,
        21.0,
        13.0,
    ),
    (
        "`session_list`, waiting for every running session's status",
        "session_list_ms",
        1.0,
        1.0,
        72.0,
        0.0,
        0.0,
    ),
    (
        "`paging` jig, open pass",
        "paging_open_ms",
        725.0,
        828.0,
        1049.0,
        11.0,
        4.0,
    ),
    (
        "`paging` jig, slowest frame that loaded",
        "paging_load_ms",
        8.0,
        5.0,
        10.0,
        16.0,
        4.0,
    ),
    (
        "`paging` jig, slowest jump",
        "paging_jump_ms",
        9.0,
        9.0,
        13.0,
        11.0,
        5.0,
    ),
    (
        "`paging` jig, slowest re-count at a new width",
        "paging_width_ms",
        67.0,
        77.0,
        99.0,
        16.0,
        8.0,
    ),
    (
        "`paging` jig, slowest append",
        "paging_append_ms",
        7.0,
        4.0,
        6.0,
        56.0,
        32.0,
    ),
    (
        "Terminal, wheel to frame, the `paging` jig's session",
        "wheel_paging_ms",
        7.0,
        6.0,
        9.0,
        21.0,
        12.0,
    ),
    (
        "Terminal, wheel to frame, one turn with an opened `read`",
        "wheel_read_ms",
        5.0,
        4.0,
        7.0,
        40.0,
        23.0,
    ),
];

fn neutral_scalars(target: &str, h: Value) -> Value {
    let b = base();
    let mut h = h;
    for (_, other, _, _, _, _, _) in SCALAR_ROWS {
        if other != target {
            h["metrics"][other] = b["metrics"][other].clone();
        }
    }
    h
}

fn scalar_pair(id: &str, head_vals: Value, base_vals: Value, cpu: &str) -> (Value, Value) {
    let h = with_cpu(with_metric(neutral_scalars(id, head()), id, head_vals), cpu);
    let b = with_cpu(with_metric(base(), id, base_vals), cpu);
    (h, b)
}

fn attach_pair(
    ten_head: &[f64],
    one_head: &[f64],
    ten_base: &[f64],
    one_base: &[f64],
    cpu: &str,
) -> (Value, Value) {
    let mut head_entries = attach_entries("1 MiB", 1_050_231, one_head);
    head_entries.extend(attach_entries("10 MiB", 10_492_016, ten_head));
    let mut base_entries = attach_entries("1 MiB", 1_050_100, one_base);
    base_entries.extend(attach_entries("10 MiB", 10_490_000, ten_base));
    let h = with_cpu(
        with_metric(
            neutral_scalars("terminal_attach_ms", head()),
            "terminal_attach_ms",
            Value::Array(head_entries),
        ),
        cpu,
    );
    let b = with_cpu(
        with_metric(base(), "terminal_attach_ms", Value::Array(base_entries)),
        cpu,
    );
    (h, b)
}

#[test]
fn class_ceiling_cells_carry_the_ruled_figures() {
    for (budget, _, fast, middle, slow, fast_tol, slow_tol) in SCALAR_ROWS {
        let prefix = format!("| {budget}");
        let line = DOC
            .lines()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no {budget} row in DOC"));
        for want in [
            format!("{fast:.0} ms"),
            format!("{middle:.0} ms"),
            format!("{slow:.0} ms"),
            format!("{fast_tol:.0} %"),
            format!("{slow_tol:.0} %"),
        ] {
            assert!(line.contains(&want), "{budget}: {line} misses {want}");
        }
    }
    let attach = DOC
        .lines()
        .find(|l| l.starts_with("| Terminal to its first frame, attaching"))
        .unwrap();
    for want in ["1008 ms", "1101 ms", "1364 ms", "23 %", "4 %"] {
        assert!(attach.contains(want), "{attach} misses {want}");
    }
}

#[test]
fn eligible_models_gate_and_advisory_models_stay_advisory() {
    for (budget, id, _, _, _, _, _) in SCALAR_ROWS {
        // Head far over every ceiling, base near zero: both legs exceed.
        let (eligible_h, eligible_b) = scalar_pair(
            id,
            json!([2000.0, 2001.0, 1999.0, 2002.0, 1998.0]),
            json!([1.0, 1.1, 0.9, 1.0, 1.0]),
            SLOW_MODEL,
        );
        let out = judge(&eligible_h, Some(&eligible_b), Event::PullRequest);
        assert_eq!(out.failures.len(), 1, "{budget}: {:?}", out.failures);
        assert!(has(&out.failures, budget), "{budget}: {:?}", out.failures);
        assert!(
            row(&out.comment, budget).ends_with("| fail |"),
            "{}",
            out.comment
        );
        let (eligible_h, eligible_b) = scalar_pair(
            id,
            json!([2000.0, 2001.0, 1999.0, 2002.0, 1998.0]),
            json!([1.0, 1.1, 0.9, 1.0, 1.0]),
            FAST_MODEL,
        );
        let out = judge(&eligible_h, Some(&eligible_b), Event::PullRequest);
        assert_eq!(out.failures.len(), 1, "{budget} fast: {:?}", out.failures);

        for (model, count) in [
            (SLOW_ADVISORY_MODEL, "4 stored runs"),
            (MIDDLE_MODEL, "2 stored runs"),
            (UNCLASSIFIED_MODEL, "1 stored run"),
        ] {
            let (h, b) = scalar_pair(
                id,
                json!([2000.0, 2001.0, 1999.0, 2002.0, 1998.0]),
                json!([1.0, 1.1, 0.9, 1.0, 1.0]),
                model,
            );
            let out = judge(&h, Some(&b), Event::PullRequest);
            assert_eq!(
                out.failures,
                Vec::<String>::new(),
                "{budget} {model}: {:?}",
                out.failures
            );
            let line = row(&out.comment, budget);
            assert!(line.ends_with("| advisory |"), "{budget} {model}: {line}");
            assert!(line.contains("2000.0 ms"), "{budget} {model}: {line}");
            assert!(line.contains(count), "{budget} {model}: {line}");
        }
        // Unknown, missing and empty CPU keys stay advisory.
        let (h, b) = scalar_pair(
            id,
            json!(vec![2000.0; 5]),
            json!(vec![1.0; 5]),
            "Some Other CPU 1234",
        );
        let out = judge(&h, Some(&b), Event::PullRequest);
        assert_eq!(out.failures, Vec::<String>::new(), "{budget} unknown");
        assert!(
            row(&out.comment, budget).contains("unknown or not recorded"),
            "{}",
            out.comment
        );
        let h = with_metric(head(), id, json!(vec![2000.0; 5]));
        let b = with_metric(base(), id, json!(vec![1.0; 5]));
        let out = judge(&h, Some(&b), Event::PullRequest);
        assert_eq!(out.failures, Vec::<String>::new(), "{budget} missing cpu");
        assert!(
            row(&out.comment, budget).contains("unknown or not recorded"),
            "{}",
            out.comment
        );
        let (h, b) = scalar_pair(id, json!(vec![2000.0; 5]), json!(vec![1.0; 5]), "");
        let out = judge(&h, Some(&b), Event::PullRequest);
        assert_eq!(out.failures, Vec::<String>::new(), "{budget} empty cpu");
    }
}

#[test]
fn model_matching_is_case_insensitive_but_not_a_substring() {
    let id = "session_start_ms";
    let upper = SLOW_MODEL.to_ascii_uppercase();
    let (h, b) = scalar_pair(id, json!(vec![2000.0; 5]), json!(vec![1.0; 5]), &upper);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(
        out.failures.len(),
        1,
        "upper-case 7763 should gate: {:?}",
        out.failures
    );
    // A prefix alone is unknown, never a gate.
    let (h, b) = scalar_pair(
        id,
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        "AMD EPYC 7763",
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("unknown or not recorded"),
        "{}",
        out.comment
    );
    let (h, b) = scalar_pair(
        id,
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        "xx AMD EPYC 7763 64-Core Processor xx",
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    // Mixed-case head and base keys compare normally.
    let h = with_cpu(
        with_metric(
            neutral_scalars(id, head()),
            id,
            json!([6.0, 6.2, 5.8, 6.1, 5.9]),
        ),
        SLOW_MODEL,
    );
    let b = with_cpu(
        with_metric(base(), id, json!([6.0, 6.2, 5.8, 6.1, 5.9])),
        &upper,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(
        out.failures,
        Vec::<String>::new(),
        "case-only base difference compares: {:?}",
        out.failures
    );
    assert!(
        row(&out.comment, "Session start").ends_with("| pass |"),
        "{}",
        out.comment
    );
}

#[test]
fn malformed_class_cells_are_configuration_failures() {
    let id = "session_start_ms";
    let (h, b) = scalar_pair(id, json!(vec![2000.0; 5]), json!(vec![1.0; 5]), SLOW_MODEL);
    // Missing class cells: drop the appended columns from the row.
    let short = DOC.lines().map(|l| if l.starts_with("| Session start, the internal") { "| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked |\n".to_owned() } else { format!("{l}\n") }).collect::<String>();
    let out = judge_doc(
        &format!(
            "# Performance\n\n## Budgets\n\n{short}\n## Measuring\n\n| Not | A budget |\n|---|---|\n| x | y |\n"
        ),
        &h,
        Some(&b),
        Event::PullRequest,
    );
    assert!(has(&out.failures, "Session start"), "{:?}", out.failures);
    // Wrong units, negative and non-finite ceilings fail.
    for bad in ["16 s", "16 KiB", "-16 ms", "NaN ms", "inf ms", "—"] {
        let doc = doc_with(
            "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
            &format!("| {bad} | 12 ms | 13 ms | 55 % | 9 % |"),
        );
        let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
        assert!(
            has(&out.failures, "Session start"),
            "{bad}: {:?}",
            out.failures
        );
    }
    // Missing and malformed tolerances fail.
    for bad in ["55", "-55 %", "NaN %", "—"] {
        let doc = doc_with(
            "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
            &format!("| 16 ms | 12 ms | 13 ms | {bad} | 9 % |"),
        );
        let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
        assert!(
            has(&out.failures, "Session start"),
            "{bad}: {:?}",
            out.failures
        );
    }
    // A malformed calibration fails even for an advisory model, never
    // silently advisory.
    let (h, b) = scalar_pair(
        id,
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        SLOW_ADVISORY_MODEL,
    );
    let doc = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| — | 12 ms | 13 ms | 55 % | 9 % |",
    );
    let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
    assert!(has(&out.failures, "Session start"), "{:?}", out.failures);
}

#[test]
fn the_conjunction_needs_both_legs_strictly_over() {
    // Synthetic 100 ms ceiling with 25% tolerance on the session row.
    let doc = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| 100 ms | 100 ms | 100 ms | 25 % | 25 % |",
    );
    let base80 = || json!([79.0, 81.0, 80.0, 82.0, 78.0]);
    for (head_vals, median, fails) in [
        (json!([98.0, 99.0, 101.0, 97.0, 100.0]), 99.0, false),
        (json!([99.0, 100.0, 102.0, 98.0, 101.0]), 100.0, false),
        (json!([100.0, 101.0, 103.0, 99.0, 102.0]), 101.0, true),
    ] {
        let (h, b) = scalar_pair("session_start_ms", head_vals, base80(), SLOW_MODEL);
        let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
        assert_eq!(
            !out.failures.is_empty(),
            fails,
            "head {median}: {:?}",
            out.failures
        );
        let line = row(&out.comment, "Session start");
        assert_eq!(line.ends_with("| fail |"), fails, "{line}");
        if fails {
            assert!(has(&out.failures, "session_start_ms"), "{:?}", out.failures);
            assert!(has(&out.failures, "over"), "{:?}", out.failures);
        }
    }
    // Hold base at 100 ms above a lower 50 ms ceiling: the relative leg decides.
    let doc = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| 50 ms | 50 ms | 50 ms | 25 % | 25 % |",
    );
    let base100 = || json!([99.0, 101.0, 100.0, 102.0, 98.0]);
    for (head_vals, median, fails) in [
        (json!([123.0, 124.0, 126.0, 122.0, 125.0]), 124.0, false),
        (json!([124.0, 125.0, 127.0, 123.0, 126.0]), 125.0, false),
        (json!([125.0, 126.0, 128.0, 124.0, 127.0]), 126.0, true),
    ] {
        let (h, b) = scalar_pair("session_start_ms", head_vals, base100(), SLOW_MODEL);
        let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
        assert_eq!(
            !out.failures.is_empty(),
            fails,
            "head {median}: {:?}",
            out.failures
        );
    }
    // Ceiling exceeded but within tolerance passes; tolerance exceeded but
    // under the ceiling passes.
    let doc = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| 100 ms | 100 ms | 100 ms | 25 % | 25 % |",
    );
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!(vec![101.0; 5]),
        json!(vec![100.0; 5]),
        SLOW_MODEL,
    );
    let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
    assert_eq!(
        out.failures,
        Vec::<String>::new(),
        "over ceiling but within tolerance: {:?}",
        out.failures
    );
    assert!(
        row(&out.comment, "Session start").ends_with("| pass |"),
        "{}",
        out.comment
    );
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!(vec![90.0; 5]),
        json!(vec![50.0; 5]),
        SLOW_MODEL,
    );
    let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
    assert_eq!(
        out.failures,
        Vec::<String>::new(),
        "over relative bound but under ceiling: {:?}",
        out.failures
    );
}

#[test]
fn decisions_follow_the_doc_value_not_a_hard_coded_limit() {
    let id = "session_start_ms";
    let (h, b) = scalar_pair(
        id,
        json!([100.0, 101.0, 103.0, 99.0, 102.0]),
        json!([79.0, 81.0, 80.0, 82.0, 78.0]),
        SLOW_MODEL,
    );
    let tight = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| 100 ms | 100 ms | 100 ms | 25 % | 25 % |",
    );
    let out = judge_doc(&tight, &h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    let loose = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| 102 ms | 102 ms | 102 ms | 25 % | 25 % |",
    );
    let out = judge_doc(&loose, &h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    assert!(
        row(&out.comment, "Session start").ends_with("| pass |"),
        "{}",
        out.comment
    );
}

#[test]
fn unavailable_bases_stay_advisory_with_their_reason() {
    let id = "session_start_ms";
    let headed = || {
        with_cpu(
            with_metric(neutral_scalars(id, head()), id, json!(vec![2000.0; 5])),
            SLOW_MODEL,
        )
    };
    // A push has no base column and stays advisory.
    let out = judge(&headed(), None, Event::Push);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("no comparable same-job base timing"),
        "{}",
        out.comment
    );
    // A pull request without --base is still a configuration failure.
    let out = judge(&headed(), None, Event::PullRequest);
    assert!(has(&out.failures, "--base"), "{:?}", out.failures);
    // A failed base stays advisory and shows why.
    let mut failed = base();
    failed["failures"] = json!(["base broke"]);
    let failed = with_cpu(failed, SLOW_MODEL);
    let out = judge(&headed(), Some(&failed), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("base failed: base broke"),
        "{}",
        out.comment
    );
    // A base predating the workload shows unavailable, never a failure.
    let mut missing = base();
    missing["metrics"].as_object_mut().unwrap().remove(id);
    let missing = with_cpu(missing, SLOW_MODEL);
    let out = judge(&headed(), Some(&missing), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let line = row(&out.comment, "Session start");
    assert!(line.contains("unavailable"), "{line}");
    assert!(line.ends_with("| advisory |"), "{line}");
    // A malformed base metric is unavailable-for-comparison, not a head overage.
    let bad = with_cpu(with_metric(base(), id, json!("slow")), SLOW_MODEL);
    let out = judge(&headed(), Some(&bad), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("base failed:"),
        "{}",
        out.comment
    );
    // An absent base CPU key stays advisory.
    let plain_base = with_metric(base(), id, json!([1.0, 1.1, 0.9, 1.0, 1.0]));
    let out = judge(&headed(), Some(&plain_base), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("no comparable same-job base timing"),
        "{}",
        out.comment
    );
    // A mismatched base model stays advisory.
    let other = with_cpu(with_metric(base(), id, json!(vec![1.0; 5])), FAST_MODEL);
    let out = judge(&headed(), Some(&other), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    assert!(
        row(&out.comment, "Session start").contains("no comparable same-job base timing"),
        "{}",
        out.comment
    );
}

#[test]
fn zero_and_overflow_edges_hold() {
    let doc = doc_with(
        "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
        "| 100 ms | 100 ms | 100 ms | 25 % | 25 % |",
    );
    // Zero base is valid: a positive head over the ceiling fails.
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!([100.0, 101.0, 103.0, 99.0, 102.0]),
        json!(vec![0.0; 5]),
        SLOW_MODEL,
    );
    let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    // A zero head never fails.
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!(vec![0.0; 5]),
        json!(vec![0.0; 5]),
        SLOW_MODEL,
    );
    let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    // An overflowing relative bound is a configuration failure, not a pass:
    // 1.7e308 is finite, but times 1.25 overflows f64.
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!(vec![200.0; 5]),
        json!(vec![1.7e308; 5]),
        SLOW_MODEL,
    );
    let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(has(&out.failures, "overflow"), "{:?}", out.failures);
}

#[test]
fn malformed_head_timings_fail_even_on_advisory_models() {
    let id = "session_start_ms";
    for cpu in [SLOW_MODEL, SLOW_ADVISORY_MODEL, "unknown cpu"] {
        let mut missing = with_cpu(head(), cpu);
        missing["metrics"].as_object_mut().unwrap().remove(id);
        let b = with_cpu(with_metric(base(), id, json!(vec![1.0; 5])), cpu);
        let out = judge(&missing, Some(&b), Event::PullRequest);
        assert!(has(&out.failures, id), "{cpu}: {:?}", out.failures);
        for bad in [
            json!("slow"),
            json!([1.0, 2.0]),
            json!([-1.0, 1.0, 1.0, 1.0, 1.0]),
            json!([f64::NAN, 1.0, 1.0, 1.0, 1.0]),
        ] {
            let h = with_cpu(with_metric(head(), id, bad), cpu);
            let out = judge(&h, Some(&b), Event::PullRequest);
            assert!(has(&out.failures, id), "{cpu}: {:?}", out.failures);
        }
    }
}

#[test]
fn stored_timings_neither_gate_nor_waive_a_live_timing_failure() {
    let id = "session_start_ms";
    // Live head over both legs fails even when stored is well under.
    let (h, b) = scalar_pair(id, json!(vec![2000.0; 5]), json!(vec![1.0; 5]), SLOW_MODEL);
    let calm = with_metric(full_base(), id, json!(vec![1.0; 5]));
    let out = judge_stored(&h, &b, Some(&calm), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    // Live head passing stays passing even when stored is far over.
    let (h, b) = scalar_pair(
        id,
        json!([6.0, 6.2, 5.8, 6.1, 5.9]),
        json!([6.0, 6.2, 5.8, 6.1, 5.9]),
        SLOW_MODEL,
    );
    let hot = with_metric(full_base(), id, json!(vec![2000.0; 5]));
    let out = judge_stored(&h, &b, Some(&hot), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
}

#[test]
fn three_sample_pr_files_gate_on_stored_evidence_not_sample_count() {
    let id = "session_start_ms";
    // Three declared samples on an eligible model still gates.
    let h = with_cpu(
        with_metric(
            with_metric_runs(neutral_scalars(id, head()), &[(id, 3)]),
            id,
            json!([2000.0, 2001.0, 1999.0]),
        ),
        SLOW_MODEL,
    );
    let b = with_cpu(
        with_metric(
            with_metric_runs(base(), &[(id, 3)]),
            id,
            json!([1.0, 1.1, 0.9]),
        ),
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    // Three samples on a 4-file model do not promote it.
    let h = with_cpu(
        with_metric(
            with_metric_runs(neutral_scalars(id, head()), &[(id, 3)]),
            id,
            json!([2000.0, 2001.0, 1999.0]),
        ),
        SLOW_ADVISORY_MODEL,
    );
    let b = with_cpu(
        with_metric(
            with_metric_runs(base(), &[(id, 3)]),
            id,
            json!([1.0, 1.1, 0.9]),
        ),
        SLOW_ADVISORY_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
}

#[test]
fn only_the_attach_ten_mib_fixture_gates() {
    // Both legs over on 10 MiB fails, naming the fixture.
    let (h, b) = attach_pair(
        &[1400.0, 1401.0, 1399.0, 1402.0, 1398.0],
        &[31.0, 31.2, 30.8, 31.1, 30.9],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(has(&out.failures, "10 MiB"), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.ends_with("| fail |"), "{line}");
    assert!(line.contains("1364.0 ms"), "{line}");
    assert!(line.contains("#1546"), "{line}");
    // At either exact boundary passes: head at ceiling, and head at the
    // relative bound.
    let (h, b) = attach_pair(
        &[1364.0; 5],
        &[31.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    assert!(
        row(&out.comment, "Terminal to its first frame, attaching").ends_with("| pass |"),
        "{}",
        out.comment
    );
    // Above the ceiling but within tolerance passes.
    let (h, b) = attach_pair(
        &[1400.0; 5],
        &[31.0; 5],
        &[1400.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(
        out.failures,
        Vec::<String>::new(),
        "within 4% of base: {:?}",
        out.failures
    );
    // 1 MiB far over both bounds while 10 MiB passes: no failure and a
    // positive 1 MiB advisory naming #1546.
    let (h, b) = attach_pair(
        &[800.0; 5],
        &[5000.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.contains("1 MiB") && line.contains("#1546"), "{line}");
    assert!(line.ends_with("| pass |"), "{line}");
    // Both fixtures over: only 10 MiB creates the overage.
    let (h, b) = attach_pair(
        &[1400.0; 5],
        &[5000.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(has(&out.failures, "10 MiB"), "{:?}", out.failures);
    assert!(!has(&out.failures, "1 MiB"), "{:?}", out.failures);
}

#[test]
fn attach_fixture_errors_fail_and_stages_stay_diagnostic() {
    // Mixed entry ordering still judges per-fixture medians.
    let mut entries = attach_entries("10 MiB", 10_492_016, &[1400.0; 5]);
    entries.extend(attach_entries("1 MiB", 1_050_231, &[31.0; 5]));
    let h = with_cpu(
        with_metric(
            neutral_scalars("terminal_attach_ms", head()),
            "terminal_attach_ms",
            Value::Array(entries),
        ),
        SLOW_MODEL,
    );
    let (_, b) = attach_pair(&[800.0; 5], &[30.5; 5], &[800.0; 5], &[30.5; 5], SLOW_MODEL);
    let (_, b_only) = attach_pair(&[800.0; 5], &[30.5; 5], &[800.0; 5], &[30.5; 5], SLOW_MODEL);
    let out = judge(&h, Some(&b_only), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    let _ = b;
    // Missing and unknown fixtures, missing bytes/ms and wrong counts fail.
    for build in [
        Value::Array(attach_entries("1 MiB", 1_050_231, &[31.0; 5])),
        Value::Array({
            let mut e = attach_entries("1 MiB", 1_050_231, &[31.0; 5]);
            e.extend(attach_entries("2 MiB", 2_097_152, &[15.0; 5]));
            e
        }),
        Value::Array({
            let mut e = attach_entries("1 MiB", 1_050_231, &[31.0; 5]);
            e.extend(vec![json!({"fixture": "10 MiB", "ms": 20.0}); 5]);
            e
        }),
        Value::Array({
            let mut e = attach_entries("1 MiB", 1_050_231, &[31.0; 4]);
            e.extend(attach_entries("10 MiB", 10_492_016, &[88.0; 5]));
            e
        }),
    ] {
        let h = with_cpu(
            with_metric(
                neutral_scalars("terminal_attach_ms", head()),
                "terminal_attach_ms",
                build,
            ),
            SLOW_MODEL,
        );
        let (_, b) = attach_pair(&[800.0; 5], &[30.5; 5], &[800.0; 5], &[30.5; 5], SLOW_MODEL);
        let out = judge(&h, Some(&b), Event::PullRequest);
        assert!(
            has(&out.failures, "terminal_attach_ms"),
            "{:?}",
            out.failures
        );
    }
    // A negative or non-finite ms fails even on an advisory model.
    for ms in [json!(-1.0), json!(f64::NAN)] {
        let mut entries = attach_entries("1 MiB", 1_050_231, &[31.0; 4]);
        entries.push(json!({"fixture": "1 MiB", "log_bytes": 1_050_231, "ms": ms}));
        entries.extend(attach_entries("10 MiB", 10_492_016, &[88.0; 5]));
        let h = with_cpu(
            with_metric(
                neutral_scalars("terminal_attach_ms", head()),
                "terminal_attach_ms",
                Value::Array(entries),
            ),
            SLOW_ADVISORY_MODEL,
        );
        let (_, b) = attach_pair(
            &[800.0; 5],
            &[30.5; 5],
            &[800.0; 5],
            &[30.5; 5],
            SLOW_ADVISORY_MODEL,
        );
        let out = judge(&h, Some(&b), Event::PullRequest);
        assert!(
            has(&out.failures, "terminal_attach_ms"),
            "{:?}",
            out.failures
        );
    }
    // A malformed base fixture stays advisory, never a head overage.
    let (h, _) = attach_pair(
        &[1400.0; 5],
        &[31.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let mut bad_base = base();
    bad_base["metrics"]["terminal_attach_ms"] = json!("slow");
    let bad_base = with_cpu(bad_base, SLOW_MODEL);
    let out = judge(&h, Some(&bad_base), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    // Attach stages never gate: perturbing them leaves the verdict.
    let (h, b) = attach_pair(&[800.0; 5], &[31.0; 5], &[800.0; 5], &[30.5; 5], SLOW_MODEL);
    let staged = with_metric(
        h,
        "attach_stage_ms",
        json!(vec![
            json!({"fixture": "10 MiB", "log_bytes": 10_492_016, "stage": "frames_64", "frames": 229, "ms": 9999.0});
            5
        ]),
    );
    let out = judge(&staged, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    // Three declared samples judge three per fixture.
    let declared = with_metric_runs(
        neutral_scalars("terminal_attach_ms", head()),
        &[("terminal_attach_ms", 3)],
    );
    let trimmed = trim_attach(&declared, 3);
    let h = with_cpu(
        with_metric(
            declared,
            "terminal_attach_ms",
            trimmed["metrics"]["terminal_attach_ms"].clone(),
        ),
        SLOW_MODEL,
    );
    let b =
        with_cpu(
            with_metric(
                with_metric_runs(base(), &[("terminal_attach_ms", 3)]),
                "terminal_attach_ms",
                trim_attach(&with_metric_runs(base(), &[("terminal_attach_ms", 3)]), 3)["metrics"]
                    ["terminal_attach_ms"]
                    .clone(),
            ),
            SLOW_MODEL,
        );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
}

#[test]
fn the_comment_names_gated_timing_verdicts_and_reasons() {
    let id = "session_start_ms";
    // A gated failure names the marker, both medians, the class ceiling,
    // the tolerance and the failure.
    let (h, b) = scalar_pair(id, json!(vec![2000.0; 5]), json!(vec![1.0; 5]), SLOW_MODEL);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert_eq!(out.comment.lines().next(), Some(MARKER));
    let line = row(&out.comment, "Session start");
    assert!(line.contains("2000.0 ms"), "{line}");
    assert!(line.contains("1.0 ms"), "{line}");
    assert!(line.contains("slow ceiling 13.0 ms"), "{line}");
    assert!(line.contains("+9% over base"), "{line}");
    assert!(line.ends_with("| fail |"), "{line}");
    // A gated pass carries the same ceiling and tolerance, marked pass.
    let (h, b) = scalar_pair(
        id,
        json!([6.0, 6.2, 5.8, 6.1, 5.9]),
        json!([6.0, 6.2, 5.8, 6.1, 5.9]),
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Session start");
    assert!(line.contains("slow ceiling 13.0 ms"), "{line}");
    assert!(line.contains("+9% over base"), "{line}");
    assert!(line.ends_with("| pass |"), "{line}");
    // An advisory row names its reason and stays out of the failures.
    let (h, b) = scalar_pair(
        id,
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        SLOW_ADVISORY_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new());
    let line = row(&out.comment, "Session start");
    assert!(line.contains("2000.0 ms"), "{line}");
    assert!(line.contains("4 stored runs"), "{line}");
    assert!(line.ends_with("| advisory |"), "{line}");
    // Attach rows show both the active class ceiling and the 1 MiB reason.
    let (h, b) = attach_pair(&[800.0; 5], &[31.0; 5], &[800.0; 5], &[30.5; 5], SLOW_MODEL);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.contains("slow ceiling 1364.0 ms"), "{line}");
    assert!(line.contains("#1546"), "{line}");
}

// --- #2039 mutant-killing tests (see #2039) ---
//
// Each test kills one surviving mutant from the timing-gate diff: the
// comment names the mutant so the next mapping round can match it.

#[test]
fn unclassified_model_uses_the_singular_stored_run() {
    // Kills judge.rs:126 `==` with `!=` in `model_advisory`: the mutant
    // renders "1 stored runs;", which still contains the bare
    // "1 stored run" substring every older assertion checks, so this
    // pins the semicolon-terminated singular.
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        UNCLASSIFIED_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Session start");
    assert!(line.contains("1 stored run;"), "{line}");
    assert!(line.ends_with("| advisory |"), "{line}");
    // Just above the singular edge: two stored runs stay plural (the
    // mutant renders "2 stored run;" instead).
    let (h, b) = scalar_pair(
        "session_start_ms",
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        MIDDLE_MODEL,
    );
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Session start");
    assert!(line.contains("2 stored runs;"), "{line}");
}

#[test]
fn attach_ms_of_zero_passes_validation() {
    // Kills two `attach_medians` mutants of the lower-bound check:
    // `<` with `==` rejects exactly 0.0, which no older test exercises
    // (only positive, negative and NaN ms); `<` with `<=` rejects exactly
    // 0.0 while keeping the negative rejection, so only a 0.0 fixture
    // bites it.
    let (h, b) = attach_pair(&[0.0; 5], &[0.0; 5], &[0.0; 5], &[0.0; 5], SLOW_MODEL);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.ends_with("| pass |"), "{line}");
    // Just above zero passes; just below fails as a configuration error.
    let (h, b) = attach_pair(&[0.1; 5], &[0.1; 5], &[0.1; 5], &[0.1; 5], SLOW_MODEL);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let (h, b) = attach_pair(&[-0.1; 5], &[0.1; 5], &[0.1; 5], &[0.1; 5], SLOW_MODEL);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert!(
        has(&out.failures, "terminal_attach_ms"),
        "{:?}",
        out.failures
    );
}

/// An over-ceiling 10 MiB head on `SLOW_MODEL` with a valid but
/// mismatched-model base: the row stays advisory because the base cannot
/// establish the same-job premise.
fn mismatched_attach_pair() -> (Value, Value) {
    let (h, _) = attach_pair(
        &[1400.0; 5],
        &[31.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let (_, b) = attach_pair(
        &[1400.0; 5],
        &[31.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        FAST_MODEL,
    );
    (h, b)
}

#[test]
fn a_mismatched_attach_base_never_forces_the_gate() {
    // Kills three same-job-base guard mutants, each of which gates on the
    // mismatched base and fails naming 10 MiB: the guard with `true`; the
    // second `&&` with `||`, whose (contains && is_ok) side is true for
    // the mismatched valid base; and the first `&&` with `||`, whose
    // contains side is true for the mismatched valid base.
    let (h, b) = mismatched_attach_pair();
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.ends_with("| advisory |"), "{line}");
    assert!(
        line.contains("no comparable same-job base timing"),
        "{line}"
    );
}

/// The base with `failures` recorded, on `cpu`: `report` reads it as a
/// failed base.
fn failed_base(cpu: &str) -> Value {
    let mut failed = base();
    failed["failures"] = json!(["base broke"]);
    with_cpu(failed, cpu)
}

#[test]
fn failed_attach_base_stays_advisory() {
    // Kills judge.rs:1031's deleted `(Base::Failed(_), _)` arm: the mutant
    // falls through to gating against 0.0 and fails naming 10 MiB.
    let (h, _) = attach_pair(
        &[1400.0; 5],
        &[31.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&failed_base(SLOW_MODEL)), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.ends_with("| advisory |"), "{line}");
}

#[test]
fn failed_attach_base_shows_its_failure() {
    // Pins judge.rs:1010's failed-base arm (the `!matches!(base,
    // Base::Failed(_))` guard was redundant: the arm patterns already
    // exclude a failed base, so replacing the guard with `true` changes
    // nothing observable and the arm is collapsed without it): the head
    // cell names the active ceiling and the base failure instead of the
    // no-comparable-timing reason.
    let (h, _) = attach_pair(&[800.0; 5], &[31.0; 5], &[800.0; 5], &[30.5; 5], SLOW_MODEL);
    let out = judge(&h, Some(&failed_base(SLOW_MODEL)), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(
        line.contains("slow ceiling 1364.0 ms; base failed: base broke"),
        "{line}"
    );
    assert!(
        !line.contains("no comparable same-job base timing"),
        "{line}"
    );
}

#[test]
fn missing_attach_base_metric_stays_advisory() {
    // Kills two advisory-arm mutants, each of which gates the 1400 ms
    // head against 0.0 and fails naming 10 MiB: the guard with `false`,
    // which skips the advisory arm; and the deleted `!`, which admits
    // only failed bases to the advisory arm.
    let (h, _) = attach_pair(
        &[1400.0; 5],
        &[31.0; 5],
        &[800.0; 5],
        &[30.5; 5],
        SLOW_MODEL,
    );
    let mut missing = base();
    missing["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("terminal_attach_ms");
    let missing = with_cpu(missing, SLOW_MODEL);
    let out = judge(&h, Some(&missing), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Terminal to its first frame, attaching");
    assert!(line.ends_with("| advisory |"), "{line}");
    assert!(
        line.contains("no comparable same-job base timing"),
        "{line}"
    );
}

#[test]
fn failed_scalar_base_carries_no_unavailable_suffix() {
    // Pins judge.rs:1135's failed-base arm (the `!matches!(base,
    // Base::Failed(_))` guard was redundant like its attach twin, so
    // replacing it with `true` changes nothing observable and the arm is
    // collapsed without it): a failed base stays advisory showing its
    // failure, never the no-comparable-timing suffix.
    let (h, _) = scalar_pair(
        "session_start_ms",
        json!(vec![2000.0; 5]),
        json!(vec![1.0; 5]),
        SLOW_MODEL,
    );
    let out = judge(&h, Some(&failed_base(SLOW_MODEL)), Event::PullRequest);
    assert_eq!(out.failures, Vec::<String>::new(), "{:?}", out.failures);
    let line = row(&out.comment, "Session start");
    assert!(line.contains("2000.0 ms"), "{line}");
    assert!(line.contains("base failed: base broke"), "{line}");
    assert!(
        !line.contains("no comparable same-job base timing"),
        "{line}"
    );
    assert!(line.ends_with("| advisory |"), "{line}");
}

#[test]
fn rejected_slow_ceilings_name_the_parse_failure() {
    // A class ceiling must be a finite number at or above zero: the
    // report reads every gate number from the doc table, so a malformed
    // calibration cell is a configuration failure naming the cell, never
    // a silent pass or a timing overage. Each case sits in the slow
    // column the eligible head model actually uses. This kills M1 (the
    // `&&` weakened to `||`, which accepts the negative and +inf) and
    // M3 (the guard dropped, which accepts all four): under either
    // mutant the named parse failure disappears into a pass or an
    // overage.
    let id = "session_start_ms";
    for bad in ["-13 ms", "inf ms", "-inf ms", "NaN ms"] {
        let doc = doc_with(
            "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
            &format!("| 16 ms | 12 ms | {bad} | 55 % | 9 % |"),
        );
        let (h, b) = scalar_pair(
            id,
            json!([1999.0, 2001.0, 2000.0, 2002.0, 1998.0]),
            json!([0.9, 1.1, 1.0, 1.2, 0.8]),
            SLOW_MODEL,
        );
        let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
        assert!(
            has(&out.failures, "is not a finite number at or above zero"),
            "{bad}: {:?}",
            out.failures
        );
        let line = row(&out.comment, "Session start");
        assert!(line.ends_with("| fail |"), "{bad}: {line}");
    }
}

#[test]
fn rejected_slow_tolerances_name_the_parse_failure() {
    // A tolerance must be a finite number at or above zero, for the same
    // reason as the ceilings: the doc table is the only source of gate
    // numbers, so a malformed tolerance cell fails naming the cell. This
    // kills M2 (the `&&` weakened to `||`, which accepts the negative
    // and +inf) and M4 (the guard dropped, which accepts all four):
    // under either mutant the named parse failure disappears into an
    // overage or an overflow error.
    let id = "session_start_ms";
    for bad in ["-9 %", "inf %", "-inf %", "NaN %"] {
        let doc = doc_with(
            "| 16 ms | 12 ms | 13 ms | 55 % | 9 % |",
            &format!("| 16 ms | 12 ms | 13 ms | 55 % | {bad} |"),
        );
        let (h, b) = scalar_pair(
            id,
            json!([1999.0, 2001.0, 2000.0, 2002.0, 1998.0]),
            json!([0.9, 1.1, 1.0, 1.2, 0.8]),
            SLOW_MODEL,
        );
        let out = judge_doc(&doc, &h, Some(&b), Event::PullRequest);
        assert!(
            has(&out.failures, "is not a finite number at or above zero"),
            "{bad}: {:?}",
            out.failures
        );
        let line = row(&out.comment, "Session start");
        assert!(line.ends_with("| fail |"), "{bad}: {line}");
    }
}

#[test]
fn a_head_failure_with_a_missing_base_metric_shows_unavailable() {
    // When the head metric itself is malformed, the base column still
    // distinguishes a base that never recorded the metric
    // ("unavailable") from one that did (its median): a missing base
    // metric is unavailable-for-comparison, never a synthetic zero. This
    // kills M5 (the guard replaced with false, which reports "base
    // failed" for the missing metric), M6 (the guard replaced with
    // true, which reports "unavailable" for the present metric) and M7
    // (the negation deleted, which swaps both columns).
    let id = "session_start_ms";
    let h = with_cpu(without_metric(&neutral_scalars(id, head()), id), SLOW_MODEL);
    let missing = with_cpu(without_metric(&base(), id), SLOW_MODEL);
    let out = judge(&h, Some(&missing), Event::PullRequest);
    assert!(has(&out.failures, id), "{:?}", out.failures);
    let line = row(&out.comment, "Session start");
    assert!(line.contains("unavailable"), "{line}");
    assert!(line.ends_with("| fail |"), "{line}");
    let b = with_cpu(with_metric(base(), id, json!(vec![1.0; 5])), SLOW_MODEL);
    let out = judge(&h, Some(&b), Event::PullRequest);
    assert!(has(&out.failures, id), "{:?}", out.failures);
    let line = row(&out.comment, "Session start");
    assert!(line.contains("1.0 ms"), "{line}");
    assert!(!line.contains("unavailable"), "{line}");
    assert!(line.ends_with("| fail |"), "{line}");
}

// --- #2427 the 9V45 attach class data pins its own ceiling ---

/// Every stored 9V45 file's 10 MiB attach samples: the backstop's `main`
/// results since October 11, 2026 and the base and head of run 38783449852.
const FAST_ATTACH_DATA: &str = include_str!("../tests/class_data/9v45-attach-10mib.json");
const PERFORMANCE_DOC: &str = include_str!("../../docs/performance.md");

/// A gated row's fast ceiling (ms) and fast tolerance (%) in
/// `docs/performance.md`: the row whose text starts with `prefix`.
fn doc_gate(prefix: &str) -> (f64, f64) {
    let line = PERFORMANCE_DOC
        .lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no performance row starting with {prefix:?}"));
    let cells: Vec<&str> = line.split('|').map(str::trim).collect();
    let number = |cell: &str| -> f64 {
        cell.split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no number in {cell:?}"))
    };
    (number(cells[5]), number(cells[8]))
}

fn median_of(samples: &[f64]) -> f64 {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

/// The pin both fast-class ceiling tests share: every stored file's samples
/// pin the row's fast ceiling, and the stored per-file medians pin its fast
/// tolerance. Each pin sample must be finite and at or above zero before
/// any ceiling or median math, so an appended -1, NaN or infinite entry
/// fails here. A per-file median under the 1 ms quantum counts as 1 ms
/// when `quantum` holds, so a zero median never divides the tolerance.
fn pinned_gate(data: &str, prefix: &str, label: &str, quantum: bool) {
    let files: Vec<Value> = serde_json::from_str(data).unwrap();
    assert!(files.len() >= 5, "the class gates only with 5 stored files");
    let samples: Vec<Vec<f64>> = files
        .iter()
        .map(|f| {
            f["samples"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_f64().unwrap())
                .collect()
        })
        .collect();
    for (file, file_samples) in files.iter().zip(&samples) {
        for s in file_samples {
            assert!(
                s.is_finite() && *s >= 0.0,
                "{label}_pin_samples_are_finite_and_non_negative: {} holds {s}",
                file["file"]
            );
        }
    }
    let max_sample = samples.iter().flatten().copied().fold(0.0, f64::max);
    let medians: Vec<f64> = samples
        .iter()
        .map(|s| {
            let m = median_of(s);
            if quantum && m < 1.0 { 1.0 } else { m }
        })
        .collect();
    let max_median = medians.iter().copied().fold(0.0, f64::max);
    let min_median = medians.iter().copied().fold(f64::MAX, f64::min);
    let (ceiling, tolerance) = doc_gate(prefix);
    assert!(
        ceiling >= max_sample,
        "{label} ceiling {ceiling} ms sits below stored sample {max_sample} ms"
    );
    assert_eq!(
        ceiling,
        max_sample.ceil(),
        "ceiling is the max sample rounded up"
    );
    let want = (100.0 * (max_median / min_median - 1.0)).ceil();
    assert_eq!(tolerance, want, "{label} tolerance for the stored medians");
}

#[test]
fn fast_attach_ceiling_covers_every_stored_sample_and_tolerance_the_medians() {
    pinned_gate(
        FAST_ATTACH_DATA,
        "| Terminal to its first frame, attaching",
        "fast attach",
        false,
    );
}

// --- #2668 the 9V45 session_list class data pins its own ceiling ---

/// Every stored 9V45 file's session_list samples: plain numbers per file.
const SESSION_LIST_DATA: &str = include_str!("../tests/class_data/9v45-session-list.json");

#[test]
fn session_list_ceiling_covers_every_stored_sample_and_tolerance_the_medians() {
    pinned_gate(
        SESSION_LIST_DATA,
        "| `session_list`, waiting for every running",
        "fast session_list",
        true,
    );
}

// --- #1212 Part 16 the debug-run section, from the head's op_breakdown ---

/// `op_breakdown` for two workloads: busy turn with operations and provider
/// requests, and paging with nothing recorded.
fn breakdown() -> Value {
    json!([
        {
            "workload": "busy turn", "ms": 81234.5,
            "ops": {
                "tool.run": {"count": 429, "ms": 512},
                "log.append": {"count": 862, "ms": 40}
            },
            "provider_request": {"count": 430, "total_ms": 2210},
            "notes": []
        },
        {
            "workload": "paging", "ms": 1234.0, "ops": {},
            "provider_request": {"count": 0, "total_ms": 0},
            "notes": []
        }
    ])
}

/// The table rows, sorted: the report promises each operation's row, not an
/// order of rows (`docs/performance.md`, "Measuring").
fn debug_rows(comment: &str) -> Vec<&str> {
    let section = comment
        .split("### Debug run")
        .nth(1)
        .expect("Debug run section");
    let section = section.split("### Not measured").next().unwrap_or_default();
    let mut rows: Vec<&str> = section
        .lines()
        .filter(|line| line.starts_with("| ") && !line.starts_with("| Workload"))
        .collect();
    rows.sort_unstable();
    rows
}

#[test]
fn the_debug_run_section_lists_each_operation_next_to_its_workload() {
    let out = judge(
        &with_metric(head(), "op_breakdown", breakdown()),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(out.comment.contains("### Debug run\n\nEach timing and busy workload ran once more at `diagnostics.level: \"debug\"`; never a sample, never gated (`docs/performance.md`, \"Measuring\").\n"), "{}", out.comment);
    assert!(
        out.comment.contains(
            "| Workload | Run ms | Operation | Count | Total ms |\n|---|---|---|---|---|\n"
        ),
        "{}",
        out.comment
    );
    let mut want = vec![
        "| busy turn | 81234.5 | log.append | 862 | 40 |",
        "| busy turn | 81234.5 | tool.run | 429 | 512 |",
        "| busy turn | 81234.5 | provider_request total_ms | 430 | 2210 |",
        "| paging | 1234.0 | none recorded | — | — |",
    ];
    want.sort_unstable();
    assert_eq!(debug_rows(&out.comment), want, "{}", out.comment);
}

#[test]
fn a_failed_or_noted_workload_adds_bullets_and_no_failure() {
    let metric = json!([{
        "workload": "busy turn", "ms": 5.0, "ops": {},
        "provider_request": {"count": 0, "total_ms": 0},
        "notes": ["n1"], "error": "boom"
    }]);
    let with = judge(
        &with_metric(head(), "op_breakdown", metric),
        Some(&base()),
        Event::PullRequest,
    );
    let without = judge(&head(), Some(&base()), Event::PullRequest);
    assert_eq!(with.failures, without.failures);
    assert!(
        with.comment.contains("- busy turn: failed: boom\n"),
        "{}",
        with.comment
    );
    assert!(
        with.comment.contains("- busy turn: n1\n"),
        "{}",
        with.comment
    );
}

#[test]
fn a_malformed_breakdown_says_it_could_not_be_read_and_adds_no_failure() {
    let without = judge(&head(), Some(&base()), Event::PullRequest);
    let nameless = json!([{
        "ms": 1.0, "ops": {},
        "provider_request": {"count": 0, "total_ms": 0}, "notes": []
    }]);
    for (metric, why) in [
        (json!("oops"), "op_breakdown is not a list"),
        (nameless, "an entry has no workload name"),
    ] {
        let out = judge(
            &with_metric(head(), "op_breakdown", metric),
            Some(&base()),
            Event::PullRequest,
        );
        let line = format!("The debug run could not be read: {why}.");
        assert!(out.comment.contains(&line), "{}", out.comment);
        assert_eq!(out.failures, without.failures);
    }
}

#[test]
fn a_head_without_breakdown_renders_no_debug_run_section() {
    let out = judge(&head(), Some(&base()), Event::PullRequest);
    assert!(out.comment.contains("### Not measured"), "{}", out.comment);
    assert!(!out.comment.contains("Debug run"), "{}", out.comment);
    assert!(
        !out.comment.contains("could not be read"),
        "{}",
        out.comment
    );
}

#[test]
fn a_single_provider_request_is_recorded_not_none_recorded() {
    let metric = json!([{
        "workload": "web_fetch", "ms": 2.0, "ops": {},
        "provider_request": {"count": 1, "total_ms": 7},
        "notes": []
    }]);
    let out = judge(
        &with_metric(head(), "op_breakdown", metric),
        Some(&base()),
        Event::PullRequest,
    );
    assert!(
        out.comment
            .contains("| web_fetch | 2.0 | provider_request total_ms | 1 | 7 |"),
        "{}",
        out.comment
    );
    assert!(!out.comment.contains("none recorded"), "{}", out.comment);
}
