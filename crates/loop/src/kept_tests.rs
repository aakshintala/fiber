use contract::Envelope;
use serde_json::{Value, json};

fn line(kind: &str, payload: Value) -> Envelope {
    serde_json::from_value(json!({"kind":kind,"session_id":"s_one","ts":0,
        "schema_version":1,"seq":1,"payload":payload}))
    .expect("envelope")
}
fn completed(id: &str) -> Envelope {
    line("job_completed", json!({"job_id":id,"status":"completed"}))
}

#[test]
fn completions_from_multiple_crashes_keep_log_order_until_a_turn_consumes_them() {
    let lines = vec![
        line("fiber_started", json!({})),
        completed("j_one"),
        line("fiber_started", json!({})),
        completed("j_two"),
    ];
    assert_eq!(
        super::collect(&lines)
            .expect("kept")
            .iter()
            .map(|end| end.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["j_one", "j_two"]
    );
    let mut consumed = lines;
    consumed.push(line("turn_started", json!({"input":[]})));
    assert!(super::collect(&consumed).expect("consumed").is_empty());
}

#[test]
fn session_and_extension_owned_endings_are_not_keeper_deliveries() {
    let mut orphan = completed("j_orphan");
    orphan
        .payload
        .insert("error".into(), json!({"code":"orphaned","message":"gone"}));
    let mut session = completed("j_session");
    session.turn_id = Some(contract::TurnId("t_one".into()));
    let mut hooked = completed("j_hooked");
    hooked
        .payload
        .insert("notice".into(), json!("already delivered"));
    let lines = vec![
        completed("j_before"),
        line("fiber_exited", json!({})),
        line("fiber_started", json!({})),
        orphan,
        session,
        hooked,
        line(
            "job_started",
            json!({"job_id":"j_extension","extension":"x"}),
        ),
        completed("j_extension"),
        completed("j_keeper"),
    ];
    assert_eq!(
        super::collect(&lines)
            .expect("kept")
            .iter()
            .map(|end| end.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["j_keeper"]
    );
}

#[test]
fn completions_in_closed_process_spans_are_not_delivered() {
    let lines = vec![
        line("fiber_started", json!({})),
        completed("j_closed"),
        line("moved", json!({})),
        line("fiber_started", json!({})),
        completed("j_keeper"),
    ];
    assert_eq!(
        super::collect(&lines)
            .expect("kept")
            .iter()
            .map(|end| end.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["j_keeper"]
    );
}
