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

fn signal_end() -> Exit {
    Exit {
        code: None,
        signal: Some(15),
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
    // docs/events.md, "Writing": one writer at a time, enforced by a lock file: acquire before writing.
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
fn bound_stops_every_live_process_and_marks_held_jobs() {
    let mut sup = mixed();
    // docs/tools.md, "The keeper": a crash hands the log to the keeper, and
    // the keeper stops each command still running as a tool call.
    let steps = sup.lifeline_eof();
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::HoldLog));
    assert!(steps.contains(&Step::Stop { id: "call".into() }));
    // Shutdown-style: every process still running gets a stop. The bound's
    // mark lands on the held job only; the tool call keeps none of it.
    // docs/tools.md, "The keeper": the keeper stops every job and delegate
    // still running, as a shutdown does.
    let stops = sup.bound_elapsed();
    assert_eq!(stops.len(), 2);
    assert!(stops.contains(&Step::Stop { id: "call".into() }));
    assert!(stops.contains(&Step::Stop { id: "job".into() }));
    assert!(sup.exited("call", end()).is_empty());
    // docs/tools.md, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout, and exits.
    let steps = sup.exited("job", signal_end());
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::WriteKeeperTimeout {
        job_id: JobId("j_one".into()),
        exit: signal_end(),
    }));
    assert!(steps.contains(&Step::Exit));
}

#[test]
fn reap_after_bound_writes_keeper_timeout_once_per_job() {
    let mut sup = Supervisor::new();
    sup.started("first".into());
    sup.started("second".into());
    assert!(sup.adopt("first", JobId("j_first".into())));
    assert!(sup.adopt("second", JobId("j_second".into())));
    // docs/tools.md, "The keeper": a crash hands the log to the keeper.
    assert_eq!(sup.lifeline_eof(), vec![Step::HoldLog]);
    // docs/tools.md, "The keeper": the keeper stops every job and delegate
    // still running, as a shutdown does.
    let stops = sup.bound_elapsed();
    assert_eq!(stops.len(), 2);
    assert!(stops.contains(&Step::Stop { id: "first".into() }));
    assert!(stops.contains(&Step::Stop {
        id: "second".into()
    }));
    // The first reap writes its timeout without exiting: one job remains.
    // docs/tools.md, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout.
    assert_eq!(
        sup.exited("first", signal_end()),
        vec![Step::WriteKeeperTimeout {
            job_id: JobId("j_first".into()),
            exit: signal_end(),
        }]
    );
    // docs/tools.md, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout, and exits once the last job ends.
    let steps = sup.exited("second", end());
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::WriteKeeperTimeout {
        job_id: JobId("j_second".into()),
        exit: end(),
    }));
    assert!(steps.contains(&Step::Exit));
}

#[test]
fn reap_before_bound_keeps_the_normal_completion() {
    let mut sup = mixed();
    let _ = sup.lifeline_eof();
    // docs/tools.md, "The keeper": a job_completed for each job that ends,
    // and the keeper exits once its last job ends.
    let steps = sup.exited("job", end());
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::WriteJobCompleted {
        id: "job".into(),
        job_id: JobId("j_one".into()),
        exit: end(),
        unless_logged: false,
    }));
    assert!(steps.contains(&Step::Exit));
    assert!(sup.bound_elapsed().is_empty());
}

#[test]
fn bound_twice_stops_again_but_writes_once() {
    let mut sup = mixed();
    let _ = sup.lifeline_eof();
    // docs/tools.md, "The keeper": the keeper stops every job and delegate
    // still running, as a shutdown does.
    let first = sup.bound_elapsed();
    assert_eq!(first.len(), 2);
    assert!(first.contains(&Step::Stop { id: "call".into() }));
    assert!(first.contains(&Step::Stop { id: "job".into() }));
    // A second firing re-issues stops for processes still running, but the
    // mark is already there: the reap still writes exactly one timeout.
    let second = sup.bound_elapsed();
    assert_eq!(second.len(), first.len());
    assert!(second.contains(&Step::Stop { id: "call".into() }));
    assert!(second.contains(&Step::Stop { id: "job".into() }));
    // docs/tools.md, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout, and exits.
    let steps = sup.exited("job", signal_end());
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::WriteKeeperTimeout {
        job_id: JobId("j_one".into()),
        exit: signal_end(),
    }));
    assert!(steps.contains(&Step::Exit));
}

