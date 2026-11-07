//! The shared support helpers' own tests, compiled once: every integration
//! binary declares `mod support`, so a `#[test]` under
//! `crates/main/tests/support/` runs in every binary. The helpers' tests
//! live in this one binary instead and run once (`docs/code-quality.md`,
//! "Size").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::Watchdog;
use serde_json::{Value, json};
use support::*;
/// Present in a re-executed child, absent in the parent: the child drives
/// the support wrapper into a stalled listing under a derived cleanup
/// budget.
const STALLED_LISTING_CHILD: &str = "FIBER_TEST_STALLED_LISTING_CHILD";

/// The stalled listing's marker, set by the parent for the child alone.
const STALLED_LISTING_MARKER: &str = "FIBER_TEST_STALLED_LISTING_MARKER";

/// The outer bound in milliseconds, set by the parent for the child alone.
const STALLED_LISTING_BOUND_MS: &str = "FIBER_TEST_STALLED_LISTING_BOUND_MS";

/// The support `kill_matching` wrapper funds its listing step from its
/// enclosing cleanup wait: the child reads a 400 ms outer bound from its
/// environment, derives a 400 ms within on an injected clock, and drives
/// the wrapper into a stalled `pgrep` shadow, asserting the returned error
/// (never a panic) names the `pgrep` step with the exact derived 200 ms
/// budget. The parent re-executes this test under a 30 s wall deadline
/// with a `PATH`-shadowed `pgrep` that blocks on a FIFO only for the
/// run's unique marker and execs the real `pgrep` otherwise (per-test
/// processes, so no other test's listing can block on this FIFO), guards
/// the stall by pid, and asserts the child's success before releasing the
/// FIFO; the expiry already killed and reaped the shadow through
/// production. The residual gate cost is one 200 ms wall-clock expiry
/// plus the re-executed child's boot, about a second on an idle host.
/// The wait cannot move onto the injected clock: the shadow is a real
/// `pgrep` parked in a FIFO `read`, and production `bounded()` takes a
/// plain `Duration` awaited through `Deadline::after` on the wall clock.
#[test]
fn kill_matching_with_a_stalled_listing_times_out_naming_pgrep_with_the_halved_cleanup_budget() {
    if std::env::var_os(STALLED_LISTING_CHILD).is_some() {
        stalled_listing_child();
        return;
    }
    // debt: this stalled-listing parent harness duplicates fakes'
    // stalled-pgrep parent harness (two copies): FIFO creation, PATH setup,
    // guarded rerun and cleanup stay duplicated; share them on the third,
    // once a public helper used only by tests stops widening the surface
    // this ticket narrows.
    let dir = fakes::TempDir::new("fiber-stalled-listing");
    // Unique to the run: the shadow stalls only this marker, and the
    // watchdog guards only it, so no other test's process can match.
    let marker = format!("stall-kill-{}", std::process::id());
    let fifo = dir.path().join("fifo");
    fakes::fifo(&fifo);
    // Resolved before shadowing: the shadow falls through to it for every
    // listing that is not this run's stall.
    let real = Command::new("sh")
        .args(["-c", "command -v pgrep"])
        .output()
        .expect("a shell resolves pgrep")
        .stdout;
    let real = String::from_utf8_lossy(&real).trim().to_owned();
    assert!(!real.is_empty(), "a real pgrep resolves before shadowing");
    // The production call passes the pattern form, so the shadow matches
    // the pattern literally: the raw marker never appears contiguously.
    // The marker is alphanumeric plus hyphens, so `fakes` wraps only the
    // first character in brackets.
    // debt: this stall shadow duplicates fakes' stalled-pgrep shadow (two
    // copies), and `[s]...` hand-rolls `pattern`, which stays `pub(crate)`
    // and unreachable here: share both on the third, once a public helper
    // used only by tests stops widening the surface this ticket narrows.
    let listing = format!("[{}]{}", &marker[..1], &marker[1..]);
    let body = format!(
        "for arg in \"$@\"; do\n  case \"$arg\" in\n    *'{}'*)\n      read line < '{}'\n      exit 0\n      ;;\n  esac\ndone\nexec '{}' \"$@\"\n",
        listing,
        fifo.display(),
        real,
    );
    fakes::script(dir.path(), "pgrep", &body);
    // Held open read+write across the rerun, so the shadow's open succeeds
    // and its read parks: the stall holds reliably past child startup, and
    // the 200 ms wall expiry fires at least 29,800 ms before the 30 s exit
    // cap can release it, with a 120 ms floor over the slowest healthy
    // `pgrep` spawn. Dropping it releases a shadow the expiry left behind,
    // which then exits at once.
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .expect("the test holds the stall FIFO open");
    // The guard kills this stall alone by its argv if the child hangs and
    // the expiry below never runs: never this test's own group.
    let stall_guard = Watchdog::matching(&marker);
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").expect("the parent reads PATH")
    );
    let output = fakes::rerun_within(
        "kill_matching_with_a_stalled_listing_times_out_naming_pgrep_with_the_halved_cleanup_budget",
        &[
            (STALLED_LISTING_CHILD, "1"),
            ("PATH", path.as_str()),
            (STALLED_LISTING_MARKER, marker.as_str()),
            (STALLED_LISTING_BOUND_MS, "400"),
        ],
        // A 30 s wall bound, the same as Task 1's proof: the re-executed
        // child's expected ≈2-4 s wall (boot plus setup plus the 200 ms
        // stall) fits inside the exit wait with margin — the 200 ms bound
        // fires at least 29,800 ms before the exit cap can release the
        // stall — with group cleanup inside a second 30 s.
        Deadline::after(Duration::from_secs(30)),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the stalled-listing child proves the derived budget:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1 passed"),
        "the child ran exactly the stalled-listing test: {stdout}"
    );
    drop(held);
    stall_guard.stand_down(Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

/// The re-executed child: a 400 ms env-fed outer cleanup wait on an
/// injected clock halves to the exact nonzero 200 ms listing step the
/// wrapper must fund.
#[track_caller]
fn stalled_listing_child() {
    let marker = std::env::var(STALLED_LISTING_MARKER).expect("the parent sets the stall marker");
    let outer_ms: u64 = std::env::var(STALLED_LISTING_BOUND_MS)
        .expect("the parent sets the outer bound")
        .parse()
        .expect("the outer bound is milliseconds");
    let clock: &'static Arc<fakes::clock::FakeClock> =
        Box::leak(Box::new(fakes::clock::FakeClock::new()));
    // 400 ms of cleanup from the environment on a clock that never
    // advances: the wrapper's outer wait is this cleanup phase, and its
    // listing step is half of it. The outer `within` receives on the wall
    // clock through this deadline, with margin for loaded-host scheduling.
    let wait = Deadline::on(&**clock).within(Duration::from_millis(outer_ms));
    let error = match kill_matching(wait, &marker) {
        Ok(()) => panic!("the stalled listing outlives the derived cleanup budget"),
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        ErrorKind::TimedOut,
        "the expiry is a timeout, not a pgrep failure"
    );
    assert_eq!(
        error.to_string(),
        "pgrep did not exit within 200ms",
        "the expiry names the pgrep step with the derived budget"
    );
}

#[test]
fn output_from_a_zero_exit_has_empty_text() {
    use std::os::unix::process::ExitStatusExt;
    let output = Output::from(std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    });
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout, "");
    assert_eq!(output.stderr, "");
}

