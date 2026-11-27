#[test]
fn a_second_resume_over_a_logged_orphan_keeps_it_after_the_results() {
    // The first resume logs the orphan and the process exits again on the
    // same request; the second resume reads the orphan from the log and
    // still sends it after the open batch's results.
    let mut history = History::new(vec![Scripted::text("Hello."), Scripted::text("Again.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_9"));
    history.write(requested("shell", "Paris"), Some("a_0"));
    history.write(job_started("j_a"), Some("a_0"));
    history.write(completed_event("Started j_a."), Some("a_0"));
    history.write(requested("exec", "Paris"), Some("a_1"));
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);

    history.write(job_completed("j_a"), None);
    history.write(fiber_started(), None);
    history.write(fiber_exited(Some("r_9")), None);
    history.freeze();
    let (tx, rx) = mpsc::channel();
    history.inbox_tx = tx;
    history.inbox_rx = Some(rx);
    let looped = history.resume_headless(Vec::new());
    // The orphan is logged once.
    assert!(history.new_lines().is_empty());
    let (looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    drop(looped);

    let lines = history.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind() == EventKind::JobCompleted)
            .count(),
        1
    );
    let requests = history.provider.requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation;
    let rebuilt = history.rebuilt();
    assert_eq!(&rebuilt[..conversation.len()], &conversation[..]);
    // The rebuild puts the opening message at index 0, so the a_1 call is
    // followed directly by its result, and the notice follows the result.
    let call = conversation
        .iter()
        .position(
            |input| matches!(input, Input::ToolCall { action_id, .. } if action_id.0 == "a_1"),
        )
        .unwrap();
    let result = conversation
        .iter()
        .position(
            |input| matches!(input, Input::ToolResult { action_id, .. } if action_id.0 == "a_1"),
        )
        .unwrap();
    assert_eq!(result, call + 1);
    assert_eq!(result, conversation.len() - 2);
    assert_eq!(
        conversation.last(),
        Some(&Input::User {
            text: format!("Fiber: background job j_a ended: failed.\n{ORPHANED}"),
            images: Vec::new()
        })
    );
}

