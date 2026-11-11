use super::{FromKeeper, HostStdin, KeptJob, ToKeeper};

#[test]
fn every_session_line_round_trips_byte_for_byte() {
    // The keeper's internal protocol is one JSON object per line
    // (docs/tools.md, "The keeper").
    let lines = [
        r#"{"start":{"id":"call_1","program":"/bin/bash","args":["-c","make"],"cwd":"/w","env":[["FIBER_MATCHES","/w/.m"]],"stdin":"null","timeout_ms":600000,"output_cap":5368709120,"sandbox":null}}"#,
        r#"{"start":{"id":"call_1","program":"/bin/bash","args":["-c","make"],"cwd":"/w","env":[],"stdin":"pty","timeout_ms":null,"output_cap":5368709120,"sandbox":{"enabled":true,"allow_write":["/w"],"deny_read":[],"fiber_home":"/h/.fiber","workspace":"/w","baseline":"/h/.fiber/projects/k/baseline"}}}"#,
        r#"{"stop":"call_1"}"#,
        r#"{"write":{"id":"call_1","text":"y\n"}}"#,
        r#"{"adopt":{"id":"call_1","job_id":"j_cd","output_path":"/h/.fiber/projects/k/sessions/s_1/artifacts/j_cd.log","errors_path":null,"lines":false}}"#,
        r#"{"adopt":{"id":"call_1","job_id":"j_cd","output_path":"/h/.fiber/projects/k/sessions/s_1/artifacts/j_cd.log","errors_path":"/h/.fiber/projects/k/sessions/s_1/artifacts/j_cd.stderr.log","lines":true}}"#,
        r#"{"confirmed":"call_1"}"#,
        r#"{"shutdown":true}"#,
        r#"{"identify":"s_0123456789abcdef"}"#,
        r#"{"handover":true}"#,
        r#"{"adopted":true}"#,
    ];
    for line in lines {
        let parsed: ToKeeper = serde_json::from_str(line).expect("session line");
        assert_eq!(serde_json::to_string(&parsed).expect("encode"), line);
    }
}

#[test]
fn every_keeper_line_round_trips_byte_for_byte() {
    let lines = [
        r#"{"started":{"id":"call_1","pgid":4242}}"#,
        r#"{"failed":{"id":"call_1","error":"No such file or directory (os error 2)"}}"#,
        r#"{"output":{"id":"call_1","stream":"out","bytes":"aGkK"}}"#,
        r#"{"output":{"id":"call_1","stream":"err","bytes":"/w=="}}"#,
        r#"{"exit":{"id":"call_1","code":0,"signal":null,"timed_out":false,"output_cap":false}}"#,
        r#"{"identity":{"session_id":"s_0123456789abcdef"}}"#,
        r#"{"handover":{"running":[{"id":"call_0","job_id":"j_0123456789abcdef","output_path":"/home/test/sessions/s_0123456789abcdef/artifacts/j_0123456789abcdef.log","errors_path":null,"tty":false,"lines":false}]}}"#,
        r#"{"handover":{"running":[{"id":"call_1","job_id":"j_cd","output_path":"/a/j_cd.log","errors_path":"/a/j_cd.stderr.log","tty":true,"lines":true},{"id":"call_2","job_id":"j_ef","output_path":"/a/j_ef.log","errors_path":null,"tty":false,"lines":true}]}}"#,
        r#"{"handover":{"running":[]}}"#,
        r#"{"adopt_done":true}"#,
    ];
    for line in lines {
        let parsed: FromKeeper = serde_json::from_str(line).expect("keeper line");
        assert_eq!(serde_json::to_string(&parsed).expect("encode"), line);
    }
}

#[test]
fn shutdown_true_is_the_frozen_keeper_stop_line() {
    // `docs/invocation.md`, "Commands and flags": the keeper stop a newer
    // `fiber stop` delivers to any older keeper is exactly this line.
    let line = serde_json::to_string(&ToKeeper::Shutdown(true)).expect("encode");
    assert_eq!(line, r#"{"shutdown":true}"#);
    assert_eq!(
        serde_json::to_string(&ToKeeper::Shutdown(false)).expect("encode"),
        r#"{"shutdown":false}"#
    );
}

#[test]
fn an_unknown_stdin_is_refused() {
    assert!(serde_json::from_str::<HostStdin>(r#""pipe""#).is_err());
}

#[test]
fn null_stdin_is_recognized() {
    assert_eq!(
        serde_json::from_str::<HostStdin>(r#""null""#).unwrap(),
        HostStdin::Null
    );
}

#[test]
fn pty_stdin_is_recognized() {
    assert_eq!(
        serde_json::from_str::<HostStdin>(r#""pty""#).unwrap(),
        HostStdin::Pty
    );
}

#[test]
fn the_adopt_line_requires_an_explicit_boolean_lines() {
    let base = r#""id":"c","job_id":"j","output_path":"/o","errors_path":null"#;
    assert!(serde_json::from_str::<ToKeeper>(&format!(r#"{{"adopt":{{{base}}}}}"#)).is_err());
    for bad in ["\"true\"", "1", "null"] {
        let line = format!(r#"{{"adopt":{{{base},"lines":{bad}}}}}"#);
        assert!(serde_json::from_str::<ToKeeper>(&line).is_err(), "{line}");
    }
}

#[test]
fn a_kept_job_keeps_lines_apart_from_its_errors_path() {
    // A monitor whose stderr file could not be created still delivers lines.
    let job: KeptJob = serde_json::from_str(
        r#"{"id":"c","job_id":"j","output_path":"/o","errors_path":null,"tty":false,"lines":true}"#,
    )
    .expect("kept job");
    assert!(job.lines);
    assert_eq!(job.errors_path, None);
    let job: KeptJob = serde_json::from_str(
        r#"{"id":"c","job_id":"j","output_path":"/o","errors_path":"/e","tty":true,"lines":false}"#,
    )
    .expect("kept job");
    assert!(!job.lines);
    assert!(job.tty);
    for bad in ["\"true\"", "1", "null"] {
        let line = format!(
            r#"{{"id":"c","job_id":"j","output_path":"/o","errors_path":null,"tty":false,"lines":{bad}}}"#
        );
        assert!(serde_json::from_str::<KeptJob>(&line).is_err(), "{line}");
    }
}

#[test]
fn handshake_booleans_reject_other_tokens() {
    for line in [
        r#"{"handover":"yes"}"#,
        r#"{"handover":1}"#,
        r#"{"adopted":"true"}"#,
        r#"{"adopted":null}"#,
    ] {
        assert!(serde_json::from_str::<ToKeeper>(line).is_err(), "{line}");
    }
    for line in [r#"{"adopt_done":"true"}"#, r#"{"adopt_done":0}"#] {
        assert!(serde_json::from_str::<FromKeeper>(line).is_err(), "{line}");
    }
}

#[test]
fn no_handover_line_carries_a_process_id() {
    // The old keeper's pid comes from the kernel's peer credential on the
    // connection, never from the wire.
    let frames = [
        serde_json::to_string(&ToKeeper::Identify(contract_session())).expect("encode"),
        serde_json::to_string(&FromKeeper::Handover {
            running: Vec::new(),
        })
        .expect("encode"),
        serde_json::to_string(&FromKeeper::Identity {
            session_id: contract_session(),
        })
        .expect("encode"),
    ];
    for frame in frames {
        assert!(!frame.contains("pid"), "{frame}");
    }
}

fn contract_session() -> crate::SessionId {
    crate::SessionId("s_0123456789abcdef".into())
}