#[test]
fn subscribe_direct_returns_last_seq_and_keeps_the_replay() {
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\",\"result\":{\"last_seq\":7}}}\n",
    ]);
    let sub = subscribe_direct(&client);
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert_eq!(sent["command"], "subscribe", "{sent}");
    assert!(sent.get("session_id").is_none(), "{sent}");
    assert_eq!(sub.last_seq, Some(7));
    assert!(sub.session.is_none());
    assert_eq!(
        recv(&client, "the replayed exit")
            .get("seq")
            .and_then(Value::as_u64),
        Some(5)
    );
    assert_eq!(
        recv(&client, "the replayed exit")
            .get("seq")
            .and_then(Value::as_u64),
        Some(6)
    );
}

#[test]
fn subscribe_direct_returns_no_bound_on_an_empty_log() {
    // The peer reads the subscribe line before answering, so it is still
    // alive when the send takes the socket timeout: a peer that only
    // writes can exit first, and on macOS the send then fails with
    // `EINVAL` instead of reaching the acknowledgement.
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\"}}\n",
    ]);
    let sub = subscribe_direct(&client);
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert_eq!(sub.last_seq, None);
}

#[test]
fn subscribe_direct_panics_on_a_rejected_subscribe() {
    // As above: the peer reads first, so the send cannot race its exit.
    let (client, _sent) = subscribe_peer(vec![
        "{\"kind\":\"command_rejected\",\"payload\":{\"command_id\":\"c_sub\",\"code\":\"gone\"}}\n",
    ]);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        subscribe_direct(&client);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("command_rejected"), "{message}");
}

#[test]
fn subscribe_names_the_session_and_returns_its_last_seq() {
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\",\"result\":{\"last_seq\":3}}}\n",
    ]);
    let sub = subscribe(&client, "s_9");
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert_eq!(sent["session_id"], "s_9", "{sent}");
    assert_eq!(sub.last_seq, Some(3));
    assert_eq!(sub.session, Some("s_9".to_owned()));
}

#[test]
fn from_ack_bounds_a_hand_sent_subscribe_with_and_without_last_seq() {
    let ack: Value = serde_json::from_str(
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_x\",\"result\":{\"last_seq\":7}}}",
    )
    .unwrap();
    let sub = Subscribed::from_ack(Some("s_1"), ack);
    assert_eq!(sub.last_seq, Some(7));
    assert_eq!(sub.session, Some("s_1".to_owned()));
    let ack: Value = serde_json::from_str(
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_x\"}}",
    )
    .unwrap();
    let sub = Subscribed::from_ack(None, ack);
    assert_eq!(sub.last_seq, None);
    assert!(sub.session.is_none());
}

#[test]
fn subscribe_as_sends_the_caller_command_id() {
    let (client, sent) = subscribe_peer(vec![
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_watch\"}}\n",
    ]);
    let sub = subscribe_as(&client, "c_watch", "s_2");
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_watch", "{sent}");
    assert_eq!(sent["session_id"], "s_2", "{sent}");
    assert_eq!(sub.last_seq, None);
    assert_eq!(sub.session, Some("s_2".to_owned()));
}

#[test]
fn subscribe_client_matches_the_acknowledgement_by_command_id() {
    let dir = fakes::TempDir::new("subscribe-client");
    let path = dir.path().join("session.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let (sent_tx, sent) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let line = read_line(&mut reader, deadline, "the subscribe line")
            .unwrap()
            .unwrap();
        sent_tx.send(line).unwrap_or(());
        let mut stream = stream;
        // A decoy answer for another command, then a decoy rejection of
        // this one: neither is the acknowledgement.
        for line in [
            "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_9\"}}\n",
            "{\"kind\":\"command_rejected\",\"payload\":{\"command_id\":\"c_sub\"}}\n",
            "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_sub\",\"result\":{\"last_seq\":11}}}\n",
        ] {
            write_line(&mut stream, deadline, line.as_bytes(), "the answers").unwrap();
        }
    });
    let client = fakes::Client::connect(&path).unwrap();
    let sub = subscribe_client(deadline, &client);
    let sent = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&sent)
        .expect("the peer read the subscribe line");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(sent["id"], "c_sub", "{sent}");
    assert!(sent.get("session_id").is_none(), "{sent}");
    assert_eq!(sub.last_seq, Some(11));
    assert!(sub.session.is_none());
}

#[test]
fn until_after_skips_the_replay_and_ends_on_the_live_line() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":4}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("seq").and_then(Value::as_u64));
        line.get("seq").and_then(Value::as_u64) == Some(6)
    });
    assert_eq!(*seen.lock().unwrap(), [Some(6)]);
    assert_eq!(
        lines
            .iter()
            .map(|line| line.get("seq").and_then(Value::as_u64))
            .collect::<Vec<_>>(),
        [Some(4), Some(5), Some(6)]
    );
}

#[test]
fn until_after_bounds_nothing_on_an_empty_log() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":1}\n",
    ]);
    let since = subscribed_since(None, Some("s_1"));
    let lines = until_after(&client, &since, "the first exit", |_| true);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("seq").and_then(Value::as_u64), Some(1));
}

#[test]
fn until_after_passes_ephemeral_and_other_session_lines() {
    let client = replay_peer(vec![
        "{\"kind\":\"notice\",\"payload\":{\"message\":\"hi\"}}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_2\",\"seq\":2}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after(&client, &since, "the pass-through lines", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("kind").and_then(Value::as_str).unwrap().to_owned());
        seen.lock().unwrap().len() == 2
    });
    assert_eq!(*seen.lock().unwrap(), ["notice", "fiber_exited"]);
    assert_eq!(lines.len(), 2);
}