#[test]
fn exit_before_adopt_still_writes_the_normal_completion() {
    // A fast call can reap before the session's Adopt line reaches the
    // control thread. The exit must survive until the adopt binds the job,
    // so the crash takeover still writes the one normal ending.
    let mut sup = Supervisor::new();
    sup.started("quick".into());
    assert!(sup.exited("quick", end()).is_empty());
    assert!(sup.adopt("quick", JobId("j_quick".into())));
    // docs/tools.md, "The keeper": a job_completed for each job that ends,
    // and the keeper exits once its last job ends.
    assert_eq!(
        sup.lifeline_eof(),
        vec![
            Step::HoldLog,
            Step::WriteJobCompleted {
                id: "quick".into(),
                job_id: JobId("j_quick".into()),
                exit: end(),
                unless_logged: true,
            },
            Step::Exit,
        ]
    );
}

#[test]
fn bound_outside_holding_marks_nothing() {
    // Live: the session is still there, so the bound cannot fire.
    let mut live = mixed();
    assert!(live.bound_elapsed().is_empty());
    // docs/tools.md, "The keeper": a crash hands the log to the keeper, and
    // the keeper stops each command still running as a tool call.
    let steps = live.lifeline_eof();
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::HoldLog));
    assert!(steps.contains(&Step::Stop { id: "call".into() }));
    // Shutdown: the shutdown path already owns every ending.
    let mut shutdown = mixed();
    let _ = shutdown.shutdown_requested();
    assert!(shutdown.bound_elapsed().is_empty());
    // docs/tools.md, "The keeper": the keeper exits once its last job ends.
    assert_eq!(shutdown.lifeline_eof(), vec![Step::Exit]);
    // Gone: nothing is tracked anymore.
    // docs/tools.md, "The keeper": a command that is not yet a job ends with
    // the crash, and the keeper exits once its last job ends.
    let mut gone = Supervisor::new();
    gone.started("call".into());
    let steps = gone.lifeline_eof();
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::Stop { id: "call".into() }));
    assert!(steps.contains(&Step::Exit));
    assert!(gone.bound_elapsed().is_empty());
}

#[test]
fn live_exit_then_crash_then_bound_writes_nothing_more() {
    // Reap-then-expire: an ending the live session saw but never confirmed
    // is written normally at takeover and never rewritten at the bound.
    let mut sup = mixed();
    assert!(sup.exited("job", signal_end()).is_empty());
    // docs/tools.md, "The keeper": a crash hands the log to the keeper, the
    // keeper stops each command still running as a tool call, a job_completed
    // for each job that ends, and the keeper exits once its last job ends.
    let steps = sup.lifeline_eof();
    assert_eq!(steps.len(), 4);
    assert!(steps.contains(&Step::HoldLog));
    assert!(steps.contains(&Step::Stop { id: "call".into() }));
    assert!(steps.contains(&Step::WriteJobCompleted {
        id: "job".into(),
        job_id: JobId("j_one".into()),
        exit: signal_end(),
        unless_logged: true,
    }));
    assert!(steps.contains(&Step::Exit));
    assert!(sup.bound_elapsed().is_empty());
}

#[test]
fn takeover_write_then_bound_write_gives_each_job_one_ending() {
    let mut sup = Supervisor::new();
    sup.started("early".into());
    sup.started("late".into());
    assert!(sup.adopt("early", JobId("j_early".into())));
    assert!(sup.adopt("late", JobId("j_late".into())));
    assert!(sup.exited("early", end()).is_empty());
    // docs/tools.md, "The keeper": a crash hands the log to the keeper, and a
    // job_completed for each job that ends.
    let steps = sup.lifeline_eof();
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::HoldLog));
    assert!(steps.contains(&Step::WriteJobCompleted {
        id: "early".into(),
        job_id: JobId("j_early".into()),
        exit: end(),
        unless_logged: true,
    }));
    // docs/tools.md, "The keeper": the keeper stops every job and delegate
    // still running, as a shutdown does.
    assert_eq!(sup.bound_elapsed(), vec![Step::Stop { id: "late".into() }]);
    // docs/tools.md, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout, and exits.
    let steps = sup.exited("late", signal_end());
    assert_eq!(steps.len(), 2);
    assert!(steps.contains(&Step::WriteKeeperTimeout {
        job_id: JobId("j_late".into()),
        exit: signal_end(),
    }));
    assert!(steps.contains(&Step::Exit));
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
