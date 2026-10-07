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

#[test]
fn a_symlink_is_denied_as_read_not_as_its_target() {
    let root = fakes::TempDir::new("fiber-switch-deny-link");
    let target = root.path().join("target");
    std::fs::write(&target, "sk").unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    // The read hands over the canonical file it read; the deny keeps
    // it verbatim even while it names a link, so a link moved after the
    // read cannot redirect the deny onto another file.
    let mut denied = Vec::new();
    deny_also(&mut denied, vec![link.clone()]);
    assert_eq!(denied, vec![link]);
    assert!(!denied.contains(&target.canonicalize().unwrap()));
}