#[test]
fn until_after_returns_every_line_read() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":4}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let lines = until_after(&client, &since, "the live exit", |line| {
        line.get("seq").and_then(Value::as_u64) == Some(6)
    });
    assert_eq!(
        lines,
        vec![
            json!({"kind":"fiber_exited","session_id":"s_1","seq":4}),
            json!({"kind":"fiber_exited","session_id":"s_1","seq":5}),
            json!({"kind":"fiber_exited","session_id":"s_1","seq":6}),
        ]
    );
}

#[test]
fn until_replayed_passes_only_the_replay() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let lines = until_replayed(&client, &since, "the replayed exit", |line| {
        assert_ne!(
            line.get("seq").and_then(Value::as_u64),
            Some(6),
            "a live line never reaches done: {line}"
        );
        line.get("seq").and_then(Value::as_u64) == Some(5)
    });
    assert_eq!(
        lines
            .iter()
            .map(|line| line.get("seq").and_then(Value::as_u64))
            .collect::<Vec<_>>(),
        [Some(6), Some(5)]
    );
}

#[test]
fn until_after_names_the_wait_when_the_socket_closes() {
    let (mine, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let client = Socket::from(Deadline::after(fakes::MUST_SUCCEED_WITHIN), mine);
    let since = subscribed_since(Some(5), Some("s_1"));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until_after(&client, &since, "the live exit", |_| true);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert_eq!(
        message,
        "the socket closed while waiting for the live exit; got []"
    );
}

#[test]
fn until_after_panics_on_a_new_fold_for_the_session() {
    let client = replay_peer(vec![
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_1\",\"last_seq\":14}}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until_after(&client, &since, "the live exit", |_| true);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("new Subscribed"), "{message}");
}

#[test]
fn until_after_passes_a_new_fold_for_another_session() {
    let client = replay_peer(vec![
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_2\",\"last_seq\":14}}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("kind").and_then(Value::as_str).unwrap().to_owned());
        line.get("seq").and_then(Value::as_u64) == Some(6)
    });
    assert_eq!(*seen.lock().unwrap(), ["stream_reopened", "fiber_exited"]);
    assert_eq!(lines.len(), 2);
}

#[test]
fn until_after_reopen_without_reopen_matches_until_after() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after_reopen(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("seq").and_then(Value::as_u64));
        line["kind"] == "fiber_exited" && line["seq"] == 6
    });
    assert_eq!(*seen.lock().unwrap(), [Some(6)]);
    assert_eq!(lines.len(), 2);
}

#[test]
fn until_after_reopen_skips_the_new_fold_and_waits_for_a_live_line() {
    let client = replay_peer(vec![
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":5}\n",
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_1\",\"last_seq\":14}}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":13}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":14}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":15}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after_reopen(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("seq").and_then(Value::as_u64));
        line["kind"] == "fiber_exited" && line["seq"] == 15
    });
    assert_eq!(*seen.lock().unwrap(), [Some(15)]);
    assert_eq!(lines.len(), 5);
}

#[test]
fn until_after_reopen_ignores_another_sessions_reopen() {
    let client = replay_peer(vec![
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_2\",\"last_seq\":14}}\n",
        "{\"kind\":\"fiber_exited\",\"session_id\":\"s_1\",\"seq\":6}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let seen = Mutex::new(Vec::new());
    let lines = until_after_reopen(&client, &since, "the live exit", |line| {
        seen.lock()
            .unwrap()
            .push(line.get("kind").and_then(Value::as_str).unwrap().to_owned());
        line["kind"] == "fiber_exited" && line["seq"] == 6
    });
    assert_eq!(*seen.lock().unwrap(), ["stream_reopened", "fiber_exited"]);
    assert_eq!(lines.len(), 2);
}

#[test]
fn until_after_reopen_names_a_reopen_line_without_last_seq() {
    let reopen = json!({"kind":"stream_reopened","payload":{"session_id":"s_1"}});
    let client = replay_peer(vec![
        "{\"kind\":\"stream_reopened\",\"payload\":{\"session_id\":\"s_1\"}}\n",
    ]);
    let since = subscribed_since(Some(5), Some("s_1"));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until_after_reopen(&client, &since, "the live exit", |_| true);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains(&reopen.to_string()), "{message}");
}

#[test]
fn hub_log_tail_shows_an_empty_log_empty() {
    assert_eq!(hub_log_tail(""), "");
}

#[test]
fn hub_log_tail_shows_a_short_log_whole() {
    assert_eq!(hub_log_tail("a\nb"), "a\nb");
}

#[test]
fn hub_log_tail_keeps_the_last_line() {
    let log = (1..=25)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = hub_log_tail(&log);
    assert!(
        tail.lines().any(|line| line == "line 25"),
        "the tail keeps the log's last line: {tail}"
    );
}

#[test]
fn hub_log_tail_drops_an_early_line() {
    let log = (1..=25)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = hub_log_tail(&log);
    assert!(
        !tail.lines().any(|line| line == "line 1"),
        "the tail drops the log's early lines: {tail}"
    );
}

#[test]
fn timeout_message_holds_the_old_first_line_and_all_three_sections() {
    let dir = fakes::TempDir::new("diag");
    let dir_path = dir.path().join("s_1");
    fs::create_dir_all(&dir_path).unwrap();
    fs::write(
        dir_path.join("events.jsonl"),
        "{\"kind\":\"session_started\"}\n{\"kind\":\"fiber_started\"}\n",
    )
    .unwrap();
    let stderr = Arc::new(Mutex::new("boom on fd 2\n".to_owned()));
    let diag = Diag::new(Arc::clone(&stderr), dir_path);
    let got = vec![json!({"kind": "command_accepted"})];
    let message = diag.report("the refused prompt", &got);
    let first = message.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "waited until the deadline for the refused prompt; got [Object {\"kind\": String(\"command_accepted\")}]"
    );
    assert!(message.contains("fiber stderr:"), "{message}");
    assert!(message.contains("boom on fd 2"), "{message}");
    assert!(
        message.contains("session log tail (events.jsonl, last 20 lines):"),
        "{message}"
    );
    assert!(
        message.contains("\"kind\":\"session_started\""),
        "{message}"
    );
}

#[test]
fn timeout_message_marks_empty_stderr_as_still_running() {
    let message = timeout_message("the refused prompt", &[], "", "{\"kind\":\"x\"}\n");
    assert!(
        message.contains("(still running: no stderr captured yet)"),
        "{message}"
    );
    let message = timeout_message("the refused prompt", &[], "late line\n", "");
    assert!(!message.contains("still running"), "{message}");
    assert!(message.contains("late line"), "{message}");
}

