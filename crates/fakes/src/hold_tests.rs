//! `Hold`'s own tests: the order the FIFO keeps.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::fs::FileTypeExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use super::*;
use crate::deadline::Deadline;
use crate::{MUST_SUCCEED_WITHIN, TempDir};

/// The drop unlinks the FIFO: a holder that has not opened it yet
/// fails its open with ENOENT and exits instead of parking on the
/// FIFO forever past the test. The kill of a parked holder stays
/// pinned by `drop_reaps_a_parked_holder`.
#[test]
fn drop_unlinks_the_fifo_so_late_starters_fail_open() {
    let dir = TempDir::new("fiber-hold-drop");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    let hold = Hold::park(&path, &command);
    assert_eq!(
        hold.path(),
        path.as_path(),
        "path names the FIFO park made, so the drop below unlinks that FIFO"
    );
    let file_type = std::fs::symlink_metadata(&path).unwrap().file_type();
    assert!(file_type.is_fifo(), "{} is not a FIFO", path.display());
    drop(hold);
    assert!(
        !path.exists(),
        "the drop unlinks the FIFO so a late holder fails open"
    );
}

#[test]
fn park_makes_a_fifo_and_on_opens_an_existing_one() {
    let dir = TempDir::new("fiber-hold-park");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    let hold = Hold::park(&path, &command);
    let file_type = std::fs::symlink_metadata(&path).unwrap().file_type();
    assert!(
        file_type.is_fifo(),
        "park leaves a FIFO at {}",
        path.display()
    );
    assert_eq!(hold.path(), path.as_path(), "path names the FIFO park made");
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));

    let other = dir.path().join("other");
    crate::fifo(&other);
    // `fifo` panics on an existing path, so reaching `on`'s open below
    // proves `on` made nothing itself.
    let pattern = other.to_str().unwrap().to_owned();
    let reopened = Hold::on(&other, &pattern);
    assert_eq!(
        reopened.path(),
        other.as_path(),
        "on opens the FIFO the test made"
    );
}

/// A release written before the reader opens the FIFO is buffered and
/// read when the reader opens it (`docs/testing.md:297-299`): the
/// release end stays open until the hold is dropped or stood down.
/// The fake keeps the order the FIFO keeps: a release before the next
/// hold (`docs/testing/fakes.md`, "Fakes").
#[test]
fn a_release_written_before_the_reader_opens_is_read() {
    let dir = TempDir::new("fiber-hold-release");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    let mut hold = Hold::park(&path, &command);
    hold.release().write_all(b"\n").unwrap();
    let child = Command::new("head")
        .arg("-n")
        .arg("1")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = crate::wait_output(child, "the FIFO reader to exit");
    assert!(
        output.status.success(),
        "the reader read the buffered release: {}",
        output.status
    );
    assert_eq!(
        output.stdout, b"\n",
        "the reader read the release written before it opened"
    );
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

/// Dropping the helper without any release write reaps the parked
/// holder: the parent-death cleanup path, which is the leak this
/// helper closes. The pre-drop guard pins the test against vacuity:
/// `try_matching_exits` passes when nothing matches, so without a
/// live match on the holder's own pid the kill below would prove
/// nothing. The drop unlinks the FIFO and returns; the guard's kill
/// reaps the holder asynchronously, so the test waits for the holder
/// to exit instead of asserting it is already gone.
#[test]
fn drop_reaps_a_parked_holder() {
    let dir = TempDir::new("fiber-hold-reap");
    let path = dir.path().join("hold");
    // Per-process, not per-hold: every hold in this test shares the pid
    // fragment, and a sibling test process can match by pid prefix; the
    // absolute path keeps the helper on its hold-path derivation.
    let command = format!("head -n 1 {} #{}", path.display(), std::process::id());
    let hold = Hold::park(&path, &command);
    // A second read+write handle, held open across the drop below: the
    // helper's own handle closing must not EOF-release the holder, so
    // the reap proves the kill rather than the EOF path. Still no
    // release write anywhere in this test.
    let second = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let holder = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = holder.id();
    assert!(crate::pid_alive(pid), "the holder lives before the drop");
    let pattern = path.to_str().unwrap().to_owned();
    let listed = pattern.clone();
    crate::wait_until(
        "the holder to park",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            crate::matching(&listed)
                .map(|pids| pids.contains(&pid))
                .unwrap_or(false)
        },
    );
    drop(hold);
    crate::try_matching_exits(pattern.as_str(), Deadline::after(MUST_SUCCEED_WITHIN))
        .unwrap_or_else(|error| panic!("the dropped helper reaps its holder: {error}"));
    drop(second);
    let _output = crate::finished(
        "the reaped holder",
        holder,
        &[],
        Deadline::after(MUST_SUCCEED_WITHIN),
    );
}

