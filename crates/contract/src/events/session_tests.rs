//! `Variables::of` (`docs/invocation.md`, "A session's environment").

use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use super::{InputItem, Moved, Point, Rewound, Variables, VariablesSource};
use crate::{Seq, SessionId};

#[test]
fn rewind_input_round_trips() {
    let value = serde_json::json!({"type": "rewind", "command_id": "c_1"});
    let item: InputItem = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(item).unwrap(), value);
}

#[test]
fn rewound_summary_round_trips() {
    let value = serde_json::json!({"new_session_id": "s_new", "seq": 7, "summary": "s"});
    let item: Rewound = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(item).unwrap(), value);
}

#[test]
fn rewound_without_summary_has_no_summary_key() {
    let value = serde_json::json!({"new_session_id": "s_new", "seq": 7});
    let item: Rewound = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(item).unwrap(), value);
}

#[test]
fn moved_round_trips() {
    let value =
        serde_json::json!({"new_session_id": "s_new", "seq": 7, "workspace": "/work/other"});
    let item: Moved = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(item.new_session_id, SessionId("s_new".into()));
    assert_eq!(item.seq, Seq(7));
    assert_eq!(item.workspace, "/work/other");
    assert_eq!(serde_json::to_value(item).unwrap(), value);
}

#[test]
fn moved_requires_all_three_fields_with_their_types() {
    for value in [
        serde_json::json!({"seq": 7, "workspace": "/w"}),
        serde_json::json!({"new_session_id": "s", "workspace": "/w"}),
        serde_json::json!({"new_session_id": "s", "seq": 7}),
        serde_json::json!({"new_session_id": 7, "seq": 7, "workspace": "/w"}),
        serde_json::json!({"new_session_id": "s", "seq": "7", "workspace": "/w"}),
        serde_json::json!({"new_session_id": "s", "seq": 7, "workspace": 7}),
    ] {
        assert!(
            serde_json::from_value::<Moved>(value.clone()).is_err(),
            "{value}"
        );
    }
}

#[test]
fn point_without_project_writes_no_project_key() {
    let value = serde_json::json!({"session_id": "s", "seq": 3});
    let point: Point = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(point.project, None);
    assert_eq!(serde_json::to_value(point).unwrap(), value);
}

#[test]
fn point_with_project_round_trips() {
    let value = serde_json::json!({"session_id": "s", "seq": 3, "project": "-work-old"});
    let point: Point = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(point.project.as_deref(), Some("-work-old"));
    assert_eq!(serde_json::to_value(point).unwrap(), value);
}

#[test]
fn path_is_taken_out_and_other_names_sorted() {
    let vars = vec![
        (OsString::from("SHELL"), OsString::from("/bin/zsh")),
        (OsString::from("PATH"), OsString::from("/opt/bin:/usr/bin")),
        (OsString::from("HOME"), OsString::from("/Users/me")),
    ];
    assert_eq!(
        Variables::of(vars, VariablesSource::LoginShell),
        Variables {
            path: "/opt/bin:/usr/bin".to_owned(),
            names: vec!["HOME".to_owned(), "SHELL".to_owned()],
            source: VariablesSource::LoginShell,
        }
    );
}

#[test]
fn missing_path_is_empty() {
    let vars = vec![(OsString::from("HOME"), OsString::from("/Users/me"))];
    let got = Variables::of(vars, VariablesSource::Inherited);
    assert_eq!(got.path, "");
    assert_eq!(got.names, vec!["HOME".to_owned()]);
    assert_eq!(got.source, VariablesSource::Inherited);
}

#[test]
fn inherited_source_is_carried() {
    let vars: Vec<(OsString, OsString)> = vec![];
    let got = Variables::of(vars, VariablesSource::Inherited);
    assert_eq!(got.path, "");
    assert!(got.names.is_empty());
    assert_eq!(got.source, VariablesSource::Inherited);
}

#[test]
fn login_shell_source_is_carried() {
    let vars: Vec<(OsString, OsString)> = vec![];
    let got = Variables::of(vars, VariablesSource::LoginShell);
    assert_eq!(got.source, VariablesSource::LoginShell);
}

#[test]
#[cfg(unix)]
fn non_utf8_path_value_is_lossy() {
    let vars = vec![(OsString::from("PATH"), OsString::from_vec(vec![0xff, 0xfe]))];
    let got = Variables::of(vars, VariablesSource::Inherited);
    assert_eq!(got.path, String::from_utf8_lossy(&[0xff, 0xfe]));
    assert!(got.names.is_empty());
}

#[test]
#[cfg(target_os = "linux")]
fn non_utf8_name_is_named_lossily() {
    use std::os::unix::ffi::OsStringExt;
    let vars = vec![(OsString::from_vec(vec![0xff]), OsString::from("x"))];
    let got = Variables::of(vars, VariablesSource::Inherited);
    assert_eq!(got.path, "");
    assert_eq!(
        got.names,
        vec![String::from_utf8_lossy(&[0xff]).into_owned()]
    );
}