#[test]
fn closed_socket_dump_with_diag_shows_lines_stderr_and_log_tail() {
    let dir = fakes::TempDir::new("diag-closed");
    let dir_path = dir.path().join("s_9");
    fs::create_dir_all(&dir_path).unwrap();
    fs::write(
        dir_path.join("events.jsonl"),
        "{\"kind\":\"session_started\"}\n{\"kind\":\"fiber_exited\"}\n",
    )
    .unwrap();
    let stderr = Arc::new(Mutex::new("boom on fd 2\n".to_owned()));
    let diag = Diag::new(Arc::clone(&stderr), dir_path);
    // One line arrives before the peer goes away, so the panic names a
    // received line; the close itself needs no timing.
    let (mine, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"{\"kind\":\"command_accepted\"}\n")
        .unwrap();
    drop(peer);
    let mut client = Socket::from(Deadline::after(fakes::MUST_SUCCEED_WITHIN), mine);
    client.set_diag(diag);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until(&client, "the stopped child exits", |_| false);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    let first = message.lines().next().unwrap_or_default();
    assert_eq!(
        first, "the socket closed while waiting for the stopped child exits",
        "{message}"
    );
    assert!(message.contains("command_accepted"), "{message}");
    assert!(message.contains("fiber stderr:"), "{message}");
    assert!(message.contains("boom on fd 2"), "{message}");
    assert!(
        message.contains("session log tail (events.jsonl, last 20 lines):"),
        "{message}"
    );
    assert!(
        message.contains("\"kind\":\"session_started\""),
        "{message}"
    );
    // A closed socket already ended, so an empty snapshot says nothing was
    // captured, never that the session is still running.
    let empty = closed_message("the stopped child exits", &[], "", "{\"kind\":\"x\"}\n");
    assert!(empty.contains("(no stderr captured)"), "{empty}");
    assert!(!empty.contains("still running"), "{empty}");
}

#[test]
fn closed_socket_dump_keeps_stderr_and_log_tail_within_the_output_cap() {
    // The CI output cap keeps the first 6000 characters: with a long
    // received-lines dump, the stderr and log-tail sections still fit
    // because the dump comes last.
    const OUTPUT_CAP: usize = 6000;
    let stderr_sentinel = "stderr-sentinel-9f3a1c";
    let log_sentinel = "\"kind\":\"log-tail-sentinel-7e2b\"";
    let got: Vec<Value> = (0..2000)
        .map(|n| json!({"kind": "filler", "n": n, "pad": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}))
        .collect();
    let message = closed_message(
        "the stopped child exits",
        &got,
        stderr_sentinel,
        log_sentinel,
    );
    assert!(
        message.len() > OUTPUT_CAP,
        "dump fits the cap: {}",
        message.len()
    );
    let visible = &message[..OUTPUT_CAP];
    // A large value never prints whole on failure (`docs/testing.md`, "What
    // a test asserts"): report the length and the offsets instead.
    assert!(
        visible.contains("fiber stderr:"),
        "stderr header missing from first {OUTPUT_CAP} of {}-byte message",
        message.len()
    );
    assert!(
        visible.contains(stderr_sentinel),
        "stderr sentinel at {} outside first {OUTPUT_CAP} of {}-byte message",
        message.find(stderr_sentinel).unwrap_or(usize::MAX),
        message.len()
    );
    let tail_header = "session log tail (events.jsonl, last 20 lines):";
    assert!(
        visible.contains(tail_header),
        "log-tail header missing from first {OUTPUT_CAP} of {}-byte message",
        message.len()
    );
    assert!(
        visible.contains(log_sentinel),
        "log-tail sentinel at {} outside first {OUTPUT_CAP} of {}-byte message",
        message.find(log_sentinel).unwrap_or(usize::MAX),
        message.len()
    );
    // The diagnostics precede the dump: flipping the order back would
    // push both sentinels past the cap.
    let stderr_at = message.find(stderr_sentinel).unwrap_or(usize::MAX);
    let log_at = message.find(log_sentinel).unwrap_or(usize::MAX);
    let dump_at = message.find("got [").unwrap_or(usize::MAX);
    assert!(
        stderr_at < dump_at,
        "stderr sentinel at {stderr_at} not before dump at {dump_at} ({}-byte message)",
        message.len()
    );
    assert!(
        log_at < dump_at,
        "log-tail sentinel at {log_at} not before dump at {dump_at} ({}-byte message)",
        message.len()
    );
}

#[test]
fn until_without_diag_keeps_the_old_closed_message() {
    let (mine, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let client = Socket::from(Deadline::after(fakes::MUST_SUCCEED_WITHIN), mine);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until(&client, "the stopped child exits", |_| false);
    }))
    .unwrap_err();
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert_eq!(
        message,
        "the socket closed while waiting for the stopped child exits; got []"
    );
}

#[test]
fn session_log_tail_keeps_only_complete_lines() {
    // No asserted count cites a `docs/<area>.md` line promising it
    // (`docs/code-quality.md`, "What a reviewer checks"): the tail's width
    // is the helper's own bound, so this test pins presence and recency,
    // not the bound. A short log comes back whole.
    let dir = fakes::TempDir::new("diag-tail");
    assert_eq!(session_log_tail(&dir.path().join("missing")), "");
    let file = dir.path().join("s_2");
    fs::create_dir_all(&file).unwrap();
    fs::write(file.join("events.jsonl"), "{\"kind\":\"partial\"}").unwrap();
    assert_eq!(session_log_tail(&file), "");
    let short: String = (0..5).map(|n| format!("{{\"n\":{n}}}\n")).collect();
    fs::write(file.join("events.jsonl"), &short).unwrap();
    assert_eq!(session_log_tail(&file), short.trim_end());
    // A long log keeps its last line and drops an early one; a trailing
    // fragment without its newline is still being written, so it is dropped.
    let long: String = (0..40).map(|n| format!("{{\"n\":{n}}}\n")).collect();
    fs::write(file.join("events.jsonl"), &long).unwrap();
    let tail = session_log_tail(&file);
    assert!(tail.contains("{\"n\":39}"), "{tail}");
    assert!(!tail.contains("{\"n\":0}"), "{tail}");
    let mut cut = long.clone();
    cut.push_str("{\"n\":40");
    fs::write(file.join("events.jsonl"), &cut).unwrap();
    let tail = session_log_tail(&file);
    assert!(!tail.contains("{\"n\":40"), "{tail}");
    assert!(tail.contains("{\"n\":39}"), "{tail}");
}