/// `stand_down` closes the release end and disarms the reap: the
/// holder then exits by end-of-file, killed by no signal. The
/// holder's `echo opened` follows its `exec 3<FIFO` open in program
/// order, and the test's receive of that line (a pipe read, then a
/// channel) precedes `stand_down`, so the only writer closes after
/// the reader has opened; a holder that had not opened would
/// otherwise block in `open` forever.
#[test]
fn stand_down_closes_the_end_and_disarms_the_reap() {
    let dir = TempDir::new("fiber-hold-stand-down");
    let path = dir.path().join("hold");
    let script = format!("exec 3<'{}'; echo opened; read x <&3", path.display());
    let hold = Hold::park(&path, &script);
    let mut holder = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let out = holder.stdout.take().unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(out);
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() {
            match line_tx.send(line) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    let line = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&line_rx, "the holder to open the FIFO and print");
    assert_eq!(
        line, "opened\n",
        "the holder opened the FIFO before the stand-down below"
    );
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(holder.wait()).unwrap());
    let status = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the EOF-released holder to exit")
        .expect("the EOF-released holder exited");
    assert!(
        status.code().is_some(),
        "the holder exited, never killed by a signal: {status}"
    );
}

#[test]
fn the_watchdog_pattern_is_the_path_when_the_command_embeds_it() {
    let dir = TempDir::new("fiber-hold-pattern-path");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    assert_eq!(
        derive_pattern(&command, &path),
        path.to_str().unwrap(),
        "the command embeds the hold path"
    );
}

#[test]
fn the_watchdog_pattern_is_the_pid_scoped_hold_fragment_otherwise() {
    let dir = TempDir::new("fiber-hold-pattern-fragment");
    let path = dir.path().join("hold");
    assert_eq!(
        derive_pattern("head -n 1 hold #123  ", &path),
        "hold #123",
        "the fragment keeps the pid scope with no trailing space"
    );
}

#[test]
fn the_embedded_path_wins_over_the_hold_fragment() {
    let dir = TempDir::new("fiber-hold-pattern-both");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {} #123", path.display());
    assert_eq!(
        derive_pattern(&command, &path),
        path.to_str().unwrap(),
        "the embedded path wins over the pid-scoped fragment"
    );
}

#[test]
#[should_panic(expected = "derives its watchdog pattern from the command")]
fn a_command_with_neither_panics() {
    let dir = TempDir::new("fiber-hold-pattern-neither");
    let path = dir.path().join("hold");
    let _pattern = derive_pattern("head -n 1", &path);
}

