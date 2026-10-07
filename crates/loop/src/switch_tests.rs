//! The credential deny step a switch's read files pass through.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::path::PathBuf;

use super::deny_also;

#[test]
fn a_path_two_switches_read_is_added_once() {
    let root = fakes::TempDir::new("fiber-switch-deny");
    let key = root.path().join("key");
    std::fs::write(&key, "sk").unwrap();
    let key = key.canonicalize().unwrap();
    let started = PathBuf::from("/started/elsewhere");
    let mut denied = vec![started.clone()];
    deny_also(&mut denied, vec![key.clone()]);
    deny_also(&mut denied, vec![key.clone(), key.clone()]);
    assert_eq!(denied, vec![started, key]);
}