#[test]
fn session_logs_tail_names_each_session_in_sorted_order() {
    // `read_dir` order is the filesystem's, so the assertion pins the
    // sorted output the function promises, not the listing order: the
    // folders are created out of sorted order on purpose.
    let sessions = fakes::TempDir::new("diag-tails");
    for (name, log) in [
        ("s_b", "{\"b\":0}\n{\"b\":1}\n"),
        ("s_a", "{\"a\":0}\n{\"a\":1"),
    ] {
        let dir = sessions.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), log).unwrap();
    }
    // A stray non-directory entry is skipped without a panic and does
    // not appear in the report.
    fs::write(sessions.path().join("stray"), "x").unwrap();
    assert_eq!(
        session_logs_tail(sessions.path()),
        "session s_a:\n{\"a\":0}\nsession s_b:\n{\"b\":0}\n{\"b\":1}"
    );
    assert_eq!(session_logs_tail(&sessions.path().join("missing")), "");
}

#[test]
fn stderr_drain_keeps_a_live_snapshot_of_the_full_text() {
    let snapshot = Arc::new(Mutex::new(String::new()));
    let mut pipe = io::Cursor::new(b"first line\nsecond line\n".to_vec());
    let text = drain_stderr_to_snapshot(&mut pipe, &snapshot);
    assert_eq!(text, "first line\nsecond line\n");
    assert_eq!(*snapshot.lock().unwrap(), "first line\nsecond line\n");
    let mut empty = io::Cursor::new(Vec::new());
    let blank = Arc::new(Mutex::new(String::new()));
    assert_eq!(drain_stderr_to_snapshot(&mut empty, &blank), "");
    assert_eq!(*blank.lock().unwrap(), "");
}

/// A reader that dribbles out at most `max` bytes per read, so a multibyte
/// character lands split across reads.
struct Dribble<'a> {
    rest: &'a [u8],
    max: usize,
}

impl io::Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.rest.is_empty() {
            return Ok(0);
        }
        let take = self.rest.len().min(buf.len()).min(self.max);
        buf[..take].copy_from_slice(&self.rest[..take]);
        self.rest = &self.rest[take..];
        Ok(take)
    }
}

#[test]
fn stderr_drain_keeps_a_character_split_across_reads() {
    // One byte per read splits `é` and `ö` across reads: decoding each read
    // alone would leave a replacement character per half.
    let snapshot = Arc::new(Mutex::new(String::new()));
    let mut pipe = Dribble {
        rest: "héllo wörld\n".as_bytes(),
        max: 1,
    };
    let text = drain_stderr_to_snapshot(&mut pipe, &snapshot);
    assert_eq!(text, "héllo wörld\n");
    assert_eq!(*snapshot.lock().unwrap(), "héllo wörld\n");
    assert!(
        !snapshot.lock().unwrap().contains('\u{FFFD}'),
        "{}",
        snapshot.lock().unwrap()
    );
}

/// A reader that yields `rest` once, then fails every read after.
struct FailAfter<'a> {
    rest: Option<&'a [u8]>,
}

impl io::Read for FailAfter<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.rest.take() {
            Some(chunk) => {
                let take = chunk.len().min(buf.len());
                buf[..take].copy_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    self.rest = Some(&chunk[take..]);
                }
                Ok(take)
            }
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "stub read error")),
        }
    }
}

#[test]
fn stderr_drain_sibling_preserves_the_first_read_error() {
    // The sibling keeps the text and the snapshot exactly as the wrapper
    // would, and carries the first read error beside them.
    let snapshot = Arc::new(Mutex::new(String::new()));
    let mut pipe = FailAfter {
        rest: Some(b"half\n"),
    };
    let (text, error) = drain_stderr_to_snapshot_reported(&mut pipe, &snapshot);
    assert_eq!(text, "half\n");
    assert_eq!(*snapshot.lock().unwrap(), "half\n");
    assert!(
        error.is_some_and(|error| error.contains("stub read error")),
        "the sibling names the first read error"
    );
    // The wrapper drops the error but keeps the text and the snapshot, so
    // every existing caller reads exactly what it read before.
    let blank = Arc::new(Mutex::new(String::new()));
    let mut again = FailAfter {
        rest: Some(b"half\n"),
    };
    assert_eq!(drain_stderr_to_snapshot(&mut again, &blank), "half\n");
    assert_eq!(*blank.lock().unwrap(), "half\n");
}

#[test]
fn captured_stderr_marks_an_incomplete_drain() {
    // No completion sent: the drain is still running, so a zero bound
    // expires at once and the clone carries what arrived with the marker,
    // never a panic. The sender stays alive, so the miss is a timeout, not
    // a disconnect.
    let snapshot = Arc::new(Mutex::new("half\n".to_owned()));
    let (_live, drained) = mpsc::channel::<()>();
    let capture = CapturedStderr { snapshot, drained };
    let text = capture.text(Deadline::after(Duration::ZERO));
    assert!(
        text.contains("half\n"),
        "an incomplete drain still clones what arrived: {text}"
    );
    assert!(
        text.contains("(stderr drain incomplete: showing what arrived)"),
        "an incomplete drain reads as its own marker, never as whole: {text}"
    );
    // A drained capture clones whole with no marker, even on a zero bound:
    // the completion is already there.
    let whole = CapturedStderr::drained_text("all\n".to_owned());
    assert_eq!(whole.text(Deadline::after(Duration::ZERO)), "all\n");
}

#[test]
fn run_kinds_sets_aside_independently_triggered_lines() {
    // `Run::kinds` delegates to the free helper, so a `date_changed` line
    // the contract writes on its own trigger never reaches an ordered
    // expectation (`docs/testing.md`, "Event streams"; #3758).
    let run = Run {
        code: Some(0),
        lines: vec![
            json!({"kind": "preamble_built"}),
            json!({"kind": "clients"}),
            json!({"kind": "session_status"}),
            json!({"kind": "turn_started"}),
            json!({"kind": "attention"}),
            json!({"kind": "date_changed"}),
            json!({"kind": "turn_completed"}),
        ],
        stderr: String::new(),
    };
    assert_eq!(
        run.kinds(),
        ["preamble_built", "turn_started", "turn_completed"]
    );
}

/// A held file between a signal and its removal: the writer signals, then
/// blocks until the test releases it, then removes the state.
struct PausedRemoval {
    release: mpsc::Sender<()>,
    removed: mpsc::Receiver<()>,
}

fn pause_before_removal(path: PathBuf) -> PausedRemoval {
    let (release_tx, release) = mpsc::channel::<()>();
    let (removed_tx, removed) = mpsc::channel::<()>();
    thread::spawn(move || {
        release.recv().unwrap_or(());
        fs::remove_file(&path).unwrap_or(());
        removed_tx.send(()).unwrap_or(());
    });
    PausedRemoval {
        release: release_tx,
        removed,
    }
}