/// The search prelude decides whether the live job keeps the `hold #`
/// fragment: the product runs the model's command after three shell
/// statements and a newline (`crates/tools/src/shell.rs:498-501`), so the
/// last line is the only one bash may exec. A retained shell keeps the
/// whole `-c` string, and the fragment matches it; an exec of the last
/// line drops the trailing comment, the fragment matches nothing, and the
/// holder still ends when the release handle closes and `head` reads EOF.
#[test]
#[allow(
    clippy::print_stderr,
    reason = "the macOS CI log keeps this classification line as that leg's evidence"
)]
fn a_job_command_with_the_search_prelude_shows_its_hold_fragment_only_if_bash_keeps_the_shell() {
    let test_pid = std::process::id();
    // A literal copy of `prelude::define(Some("/opt/fiber/fiber"))`
    // (`crates/tools/src/shell/prelude.rs:13-26`), which this crate cannot
    // call (`pub(super)` in `tools`). Only the shape matters here: three
    // statements joined by `; `, then the newline below, then the command
    // (`crates/tools/src/shell.rs:498-501`).
    let prelude = "grep() { if [ -x '/opt/fiber/fiber' ]; then '/opt/fiber/fiber' grep \"$@\"; else command grep \"$@\"; fi; }; find() { if [ -x '/opt/fiber/fiber' ]; then '/opt/fiber/fiber' find \"$@\"; else command find \"$@\"; fi; }; if [ -n \"$BASH_VERSION\" ]; then export -nf grep find 2>/dev/null; fi";
    let command = format!("{prelude}\nhead -n 1 hold #{test_pid} ");
    let fragment = format!("hold #{test_pid} ");
    let dir = TempDir::new("fiber-hold-prelude");
    let path = dir.path().join("hold");
    crate::fifo(&path);
    // The release end stays open read+write, as `Hold` keeps it, so the
    // holder's open never blocks and its read parks.
    let mut release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    // The product's spawn (`crates/tools/src/shell/command.rs:516-517`):
    // `/bin/bash` when present, `sh` otherwise
    // (`crates/tools/src/shell.rs:866-872`).
    let program = if std::path::Path::new("/bin/bash").is_file() {
        "/bin/bash"
    } else {
        "sh"
    };
    let child = Command::new(program)
        .arg("-c")
        .arg(&command)
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    // The group guard reaps the shell and its `head` when the test dies.
    let watchdog = crate::Watchdog::group(pid);
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Form {
        Exec,
        Retained,
    }
    // The classification poll runs on the test thread, not in `wait_until`:
    // each poll makes at most two listing calls (`matching`'s pgrep and
    // `ps_table`'s ps, each bounded by MUST_SUCCEED_WITHIN), and a blocking
    // predicate would outlive `wait_until`'s outer wait (`wait_until.rs`).
    // Bounded by MUST_SUCCEED_WITHIN, as a literal: `budget-bounds` reads
    // only a literal duration or a complete symbolic path.
    let deadline = Deadline::after(std::time::Duration::from_secs(10));
    let bound = deadline.left();
    // `ps_table` rebuilds each line with `split_whitespace` and `join`,
    // which drops the fragment's trailing space; the rebuilt comparison
    // uses the trimmed form. `matching` keeps the spaced fragment: pgrep
    // sees the argv unmodified.
    let bare = fragment.trim_end();
    let form: Form = loop {
        let with = match crate::matching(&fragment) {
            Ok(with) => with,
            Err(_) => {
                if deadline.left().is_zero() {
                    panic!("waited {bound:?} for the prelude job to exec or keep its shell");
                }
                thread::yield_now();
                continue;
            }
        };
        let table = match ps_table() {
            Some(table) => table,
            None => {
                if deadline.left().is_zero() {
                    panic!("waited {bound:?} for the prelude job to exec or keep its shell");
                }
                thread::yield_now();
                continue;
            }
        };
        // `heads` holds only children of the shell, so a concurrent
        // job's `head` in another test process cannot satisfy the
        // Retained arm below: a sibling is never this shell's child.
        // Children whose command line still carries the fragment are
        // forked copies not yet exec'd, not parked holders.
        let has_head = table.iter().any(|(_, ppid, args)| {
            *ppid == pid && args.contains("head -n 1 hold") && !args.contains(bare)
        });
        if with.contains(&pid) {
            // The shell still carries the whole `-c` string and has
            // forked a parked `head` child.
            if has_head {
                break Form::Retained;
            }
        } else {
            // The fragment left the shell's own command line while the
            // child lives: bash exec'd the last line, so the child is
            // the `head` itself. The child's own row carries the
            // positive evidence, so a scan that simply missed the shell
            // cannot report the exec form.
            let own = table.iter().find(|(row, _, _)| *row == pid);
            if crate::pid_alive(pid)
                && matches!(own, Some((_, _, args)) if args.contains("head -n 1 hold") && !args.contains(bare))
            {
                break Form::Exec;
            }
        }
        if deadline.left().is_zero() {
            panic!("waited {bound:?} for the prelude job to exec or keep its shell");
        }
        thread::yield_now();
    };
    assert!(
        crate::pid_alive(pid),
        "the shell lives once classified, so the form names a live job"
    );
    eprintln!("prelude job form: {form:?}, shell: {program}");
    if cfg!(target_os = "linux") {
        assert_eq!(
            form,
            Form::Exec,
            "bash execs the last line of a multi-statement -c string, dropping the fragment"
        );
    }
    // One line releases the parked `head`, as the item finish path does;
    // then reap the child and stand the watchdog down.
    release.write_all(b"\n").unwrap();
    drop(release);
    let _output = crate::finished(
        "the released prelude job",
        child,
        &[],
        Deadline::after(MUST_SUCCEED_WITHIN),
    );
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

/// Every process's pid, parent pid and command line, or `None` when `ps`
/// cannot run: one `ps` call, so the caller treats `None` as "keep
/// waiting" and the outer deadline still fails naming the wait.
fn ps_table() -> Option<Vec<(u32, u32, String)>> {
    let child = Command::new("ps")
        .args(["ax", "-o", "pid=", "-o", "ppid=", "-o", "args="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let output = crate::wait_output(child, "ps to list the shell's children");
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let row: u32 = fields.next()?.parse().ok()?;
                let ppid: u32 = fields.next()?.parse().ok()?;
                Some((row, ppid, fields.collect::<Vec<_>>().join(" ")))
            })
            .collect(),
    )
}
