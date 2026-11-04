use super::{FromKeeper, HostStdin, ToKeeper};

#[test]
fn every_session_line_round_trips_byte_for_byte() {
    // The keeper's internal protocol is one JSON object per line
    // (docs/tools.md, "The keeper").
    let lines = [
        r#"{"start":{"id":"call_1","program":"/bin/bash","args":["-c","make"],"cwd":"/w","env":[["FIBER_MATCHES","/w/.m"]],"stdin":"null","timeout_ms":600000,"output_cap":5368709120,"sandbox":null}}"#,
        r#"{"start":{"id":"call_1","program":"/bin/bash","args":["-c","make"],"cwd":"/w","env":[],"stdin":"pty","timeout_ms":null,"output_cap":5368709120,"sandbox":{"enabled":true,"allow_write":["/w"],"deny_read":[],"fiber_home":"/h/.fiber","workspace":"/w","baseline":"/h/.fiber/projects/k/baseline"}}}"#,
        r#"{"stop":"call_1"}"#,
        r#"{"write":{"id":"call_1","text":"y\n"}}"#,
        r#"{"adopt":{"id":"call_1","job_id":"j_cd","output_path":"/h/.fiber/projects/k/sessions/s_1/artifacts/j_cd.log","errors_path":null}}"#,
        r#"{"confirmed":"call_1"}"#,
        r#"{"shutdown":true}"#,
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
    ];
    for line in lines {
        let parsed: FromKeeper = serde_json::from_str(line).expect("keeper line");
        assert_eq!(serde_json::to_string(&parsed).expect("encode"), line);
    }
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