#[test]
fn held_wait_reports_the_inner_wait_when_the_waiter_panics() {
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        held_wait_passes_after_release(
            "the inner step",
            fakes::MUST_SUCCEED_WITHIN,
            |_: Deadline| -> () {
                panic!("the inner boom");
            },
            || {},
        );
    }))
    .expect_err("the helper must fail when the waiter panics");
    let text = failure
        .downcast_ref::<String>()
        .cloned()
        .expect("the helper re-raises a panic text");
    assert!(
        text.contains("the inner step"),
        "the failure names the inner wait: {text}"
    );
    assert!(
        text.contains("the inner boom"),
        "the failure carries the waiter panic: {text}"
    );
}

#[test]
fn held_wait_returns_the_released_wait_result() {
    let result =
        held_wait_passes_after_release("the inner step", fakes::MUST_SUCCEED_WITHIN, |_| 7, || {});
    assert_eq!(result, 7, "the helper returns the waiter's value");
}

#[test]
fn held_wait_outer_bound_is_three_inner_budgets() {
    assert_eq!(OUTER_BUDGET_FACTOR, 3);
}

#[test]
fn path_gone_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-path-gone");
    let path = dir.path().join("gone");
    fs::write(&path, "state").unwrap();
    // The old wait is the bare assertion right after the signal: with the
    // removal held, it fails, proving the signal alone is not the proof.
    let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(!path.exists(), "the path is gone");
    }));
    assert!(held.is_err(), "the bare assertion must fail while held");
    let paused = pause_before_removal(path.clone());
    let release = paused.release.clone();
    held_wait_passes_after_release(
        "the held path to go",
        fakes::MUST_SUCCEED_WITHIN,
        move |deadline| wait_for_path_gone(deadline, &path, "the held path to go"),
        move || release.send(()).unwrap_or(()),
    );
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&paused.removed)
        .expect("the remover finished removing the path");
}

#[test]
fn lock_released_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-lock");
    let held_guard = log::try_hold(dir.path()).unwrap();
    assert!(
        matches!(held_guard, log::Hold::Held(_)),
        "the test holds the lock"
    );
    // The old wait probes once right after the exit signal: with the lock
    // still held, the probe is busy, so the bare assertion fails.
    let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            matches!(log::try_hold(dir.path()), Ok(log::Hold::Held(_))),
            "the session lock is released"
        );
    }));
    assert!(held.is_err(), "the bare probe must fail while held");
    let waited = dir.path().to_path_buf();
    held_wait_passes_after_release(
        "the held session lock",
        fakes::MUST_SUCCEED_WITHIN,
        move |deadline| wait_for_lock_released(deadline, &waited, "the held session lock"),
        move || drop(held_guard),
    );
}

#[test]
fn effect_after_accept_ignores_other_ids_and_passes_on_its_own() {
    fn parked_pair() -> (Socket, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        let (mine, peer) = UnixStream::pair().unwrap();
        let client = Socket::from(deadline, mine);
        let (release_tx, release) = mpsc::channel::<()>();
        let (accepted_tx, accepted) = mpsc::channel::<()>();
        thread::spawn(move || {
            let mut peer = peer;
            write_line(
                &mut peer,
                deadline,
                b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_1\"}}\n",
                "the acceptance",
            )
            .unwrap();
            accepted_tx.send(()).unwrap_or(());
            release.recv().unwrap_or(());
            // An unrelated answer for another command: result-bearing, but
            // the helper must not complete on it.
            write_line(
                &mut peer,
                deadline,
                b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_9\",\"result\":{\"answer\":2}}}\n",
                "the unrelated answer",
            )
            .unwrap();
            write_line(
                &mut peer,
                deadline,
                b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_1\",\"result\":{\"answer\":1}}}\n",
                "the answer",
            )
            .unwrap();
        });
        (client, accepted, release_tx)
    }
    // The old wait stops at the acceptance and reads the result off it:
    // with the command parked on a bare acceptance, there is none.
    let (old_client, old_accepted, old_release) = parked_pair();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&old_accepted)
        .expect("the parked command was accepted");
    let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let line = old_client
            .next("the acceptance", &[])
            .expect("the acceptance arrived");
        assert_eq!(line["kind"], "command_accepted");
        assert!(
            line["payload"].get("result").is_some(),
            "the answer is already there: {line}"
        );
    }));
    assert!(old.is_err(), "the old wait must fail while parked");
    old_release.send(()).unwrap_or(());
    // The helper waits past the acceptance for the id-matched answer on a
    // fresh pair: the waiter signals it entered, the unrelated answer must
    // not complete it, and the release ends it at its own answer.
    let (client, accepted, release) = parked_pair();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&accepted)
        .expect("the parked command was accepted");
    let lines = held_wait_passes_after_release(
        "the c_1 answer",
        fakes::MUST_SUCCEED_WITHIN,
        move |deadline| {
            wait_for_effect_after_accept(
                "the c_1 answer",
                "c_1",
                |what, got| client.next_through(&deadline, what, got),
                |line| line["payload"].get("result").is_some(),
            )
        },
        move || release.send(()).unwrap_or(()),
    );
    assert!(
        lines
            .iter()
            .any(|line| line["payload"]["command_id"] == "c_9"),
        "the wait saw the unrelated answer and skipped it: {lines:?}"
    );
    let last = lines.last().unwrap();
    assert_eq!(last["payload"]["command_id"], "c_1", "{lines:?}");
    assert_eq!(last["payload"]["result"], json!({"answer": 1}), "{lines:?}");
}

