//! `Variables::of` (`docs/invocation.md`, "A session's environment").

use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use super::{Variables, VariablesSource};

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
