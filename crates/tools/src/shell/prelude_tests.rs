//! Tests beside [`super::define`]: the exact prelude and its quoting.
//!
//! The shells below run the real prelude: what `sh` makes of the
//! functions, dash among them.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{define, quoted};
use fakes::Deadline;

#[test]
fn no_binary_defines_nothing() {
    assert_eq!(define(None), "");
}

#[test]
fn the_prelude_names_the_binary_and_unexports() {
    assert_eq!(
        define(Some(Path::new("/tmp/f x/fiber"))),
        "grep() { if [ -x '/tmp/f x/fiber' ]; then '/tmp/f x/fiber' grep \"$@\"; \
         else command grep \"$@\"; fi; }; \
         find() { if [ -x '/tmp/f x/fiber' ]; then '/tmp/f x/fiber' find \"$@\"; \
         else command find \"$@\"; fi; }; \
         if [ -n \"$BASH_VERSION\" ]; then export -nf grep find 2>/dev/null; fi"
    );
}

#[test]
fn a_single_quote_escapes() {
    assert_eq!(quoted(Path::new("/tmp/a'b/fiber")), "'/tmp/a'\\''b/fiber'");
    assert_eq!(quoted(Path::new("/tmp/plain/fiber")), "'/tmp/plain/fiber'");
}

/// How long one shell may take.
const DEADLINE: Duration = Duration::from_secs(10);

/// A stand-in search binary logging its arguments beside itself.
fn stand_in(dir: &Path) -> std::path::PathBuf {
    let target = dir.join("fiber");
    let body = format!("printf '%s\\n' \"$@\" >> \"{}.log\"\n", target.display());
    fakes::script(dir, "fiber", &body)
}

/// Runs `command` under `program -c` with the real prelude, waiting under
/// [`DEADLINE`].
#[track_caller]
fn under(program: &str, dir: &Path, command: &str) -> String {
    let child = Command::new(program)
        .arg("-c")
        .arg(format!("{}\n{command}", define(Some(&dir.join("fiber")))))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (done, waited) = mpsc::channel();
    thread::spawn(move || {
        done.send(child.wait_with_output()).unwrap_or(());
    });
    match Deadline::after(DEADLINE).recv(&waited) {
        Ok(Ok(output)) => {
            assert!(
                output.status.success(),
                "{program} -c {command}: {output:?}"
            );
            String::from_utf8(output.stdout).unwrap()
        }
        waited => panic!("waited {DEADLINE:?} for `{program} -c {command}`: {waited:?}"),
    }
}

#[test]
fn sh_and_bash_run_the_functions_and_keep_status_zero() {
    let dir = fakes::TempDir::new("fiber-search-shells");
    stand_in(dir.path());
    for program in ["/bin/sh", "/bin/bash"] {
        assert_eq!(
            under(program, dir.path(), "echo \"status=$?\""),
            "status=0\n",
            "{program}"
        );
        under(program, dir.path(), "grep x");
        let log = fs::read_to_string(dir.path().join("fiber.log")).unwrap();
        assert!(log.lines().eq(["grep", "x"]), "{program}: {log}");
        fs::remove_file(dir.path().join("fiber.log")).unwrap();
    }
}