#[test]
fn log_line_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-log-line");
    let log = dir.path().join("hub.log");
    fs::write(&log, "signal\n").unwrap();
    // The watch registers before the writer thread starts, so nothing the
    // writer appends lands between registration and the first wait unseen.
    // Shared with the waiter thread: every assertion stays on the test
    // thread, where a failure fails the test.
    let watch = std::sync::Arc::new(watch_log(
        &log,
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        "the state line",
    ));
    // The old wait reads once right after the signal and asserts the state
    // line is there: with the writer held, it is not.
    let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let text = fs::read_to_string(&log).unwrap_or_default();
        assert!(text.contains("state"), "the state line landed: {text}");
    }));
    assert!(old.is_err(), "the old wait must fail while held");
    let (release_tx, release) = mpsc::channel::<()>();
    let logged = log.clone();
    let writer = thread::spawn(move || {
        release.recv().unwrap_or(());
        let mut file = fs::OpenOptions::new().append(true).open(&logged).unwrap();
        file.write_all(b"state\n").unwrap();
    });
    let waited = std::sync::Arc::clone(&watch);
    let exit = held_wait_passes_after_release(
        "the state line",
        fakes::MUST_SUCCEED_WITHIN,
        move |deadline| {
            waited
                .wait_until(deadline, "the state line", |text| text.contains("state"))
                .unwrap_or_else(|error| panic!("{error}"))
        },
        move || release_tx.send(()).unwrap_or(()),
    );
    // A lost wake does not fail the wait: the loop rereads after expiry and
    // returns `Ok`. `Ok` plus a last park that expired IS the lost wake.
    assert_ne!(
        exit,
        ParkExit::Expired,
        "the released wait ended by expiry: a lost wake"
    );
    // The writer ended once its append landed, so the join returns at once;
    // a writer panic re-raises with its own text on this thread.
    match writer.join() {
        Ok(()) => {}
        Err(payload) => std::panic::resume_unwind(payload),
    }
    // The landed line stays visible to a later wait on the same watch.
    wait_for_watched_line(
        &watch,
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        "the already-landed state line",
        |text| text.contains("state"),
    );
}

#[test]
fn refresh_exit_fails_fast_while_held_and_passes_after_release() {
    let dir = fakes::TempDir::new("wait-refresh-exit");
    let fifo = dir.path().join("hold");
    let pattern = format!("wait-refresh-exit-{}", dir.path().display());
    // The marker rides the child's command line, so the match is this
    // test's own; the FIFO keeps the child parked between the old wait's
    // signal and the exit: opening it for read blocks until the test
    // writes the release line through the hold's release end.
    let command = format!(
        "read line < '{fifo}'; exit 0 # {pattern}",
        fifo = fifo.display()
    );
    // The hold owns the FIFO, the release end, and the watchdog: if this
    // test dies while the child is parked, the kernel closes the hold's
    // ends, the watchdog sees end of file, and its group kill reaps the
    // child. The command embeds the FIFO path, so the watchdog pattern
    // is the path itself.
    let mut hold = fakes::Hold::park(&fifo, &command);
    let child = Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    // A reaper, so the exit leaves no zombie for the match to keep seeing.
    // It waits only: the watchdog owns the kill, and the hold owns the
    // teardown below.
    let (reaped_tx, reaped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut child = child;
        match child.wait() {
            Ok(_) | Err(_) => {}
        }
        reaped_tx.send(()).unwrap_or(());
    });
    // The parked child is listed before the release goes out, so a vacuous
    // pass cannot hide a holder that never parked.
    let listed = pattern.clone();
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN).start_wait();
    fakes::wait_until(
        "the parked refresh child to list",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || match fakes::matching(&listed, wait.left() / 2) {
            Ok(pids) => pids.contains(&pid),
            Err(error) => panic!("listing the parked refresh child: {error}"),
        },
    );
    // The old wait checks the match at once: the parked child is still
    // listed, so the bare assertion fails.
    let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            fakes::matching(&pattern, fakes::MUST_SUCCEED_WITHIN / 2)
                .unwrap_or_default()
                .is_empty(),
            "the refresh child already exited"
        );
    }));
    assert!(old.is_err(), "the old wait must fail while held");
    let waited = pattern.clone();
    // The release is one line written through the hold's open release end:
    // the parked read consumes it, the child exits, and the reaper clears
    // it. The write happens before the wait below, never after.
    held_wait_passes_after_release(
        &pattern,
        fakes::MUST_SUCCEED_WITHIN,
        move |deadline| wait_for_refresh_exit(deadline, &waited),
        || {
            hold.release()
                .write_all(b"\n")
                .expect("the release line reaches the parked refresh child");
        },
    );
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&reaped)
        .expect("the reaper cleared the exited child");
    // The holder already exited on the success path: the stand-down drops
    // the release handle and disarms the watchdog without signalling.
    hold.stand_down(Deadline::after(fakes::MUST_SUCCEED_WITHIN).cleanup_phase());
}

