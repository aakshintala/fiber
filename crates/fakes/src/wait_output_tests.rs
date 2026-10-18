use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use super::wait_output;

#[test]
fn a_child_in_its_own_group_returns_its_stdout() {
    let child = Command::new("sh")
        .args(["-c", "printf hi"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = wait_output(child, "printf hi");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"hi");
}
