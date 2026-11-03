use super::{Step, Supervisor};
use contract::{JobId, keeper::Exit};

fn end() -> Exit {
    Exit {
        code: Some(0),
        signal: None,
        timed_out: false,
        output_cap: false,
    }
}

fn mixed() -> Supervisor {
    let mut sup = Supervisor::new();
    sup.started("call".into());
    sup.started("job".into());
    assert!(sup.adopt("job", JobId("j_one".into())));
    sup
}

#[test]
fn shutdown_stops_every_process_without_holding_the_log() {
    let mut sup = mixed();
    sup.started("second".into());
    // docs/tools.md, "The keeper": shutdown stops tool calls and jobs.
    let steps = sup.shutdown_requested();
    assert_eq!(steps.len(), 3);
    assert!(steps.iter().all(|step| matches!(step, Step::Stop { .. })));
    assert!(matches!(sup.lifeline_eof().as_slice(), [Step::Exit]));
}

#[test]
fn crash_holds_before_stopping_only_tool_calls() {
    let mut sup = mixed();
    // docs/events.md, "One writer at a time": acquire before writing.
    assert_eq!(
        sup.lifeline_eof(),
        vec![Step::HoldLog, Step::Stop { id: "call".into() }]
    );
    assert_eq!(
        sup.exited("job", end()),
        vec![
            Step::WriteJobCompleted {
                id: "job".into(),
                job_id: JobId("j_one".into()),
                exit: end(),
                unless_logged: false,
            },
            Step::Exit
        ]
    );
}

#[test]
fn no_job_stops_calls_and_exits_without_taking_the_log() {
    let mut sup = Supervisor::new();
    sup.started("call".into());
    assert_eq!(
        sup.lifeline_eof(),
        vec![Step::Stop { id: "call".into() }, Step::Exit]
    );
}

#[test]
fn live_exits_are_retained_until_confirmed_or_recovered() {
    let mut sup = mixed();
    assert!(sup.exited("job", end()).is_empty());
    assert_eq!(
        sup.lifeline_eof(),
        vec![
            Step::HoldLog,
            Step::Stop { id: "call".into() },
            Step::WriteJobCompleted {
                id: "job".into(),
                job_id: JobId("j_one".into()),
                exit: end(),
                unless_logged: true
            },
            Step::Exit
        ]
    );
    let mut confirmed = mixed();
    assert!(confirmed.exited("job", end()).is_empty());
    assert!(confirmed.confirmed("job").is_empty());
    assert_eq!(
        confirmed.lifeline_eof(),
        vec![Step::Stop { id: "call".into() }, Step::Exit]
    );
}

#[test]
fn losing_the_lock_writes_nothing_and_leaves_jobs_alone() {
    let mut sup = mixed();
    let _ = sup.lifeline_eof();
    assert_eq!(
        sup.lock_lost(),
        vec![Step::Stop { id: "call".into() }, Step::Exit]
    );
    assert!(sup.exited("job", end()).is_empty());
}

#[test]
fn unknown_calls_cannot_be_adopted_or_completed() {
    let mut sup = Supervisor::new();
    assert!(!sup.adopt("missing", JobId("j_missing".into())));
    assert!(sup.exited("missing", end()).is_empty());
    assert!(sup.confirmed("missing").is_empty());
    assert_eq!(sup.lifeline_eof(), vec![Step::Exit]);
}

#[test]
fn shutdown_discards_unconfirmed_exits() {
    let mut sup = mixed();
    assert!(sup.exited("job", end()).is_empty());
    assert_eq!(
        sup.shutdown_requested(),
        vec![Step::Stop { id: "call".into() }]
    );
    assert_eq!(sup.lifeline_eof(), vec![Step::Exit]);
}