/// The child half of `holder_dies_with_the_test_process_that_parked_it`:
/// parks a holder on a FIFO, acks its pid on stdout, then blocks forever.
/// The outer test SIGKILLs this process: the watchdog sees end of file,
/// and its group kill reaps the holder. Ignored in the normal suite; the
/// outer test re-runs this helper with `--exact ... --ignored`.
#[test]
#[ignore = "re-run by holder_dies_with_the_test_process_that_parked_it with --exact --ignored"]
#[allow(clippy::print_stdout, reason = "the parent test reads this line")]
fn holder_dies_child_parks_a_holder() {
    // Standalone guard: `--run-ignored` runs every ignored test in this
    // binary (look-diff's comparator, leak-probe's run), and this helper
    // blocks forever unless the outer test drives it. Without the marker
    // only the outer test sets on the child, pass quietly.
    if std::env::var_os("HOLDER_DIES_CHILD").is_none() {
        return;
    }
    let dir = fakes::TempDir::new("holder-dies-child");
    let fifo = dir.path().join("hold");
    // No `Hold` release end here: the FIFO starts with no pre-opened ends,
    // so the holder's `read` blocks opening until the outer writer opens
    // for write. That open succeeding is the acknowledgement at the
    // blocking point itself, not a step before it.
    fakes::fifo(&fifo);
    let ready = fakes::children::Ready::new(dir.path());
    let pattern = format!("holder-dies-{}", dir.path().display());
    // The command embeds the FIFO path, so the watchdog pattern is the
    // path itself: unique to this run through the fresh directory above.
    // The pid line follows in program order, so its arrival proves the
    // holder started; the outer writer open below proves the holder's
    // `read` opened the FIFO, and the held writer keeps it parked there.
    let command = format!(
        "echo $$ > '{ready}'; read line < '{fifo}'; exit 0 # {pattern}",
        fifo = fifo.display(),
        ready = ready.path().display(),
    );
    let fifo_text = fifo
        .to_str()
        .expect("the holder's FIFO path is UTF-8")
        .to_owned();
    let _watchdog = Watchdog::matching(&fifo_text);
    let holder = Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("the parked holder to spawn");
    thread::spawn(move || {
        let mut holder = holder;
        match holder.wait() {
            Ok(_) | Err(_) => {}
        }
    });
    let pid = ready.wait(Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    println!("parked {pid} {}", fifo.display());
    io::stdout()
        .flush()
        .expect("the parked line to reach the outer test");
    // Block forever: the outer test SIGKILLs this process, and the
    // watchdog owns the holder from there.
    let (_never, forever) = mpsc::channel::<()>();
    match forever.recv() {
        Ok(()) | Err(_) => {}
    }
}

#[test]
fn holder_dies_with_the_test_process_that_parked_it() {
    // The reader thread starts before the child spawns, so the ack line
    // cannot land before the reader exists; the pipe crosses on a handoff
    // channel once the spawn makes it.
    let (parked_tx, parked) = mpsc::channel::<(u32, PathBuf)>();
    let (out_tx, out_rx) = mpsc::channel();
    thread::spawn(move || {
        let out = match out_rx.recv() {
            Ok(out) => out,
            Err(_) => return,
        };
        let mut reader = BufReader::new(out);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            // The harness's `test ... ... ` prefix shares the ack's line,
            // so the marker can start mid-line: search, never strip.
            if let Some(start) = line.find("parked ")
                && let Some((pid, fifo)) =
                    line[start + "parked ".len()..].trim_end().split_once(' ')
                && let Ok(pid) = pid.parse()
            {
                match parked_tx.send((pid, PathBuf::from(fifo))) {
                    Ok(()) | Err(_) => return,
                }
            }
        }
    });
    let exe = std::env::current_exe().expect("the test binary knows its path");
    let mut child = Command::new(exe)
        .env("HOLDER_DIES_CHILD", "1")
        .args([
            "--exact",
            "holder_dies_child_parks_a_holder",
            "--nocapture",
            "--test-threads=1",
            "--ignored",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .expect("the child test process to spawn");
    let pid = child.id();
    // Guard the group before anything that can panic (the watchdog start,
    // the stdout take): like `spawn_watched`, a `KillGroup` owns the kill
    // until the watchdog takes it, so a panic cannot leak the child.
    let guard = KillGroup(pid);
    let group = Watchdog::group(pid);
    std::mem::forget(guard);
    out_tx
        .send(child.stdout.take().expect("the child to pipe its stdout"))
        .unwrap_or(());
    // A reaper, so the SIGKILLed child leaves no zombie; the watchdog
    // above owns the kill, this thread only waits.
    let (reaped_tx, reaped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut child = child;
        match child.wait() {
            Ok(_) | Err(_) => {}
        }
        reaped_tx.send(()).unwrap_or(());
    });
    // The ack names the holder pid and its FIFO: the pid first, then the
    // path verbatim, so no space-splitting of the path.
    let (holder, fifo): (u32, PathBuf) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&parked, "the parked holder's ack line");
    let pattern = fifo
        .to_str()
        .expect("the holder's FIFO path is UTF-8")
        .to_owned();
    // The outer writer is the controlled hold at the blocking point. The
    // FIFO has no pre-opened ends (no `Hold` release), so this write-end
    // open blocks until the holder's `read` opens the FIFO. With the end
    // held open and unwritten, the holder has exactly one possible future:
    // block in `read`. The open-to-`read` window holds no syscalls,
    // signals, or exit paths. Alive-at-kill comes from the `matching`
    // listing below containing the acked pid, not from the open alone.
    // This is not kernel-level proof that the holder sits inside `read(2)`
    // at the kill instant. The ack strictly precedes the block, so a
    // scheduler preemption between them stays theoretically possible.
    // Reading the holder's wait state instead (`/proc/<pid>/stat` or
    // `wchan`) does not exist on Darwin. A proof that runs on only one of
    // the two platforms the suite runs on is worse than a sound causal
    // argument. #4077 asks that the holder not outlive its test process,
    // not where in the kernel it is parked.
    let (writing_tx, writing) = mpsc::channel();
    thread::spawn({
        let fifo = fifo.clone();
        move || {
            if let Ok(file) = fs::OpenOptions::new().write(true).open(&fifo) {
                writing_tx.send(file).unwrap_or(());
            }
        }
    });
    let _outer = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&writing, "the holder's read to open the FIFO");
    // The listing names the acked pid, tying the listed process to the
    // acknowledged reader and closing the spawn-vs-list race.
    let listed = pattern.clone();
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN).start_wait();
    fakes::wait_until(
        "the parked holder to list",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || match fakes::matching(&listed, wait.left() / 2) {
            Ok(pids) => pids.contains(&holder),
            Err(error) => panic!("listing the parked holder: {error}"),
        },
    );
    // The holder lives in another group, so this SIGKILL reaches only the
    // child: the watchdog owns the holder from here.
    fakes::kill_pid(pid, "KILL").expect("SIGKILL to reach the child test process");
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&reaped)
        .expect("the SIGKILLed child to be reaped");
    // The child is reaped, so the drop path no longer owns it: disarm the
    // watchdog without signalling.
    group.stand_down(Deadline::after(fakes::MUST_SUCCEED_WITHIN).cleanup_phase());
    if let Err(err) =
        fakes::try_matching_exits(&pattern, Deadline::after(fakes::MUST_SUCCEED_WITHIN))
    {
        // The exact group id from the ack line, no pattern sweep: the
        // holder is the group leader, and the outer writer has kept it
        // parked, so the pid cannot have been reused.
        match fakes::kill_group(holder, "KILL") {
            Ok(_) | Err(_) => {}
        }
        panic!("the holder outlived the SIGKILLed test process that parked it: {err}");
    }
    // Best-effort: the SIGKILLed child never dropped its own directory.
    drop(fs::remove_dir_all(
        fifo.parent().expect("the holder's FIFO has a directory"),
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn platform_watch_catches_write_before_first_wait() {
    let dir = fakes::TempDir::new("watch-pre-write");
    let log = dir.path().join("hub.log");
    fs::write(&log, "signal\n").unwrap();
    // Registration precedes the first wait, so the write below lands after
    // registration and before any wait: the queued event must end the park.
    let watch = PlatformWatch::register(&log).expect("registering the log watch");
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"state\n")
        .unwrap();
    let (done_tx, done) = mpsc::channel::<ParkExit>();
    thread::spawn(move || {
        let exit = watch
            .wait_once(fakes::MUST_SUCCEED_WITHIN)
            .expect("the parked wait");
        done_tx.send(exit).unwrap_or(());
    });
    // The verdict comes from `ParkExit`, never from elapsed time: 20 s is a
    // hang guard under `docs/testing.md, "Testing"`, not a timing assertion.
    let exit = Deadline::after(fakes::MUST_SUCCEED_WITHIN * 2) // ruling: #4075
        .recv_or_fail(
            &done,
            "the wait returns for a write made before the first wait",
        );
    assert_ne!(
        exit,
        ParkExit::Expired,
        "the released wait ended by expiry: a lost wake"
    );
}
