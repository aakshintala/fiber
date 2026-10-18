use super::*;
use serde_json::json;

fn lines() -> Vec<(usize, Value)> {
    vec![
        (
            1,
            json!({"kind":"tool_call_requested","action_id":"a_other","payload":{"name":"read"}}),
        ),
        (
            2,
            json!({"kind":"tool_call_started","action_id":"a_other","ts":1}),
        ),
        (
            3,
            json!({"kind":"tool_call_completed","action_id":"a_other","ts":1000}),
        ),
        (
            4,
            json!({"kind":"tool_call_requested","action_id":"a_list","payload":{"name":"session_list"}}),
        ),
        (
            5,
            json!({"kind":"tool_call_started","action_id":"a_list","ts":10}),
        ),
        (
            6,
            json!({"kind":"tool_call_completed","action_id":"a_list","ts":17,"payload":{"content":[{"type":"text","text":"[]"}]}}),
        ),
    ]
}
#[test]
fn span_is_the_matching_tools_start_to_completion_in_milliseconds() {
    assert_eq!(span(&lines()), Ok(7));
    for (index, missing) in [
        (3, "tool_call_requested"),
        (4, "tool_call_started"),
        (5, "tool_call_completed"),
    ] {
        let mut lines = lines();
        lines.remove(index);
        assert!(span(&lines).unwrap_err().contains(missing));
    }
}
#[test]
fn listing_checks_three_answering_entries_and_exactly_one_self() {
    let good = vec![
        json!({"id":"s_a","answering":true}),
        json!({"id":"s_b","answering":true,"self":true}),
        json!({"id":"s_c","answering":true}),
    ];
    let mut notes = vec![];
    check(&good, &mut notes);
    assert_eq!(notes, Vec::<String>::new());
    for count in [2, 4] {
        let entries: Vec<_> = good.iter().cycle().take(count).cloned().collect();
        let mut notes = vec![];
        check(&entries, &mut notes);
        assert!(
            notes.iter().any(|note| note.contains("entries")),
            "{notes:?}"
        );
    }
    for self_count in [0, 2] {
        let mut entries = good.clone();
        entries[1].as_object_mut().unwrap().remove("self");
        for entry in entries.iter_mut().take(self_count) {
            entry["self"] = true.into();
        }
        let mut notes = vec![];
        check(&entries, &mut notes);
        assert!(notes.iter().any(|note| note.contains("self")), "{notes:?}");
    }
    for answering in [json!(false), Value::Null] {
        let mut entries = good.clone();
        entries[0]["answering"] = answering;
        let mut notes = vec![];
        check(&entries, &mut notes);
        assert!(
            notes.iter().any(|note| note.contains("answering")),
            "{notes:?}"
        );
    }
}
#[test]
fn the_completed_tools_text_is_the_listing() {
    assert_eq!(listing(&lines()), Ok(vec![]));
    let mut bad = lines();
    bad[5].1["payload"]["content"][0]["text"] = "not JSON".into();
    assert!(listing(&bad).unwrap_err().contains("session_list"));
    let mut bad = lines();
    bad.pop();
    assert!(listing(&bad).unwrap_err().contains("tool_call_completed"));
}
