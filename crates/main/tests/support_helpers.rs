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

#[path = "support/hub_start.rs"]
#[allow(
    dead_code,
    reason = "the moved tests cover the failure paths; ATTEMPTS, free_port and the Served fields stay live through the other binaries"
)]
mod hub_start;
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
use support::pty::{
    Grid, Pause, Run, SYNC_BEGIN, SYNC_END, click_bytes, open, wait_for_workspace_holders,
};
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

// No asserted count cites a `docs/<area>.md` line promising it
// (`docs/code-quality.md`, "What a reviewer checks"): the tail's width is
// the helper's own bound, so these tests pin presence and recency, not the
// bound. A long log keeps its last line and drops an early one.
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
    let run = support::Run {
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
    // cannot land before the reader exists; the pipe crosses on a compaction
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

/// `wait_screen` panics with the grid its last check saw, never a
/// fresh read, and names how much of the deadline remained when the
/// wait began. The marker reaches the run's own terminal but its grid
/// stays stale behind a synchronized-output block, so the checked
/// grid holds no marker while the output does; the predicate then
/// publishes the held bytes to the grid synchronously after its check,
/// with no wake, so the timeout panic must show the checked grid while
/// a fresh read sees the marker. Replacing `grid.contents` with
/// `self.screen().contents` in the panic makes the shown grid contain
/// the marker and fails the equality below. The test uses no sleep
/// (`docs/testing.md`, "Waits and timeouts").
#[test]
fn wait_screen_panics_with_the_checked_grid_and_names_the_deadline_left() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let mut terminal_side = terminal.terminal;
    let mut run = Run::attach(main, 80, 24, deadline);
    // The marker goes through the run's own terminal: the reader
    // publishes it to the output but holds the grid behind the sync
    // block, so the grid stays stale while the output advances.
    let from = run.output().len();
    terminal_side.write_all(SYNC_BEGIN).unwrap();
    terminal_side.write_all(b"marker").unwrap();
    run.wait_bytes(from, b"marker", "the fed marker");
    assert!(
        !run.screen().contents.contains("marker"),
        "the grid stays stale behind the sync block"
    );
    // Stop and join the reader before draining wakes: the publish
    // releases the shared lock before its wake is sent, so a
    // nonblocking drain can run before the wake lands and the expired
    // wait below would consume it and check twice (`docs/testing.md`,
    // "Races are forced, not waited for"). After the join no more
    // wakes arrive, so the drain leaves none pending and the wait
    // times out on its first receive.
    let reader = run.reader.take().expect("the run has a reader");
    assert!(reader.stop(), "expected the reader to stop");
    while run.wakes.try_recv().is_ok() {}
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAITS);
    assert!(
        deadline.left().is_zero(),
        "the wait starts with no time left"
    );
    let before = run.screen().contents.clone();
    assert!(
        !before.contains("marker"),
        "the checked grid holds no marker"
    );
    let shared = std::sync::Arc::clone(&run.shared);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run.wait_screen("the never-arriving needle", |grid| {
            assert!(
                !grid.contents.contains("marker"),
                "the checked grid is still stale"
            );
            // Publish the held marker to the grid after the check,
            // synchronously and with no wake: the panic below must
            // still show the checked grid.
            {
                let mut guard = shared.lock().unwrap();
                guard.sync_open = false;
                guard.publish(b"");
                assert!(
                    guard.grid.contents.contains("marker"),
                    "the fresh grid sees the marker after the check"
                );
            }
            grid.contents.contains("needle-never-arrives")
        })
    }));
    let message = result
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    let shown = message.split("screen:\n").nth(1).unwrap_or("");
    assert_eq!(
        shown, before,
        "the panic shows the checked grid: {message:?}"
    );
    assert!(
        !shown.contains("marker"),
        "the held output stays out of the panic: {message:?}"
    );
    assert!(
        message.contains("left when the wait began"),
        "the panic names the remaining deadline: {message:?}"
    );
    assert!(
        message.contains("the never-arriving needle"),
        "the panic names the wait: {message:?}"
    );
    let fresh = run.screen().contents.clone();
    assert!(
        fresh.contains("marker"),
        "a fresh read after the check sees the marker"
    );
}

/// A led wait puts the caller's lead first and leaves the wait's name,
/// the remaining deadline and the screen dump after it; the same wait
/// with no lead still opens with the wait's own text. Both waits
/// start on a fake clock advanced past the bound, so they expire at
/// once (`docs/testing.md`, "Waits and timeouts").
#[test]
fn wait_screen_leading_puts_the_lead_before_the_wait_and_the_screen() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let mut run = Run::attach(main, 80, 24, deadline);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAITS);
    let panic_text = |result: std::thread::Result<Grid>| {
        result
            .unwrap_err()
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default()
    };
    let led = panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || run.wait_screen_leading("the needle", |_| false, || "the lead; ".to_owned()),
    )));
    assert!(
        led.starts_with("the lead; waited until the deadline for the needle (deadline had "),
        "the lead comes first: {led:?}"
    );
    assert!(
        led.contains("left when the wait began); screen:\n"),
        "the deadline and the screen dump follow: {led:?}"
    );
    let plain = panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || run.wait_screen("the needle", |_| false),
    )));
    assert!(
        plain.starts_with("waited until the deadline for the needle (deadline had "),
        "a wait with no lead opens with its own text: {plain:?}"
    );
}

/// `shows_row` matches a whole trimmed row only: padding around the
/// reply text still matches, while a row with any additional text,
/// such as the working status line, does not. A substring check
/// would match the bare stem and the status line alike, so each
/// negative case below fails it.
#[test]
fn shows_row_matches_a_whole_trimmed_row_only() {
    use crate::support::journey::shows_row;
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process("   Working   \r\nWorking 3s · Esc to interrupt\r\n".as_bytes());
    let grid = Grid::of(&parser);
    assert!(shows_row(&grid, "Working"), "the padded reply row matches");
    assert!(
        !shows_row(&grid, "Work"),
        "a stem of the reply row does not match"
    );
    assert!(
        !shows_row(&grid, "Working!"),
        "a longer stem of the reply row does not match"
    );
    assert!(
        !shows_row(&grid, "Working 3s"),
        "a stem of the status line does not match"
    );
    let mut bare = vt100::Parser::new(24, 80, 0);
    bare.process("Working 3s · Esc to interrupt\r\n".as_bytes());
    let bare = Grid::of(&bare);
    assert!(
        !shows_row(&bare, "Working"),
        "the working status line alone does not satisfy the reply wait"
    );
    assert!(!shows_row(&grid, "idle"), "absent text does not match");
}

/// The workspace-holder cleanup wait draws on the cleanup phase,
/// not the journey's spent success-path budget: a fake clock
/// advanced past WAITS leaves the shared wait with nothing, while
/// the cleanup phase still holds CLEANUP minus WAITS. The extracted
/// wait forwards the cleanup-phase deadline it is given. A constant
/// check only: no pty run, no real-clock hold, no process wait, so
/// this test compiles into every main test binary that includes
/// this support module.
#[test]
fn workspace_cleanup_wait_uses_the_cleanup_phase() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAITS);
    assert!(deadline.left().is_zero(), "the shared wait budget is spent");
    let cleanup = deadline.cleanup_phase();
    assert_eq!(
        cleanup.left(),
        fakes::deadline::CLEANUP.saturating_sub(fakes::deadline::WAITS),
        "the cleanup wait keeps its own budget after WAITS is spent"
    );
    wait_for_workspace_holders("fiber-cleanup-absent", cleanup, |_, received| {
        assert_eq!(
            received.left(),
            fakes::deadline::CLEANUP.saturating_sub(fakes::deadline::WAITS),
            "the wait receives the full cleanup budget"
        );
        Ok(())
    });
}

use contract::clock::Clock as _;

/// The terminal side reading without blocking: raw mode with no
/// minimum and no timeout, so a read returns the bytes already in the
/// pty buffer at once.
fn make_raw(terminal: &fs::File) {
    let mut attrs = rustix::termios::tcgetattr(terminal).unwrap();
    attrs.make_raw();
    attrs.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 0;
    attrs.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 0;
    rustix::termios::tcsetattr(terminal, rustix::termios::OptionalActions::Now, &attrs).unwrap();
}

/// Every byte waiting on the terminal side: the click reports the
/// helper wrote, already in the pty buffer.
fn drain_terminal(mut terminal: &fs::File) -> Vec<u8> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match terminal.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("reading the terminal side: {error}"),
        }
    }
    bytes
}

/// A full frame holding `text` at each 0-based row in `rows`: a clear
/// followed by each row's positioned write.
fn draw(rows: &[(u16, &str)]) -> Vec<u8> {
    let mut bytes = b"\x1b[H\x1b[J".to_vec();
    for (y, text) in rows {
        bytes.extend_from_slice(format!("\x1b[{};1H{text}", y + 1).as_bytes());
    }
    bytes
}

/// A caught helper outcome as the channel carries it: `Ok` when the
/// helper dispatched, the panic message when it did not.
fn panic_text(outcome: std::thread::Result<()>) -> Result<(), String> {
    match outcome {
        Ok(()) => Ok(()),
        Err(payload) => Err(payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|held| (*held).to_owned())
            })
            .unwrap_or_else(|| "a non-string panic".to_owned())),
    }
}

/// Parks the calling thread on the fake clock at the helper's own
/// bound instant, then blocks on the test's release: the helper
/// waiting on its clock past its own check, not a parked caller
/// alone. The test advances by `advance_when_parked` naming the
/// reported thread and instant, then releases the hook.
fn park_at(
    clock: &fakes::clock::FakeClock,
    wait_left: Duration,
    report: &mpsc::Sender<(thread::ThreadId, std::time::Instant)>,
    release: &mpsc::Receiver<()>,
) {
    let expected = clock
        .now()
        .checked_add(wait_left)
        .expect("the park instant");
    clock.wait_until(Some(expected), &mut |_| {
        report
            .send((thread::current().id(), expected))
            .unwrap_or(());
        Deadline::after(fakes::deadline::WAIT)
            .recv(release)
            .expect("the test releases the parked hook");
    });
}

/// The acceptance proof: coordinates found in frame A are stale once a
/// row is inserted above the target, so the old pattern clicks the
/// wrong row while the helper re-finds and clicks the right one. A
/// `same_frame` that always compares true sends the stale row; one
/// that always compares false never dispatches and fails with the
/// helper's named deadline panic.
#[test]
fn a_click_found_before_a_row_is_inserted_lands_on_the_target_row() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let thread_clock = (*clock).clone();
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut run = Run::attach(main, 80, 24, deadline);
    let frame_a = draw(&[(3, "decoy"), (4, "target")]);
    let frame_b = draw(&[(4, "decoy"), (5, "target")]);
    run.shared.lock().unwrap().publish(&frame_a);
    let stale = run
        .screen()
        .text_at("target")
        .expect("frame A holds the target");
    assert_eq!(stale, (0, 4), "the control finds the target in frame A");
    run.shared.lock().unwrap().publish(&frame_b);
    assert!(
        run.screen().rows[4].contains("decoy"),
        "frame B inserted a row above the target"
    );
    let target = run
        .screen()
        .text_at("target")
        .expect("frame B holds the target");
    assert_ne!(
        click_bytes(stale),
        click_bytes(target),
        "the stale coordinates would click the wrong row"
    );
    run.shared.lock().unwrap().publish(&frame_a);
    assert_eq!(
        run.screen().text_at("target"),
        Some((0, 4)),
        "the reset redraws frame A"
    );
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let (report_tx, report_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let finds = Arc::new(Mutex::new(0usize));
    let founds = Arc::new(Mutex::new(0usize));
    let shared = Arc::clone(&run.shared);
    let thread_finds = Arc::clone(&finds);
    let thread_founds = Arc::clone(&founds);
    let terminal_read = terminal_side.try_clone().unwrap();
    thread::Builder::new()
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run.mouse_where_paused(
                    "the target row",
                    |grid| {
                        *thread_finds.lock().unwrap() += 1;
                        grid.text_at("target")
                    },
                    click_bytes,
                    |pause| match pause {
                        Pause::Found { wait_left } => {
                            let mut found = thread_founds.lock().unwrap();
                            *found += 1;
                            match *found {
                                1 => shared.lock().unwrap().publish(&frame_b),
                                2 => park_at(&thread_clock, wait_left, &report_tx, &release_rx),
                                n => panic!("unexpected Found #{n}: one retry only"),
                            }
                        }
                        Pause::Waiting { .. } | Pause::Dispatched { .. } => {}
                    },
                );
            }));
            // Read while the run is alive: dropping it closes the
            // master, and a slave read past that fails.
            let clicked = drain_terminal(&terminal_read);
            outcome_tx
                .send((panic_text(outcome), clicked))
                .unwrap_or(());
        })
        .unwrap();
    // A helper that returns without parking never reports: skip the
    // advance and the release, and let the asserts below tell the
    // stale click from the fresh one.
    let (outcome, clicked) = match deadline.recv(&report_rx) {
        Ok((thread, expected)) => {
            clock.advance_when_parked(
                Some(expected),
                Some(thread),
                fakes::deadline::WAIT,
                &deadline,
            );
            let done = Deadline::after(Duration::from_secs(80)); // ruling: #3979
            release_tx.send(()).unwrap_or(());
            done.recv(&outcome_rx).expect("the helper thread's result")
        }
        Err(_) => {
            let done = Deadline::after(Duration::from_secs(80)); // ruling: #3979
            done.recv(&outcome_rx).expect("the helper thread's result")
        }
    };
    outcome.expect("the helper dispatches at the fresh row");
    assert_eq!(
        clicked,
        click_bytes(target),
        "the helper clicks the target's row in frame B"
    );
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(
        *finds.lock().unwrap(),
        2,
        "the changed attempt and the settled retry each find"
    );
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(
        *founds.lock().unwrap(),
        2,
        "the changed attempt and the settled retry each pause"
    );
}

/// A style-only change between the find and the click is a change: the
/// same text with a different background re-finds once. A frame
/// comparison over row text only never retries here. An always-retry
/// mutant reaches the third pause and fails with its message, since
/// the clock never moves and the wait never expires.
#[test]
fn a_style_only_change_between_find_and_click_is_a_change() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut run = Run::attach(main, 80, 24, deadline);
    run.shared.lock().unwrap().publish(&draw(&[(4, "target")]));
    let shared = Arc::clone(&run.shared);
    let mut finds = 0usize;
    let mut founds = 0usize;
    let mut dispatched = 0usize;
    run.mouse_where_paused(
        "the styled target",
        |grid| {
            finds += 1;
            grid.text_at("target")
        },
        click_bytes,
        |pause| match pause {
            Pause::Found { .. } => {
                founds += 1;
                match founds {
                    1 => shared
                        .lock()
                        .unwrap()
                        .publish(b"\x1b[5;1H\x1b[44mtarget\x1b[m"),
                    2 => {}
                    n => panic!("unexpected Found #{n}: only one retry"),
                }
            }
            Pause::Dispatched { .. } => dispatched += 1,
            Pause::Waiting { .. } => {
                panic!("the styled target waits for nothing");
            }
        },
    );
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(finds, 2, "the changed attempt and the retry each find");
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(founds, 2, "the changed attempt and the retry each pause");
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(dispatched, 1, "the retry dispatches once");
    assert_eq!(
        drain_terminal(&terminal_side),
        click_bytes((0, 4)),
        "the retry clicks the found cell"
    );
}

/// An unchanged frame clicks once without a retry: one find, one
/// pause, one dispatch. An always-retry mutant reaches the second
/// pause and fails with its message, since the clock never moves and
/// the wait never expires.
#[test]
fn an_unchanged_frame_clicks_once_without_a_retry() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut run = Run::attach(main, 80, 24, deadline);
    run.shared.lock().unwrap().publish(&draw(&[(4, "target")]));
    let mut finds = 0usize;
    let mut founds = 0usize;
    let mut dispatched = 0usize;
    run.mouse_where_paused(
        "the settled target",
        |grid| {
            finds += 1;
            grid.text_at("target")
        },
        click_bytes,
        |pause| match pause {
            Pause::Found { .. } => {
                founds += 1;
                if founds > 1 {
                    panic!("unexpected second Found: unchanged frame must click once");
                }
            }
            Pause::Dispatched { .. } => dispatched += 1,
            Pause::Waiting { .. } => {
                panic!("the settled target waits for nothing");
            }
        },
    );
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(finds, 1, "the settled frame finds once");
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(founds, 1, "the settled frame pauses once");
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(dispatched, 1, "the settled frame dispatches once");
    assert_eq!(
        drain_terminal(&terminal_side),
        click_bytes((0, 4)),
        "the click lands on the found cell"
    );
}

/// An absent target panics naming the wait and the last grid: the hook
/// parks the helper thread at its own bound instant, the test spends
/// the wait through the parked advance, and the helper's receive is
/// handed zero. The text finder returns the first row-major match at
/// terminal-cell columns, skipping a wide character's continuation
/// cell and keeping a combining mark in its base cell.
#[test]
fn an_absent_target_panics_naming_the_wait_and_the_last_grid() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let thread_clock = (*clock).clone();
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut run = Run::attach(main, 80, 24, deadline);
    run.shared
        .lock()
        .unwrap()
        .publish(&draw(&[(2, "something else")]));
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let (report_tx, report_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::Builder::new()
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run.mouse_where_paused(
                    "the missing target",
                    |_| None::<(u16, u16)>,
                    click_bytes,
                    |pause| match pause {
                        Pause::Waiting { wait_left } => {
                            park_at(&thread_clock, wait_left, &report_tx, &release_rx);
                        }
                        Pause::Found { .. } | Pause::Dispatched { .. } => {
                            panic!("the absent target is never found");
                        }
                    },
                );
            }));
            outcome_tx.send(panic_text(outcome)).unwrap_or(());
        })
        .unwrap();
    let (thread, expected) = deadline
        .recv(&report_rx)
        .expect("the helper thread reports its check");
    clock.advance_when_parked(
        Some(expected),
        Some(thread),
        fakes::deadline::WAIT,
        &deadline,
    );
    let done = Deadline::after(Duration::from_secs(80)); // ruling: #3979
    release_tx.send(()).unwrap_or(());
    let message = done
        .recv(&outcome_rx)
        .expect("the helper thread's result")
        .expect_err("the absent target panics");
    assert!(
        message.starts_with("waited until the deadline for the missing target"),
        "the panic names the wait: {message:?}"
    );
    assert!(
        message.contains("something else"),
        "the panic shows the last grid: {message:?}"
    );
    let probe_terminal = open(80, 24);
    let probe_main = fs::File::from(probe_terminal.main);
    let probe = Run::attach(probe_main, 80, 24, deadline);
    probe.shared.lock().unwrap().publish(&draw(&[
        (0, "target here"),
        (1, "target there"),
        (2, "界 target"),
        (3, "é target"),
    ]));
    let grid = probe.screen();
    assert_eq!(
        grid.text_at("target"),
        Some((0, 0)),
        "the finder returns the first row-major match"
    );
    assert_eq!(
        grid.text_at("there"),
        Some((7, 1)),
        "the finder searches every row"
    );
    assert_eq!(
        grid.text_at("here"),
        Some((7, 0)),
        "the match offset maps back to a column"
    );
    let wide = draw(&[(0, "界 target")]);
    probe.shared.lock().unwrap().publish(&wide);
    assert_eq!(
        probe.screen().text_at("target"),
        Some((3, 0)),
        "a wide character's continuation cell holds no column"
    );
    let combining = draw(&[(0, "é target")]);
    probe.shared.lock().unwrap().publish(&combining);
    assert_eq!(
        probe.screen().text_at("target"),
        Some((2, 0)),
        "a combining mark stays in its base cell"
    );
    assert_eq!(probe.screen().text_at("absent"), None, "no match is none");
}

/// The click is written while the frame lock is held: the dispatch
/// reports the lock crossed the write. Dropping `Shared` before the
/// write reports otherwise. An always-retry mutant reaches the second
/// pause and fails with its message, since the clock never moves and
/// the wait never expires.
#[test]
fn the_click_is_written_while_the_frame_lock_is_held() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut run = Run::attach(main, 80, 24, deadline);
    run.shared.lock().unwrap().publish(&draw(&[(4, "target")]));
    let mut founds = 0usize;
    let mut held = None;
    run.mouse_where_paused(
        "the locked target",
        |grid| grid.text_at("target"),
        click_bytes,
        |pause| match pause {
            Pause::Found { .. } => {
                founds += 1;
                if founds > 1 {
                    panic!("unexpected second Found: single dispatch must not retry");
                }
            }
            Pause::Dispatched { frame_lock_held } => held = Some(frame_lock_held),
            Pause::Waiting { .. } => {
                panic!("the locked target waits for nothing");
            }
        },
    );
    assert_eq!(held, Some(true), "the frame lock crosses the write");
    assert_eq!(
        drain_terminal(&terminal_side),
        click_bytes((0, 4)),
        "the click lands on the found cell"
    );
}

/// A panicking encoder leaves both locks usable: the encode runs with
/// no lock held, so its panic poisons neither mutex and writes
/// nothing. Encoding under `Shared` poisons it on this path. Any
/// mutant that retries instead of propagating the panic reaches the
/// second pause and fails with its message, since the clock never
/// moves and the wait never expires.
#[test]
fn a_panicking_encoder_leaves_both_locks_usable() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut run = Run::attach(main, 80, 24, deadline);
    run.shared.lock().unwrap().publish(&draw(&[(4, "target")]));
    let mut founds = 0usize;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run.mouse_where_paused(
            "the encoding target",
            |grid| grid.text_at("target"),
            |_| panic!("encoder"),
            |pause| match pause {
                Pause::Found { .. } => {
                    founds += 1;
                    if founds > 1 {
                        panic!("unexpected second Found: encoder must panic first");
                    }
                }
                Pause::Waiting { .. } | Pause::Dispatched { .. } => {
                    panic!("the encoder panics before any wait or dispatch");
                }
            },
        );
    }));
    let message = panic_text(outcome).expect_err("the encoder panics");
    assert!(
        message.contains("encoder"),
        "the encoder panic propagates: {message:?}"
    );
    assert!(run.shared.lock().is_ok(), "the frame lock stays usable");
    assert!(run.writer().lock().is_ok(), "the writer lock stays usable");
    assert!(
        drain_terminal(&terminal_side).is_empty(),
        "a panicking encode writes nothing"
    );
}

/// A held writer sends nothing even after it is released: the dispatch
/// takes `Writer` only by `try_lock` under `Shared`, so a held writer
/// panics having written nothing, and registers no worker. A dispatch
/// handing its bytes to a worker would register it instead: the thread
/// receives each registered send through the named `ack` deadline and
/// joins, and any byte on the wire fails the empty read. Deleting the
/// `try_lock` error arm's panic returns normally and fails the held
/// message assert.
#[test]
fn a_held_writer_sends_nothing_even_after_it_is_released() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let thread_clock = (*clock).clone();
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let terminal_read = terminal_side.try_clone().unwrap();
    let mut run = Run::attach(main, 80, 24, deadline);
    let frame_a = draw(&[(4, "target")]);
    let frame_b = draw(&[(5, "target")]);
    run.shared.lock().unwrap().publish(&frame_a);
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let (report_tx, report_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let helper = thread::Builder::new()
        .spawn(move || {
            let writer = run.writer();
            let held = writer.lock().unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run.mouse_where_paused(
                    "the held target",
                    |grid| grid.text_at("target"),
                    click_bytes,
                    |pause| match pause {
                        Pause::Found { wait_left } => {
                            park_at(&thread_clock, wait_left, &report_tx, &release_rx);
                        }
                        Pause::Waiting { .. } | Pause::Dispatched { .. } => {
                            panic!("the held writer neither waits nor dispatches");
                        }
                    },
                );
            }));
            drop(held);
            run.shared.lock().unwrap().publish(&frame_b);
            let ack = deadline.start_wait();
            let taken = std::mem::take(&mut *run.pending_writes.lock().unwrap());
            let registered = taken.len();
            for (handle, finished) in taken {
                ack.recv(&finished)
                    .expect("waited until the deadline for the registered worker's write")
                    .expect("the registered worker's write succeeds");
                handle.join().expect("the registered worker joins");
            }
            let read = drain_terminal(&terminal_read);
            outcome_tx
                .send((run, panic_text(outcome), registered, read))
                .unwrap_or(());
        })
        .unwrap();
    let (thread, expected) = deadline
        .recv(&report_rx)
        .expect("the helper thread reports its check");
    clock.advance_when_parked(
        Some(expected),
        Some(thread),
        fakes::deadline::WAIT,
        &deadline,
    );
    let done = Deadline::after(Duration::from_secs(80)); // ruling: #3979
    release_tx.send(()).unwrap_or(());
    let (run, outcome, registered, read) =
        done.recv(&outcome_rx).expect("the helper thread's result");
    helper.join().expect("the helper thread joins");
    assert!(read.is_empty(), "a held writer sends nothing");
    assert_eq!(registered, 0, "the helper starts no worker");
    let message = outcome.expect_err("the held writer panics");
    assert!(
        message.contains("terminal writer is held"),
        "the panic names the held writer: {message:?}"
    );
    assert!(run.shared.lock().is_ok(), "the frame lock stays usable");
    assert!(run.writer().lock().is_ok(), "the writer lock stays usable");
}

/// An open synchronized frame waits for its close, then re-finds: the
/// first find sees frame A while newer output sits behind the open
/// block, so the helper waits for the close and dispatches at the
/// target's row in frame B. A dispatch comparing the grid without
/// waiting for the close clicks the decoy's row in B.
#[test]
fn an_open_synchronized_frame_waits_for_its_close_then_refinds() {
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    let terminal = open(80, 24);
    let main = fs::File::from(terminal.main);
    let terminal_side = terminal.terminal;
    make_raw(&terminal_side);
    let mut terminal_writer = terminal_side.try_clone().unwrap();
    let done = Deadline::after(Duration::from_secs(80)); // ruling: #3979
    let mut run = Run::attach(main, 80, 24, deadline);
    let frame_a = draw(&[(3, "decoy"), (4, "target")]);
    let frame_b = draw(&[(4, "decoy"), (5, "target")]);
    run.shared.lock().unwrap().publish(&frame_a);
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let finds = Arc::new(Mutex::new(0usize));
    let founds = Arc::new(Mutex::new(0usize));
    let shared = Arc::clone(&run.shared);
    let thread_finds = Arc::clone(&finds);
    let thread_founds = Arc::clone(&founds);
    let terminal_read = terminal_side.try_clone().unwrap();
    thread::Builder::new()
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run.mouse_where_paused(
                    "the synced target",
                    |grid| {
                        *thread_finds.lock().unwrap() += 1;
                        grid.text_at("target")
                    },
                    click_bytes,
                    |pause| match pause {
                        Pause::Found { .. } => {
                            let mut found = thread_founds.lock().unwrap();
                            *found += 1;
                            if *found == 1 {
                                let mut redraw = SYNC_BEGIN.to_vec();
                                redraw.extend_from_slice(&frame_b);
                                shared.lock().unwrap().publish(&redraw);
                            }
                        }
                        Pause::Waiting { .. } => {
                            use std::io::Write as _;
                            terminal_writer.write_all(SYNC_END).unwrap();
                        }
                        Pause::Dispatched { .. } => {}
                    },
                );
            }));
            // Read while the run is alive: dropping it closes the
            // master, and a slave read past that fails.
            let clicked = drain_terminal(&terminal_read);
            outcome_tx
                .send((panic_text(outcome), clicked))
                .unwrap_or(());
        })
        .unwrap();
    let (outcome, clicked) = done.recv(&outcome_rx).expect("the helper thread's result");
    outcome.expect("the helper dispatches after the close");
    assert_eq!(
        clicked,
        click_bytes((0, 5)),
        "the helper clicks the target's row in frame B"
    );
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(
        *finds.lock().unwrap(),
        2,
        "the open-block attempt and the fresh attempt each find"
    );
    // docs/testing/screens.md, "Screens": the re-find runs under the call's one wait.
    assert_eq!(
        *founds.lock().unwrap(),
        2,
        "the open-block attempt and the fresh attempt each pause"
    );
}

use fakes::MUST_SUCCEED_WITHIN;
use std::path::Path;

use ::support::clock::Sleep;
use contract::HubEventKind;
use fakes::clock::{FakeClock, Miss};

use crate::hub_start::{
    CAPTURE_CHARS, connect_attempt, give_up, hello_bound, hub_threads, is_hello_timeout, report,
    serve_installed, slices_shown, truncate_section,
};
use crate::support::{CapturedStderr, Deadline, HubProc, Setup};

#[test]
fn slices_shown_collapses_runs_and_keeps_singles() {
    assert_eq!(
        slices_shown(&[
            Duration::from_secs(10),
            Duration::from_secs(10),
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        ]),
        "[10s x2, 0ns x3]"
    );
    assert_eq!(slices_shown(&[]), "[]");
}

#[test]
fn hello_timeout_names_the_bound_the_slice_died_with() {
    assert!(is_hello_timeout("waited 0ns for the installed hub's hello"));
    assert!(is_hello_timeout(
        "waited 39.75s for the installed hub's hello"
    ));
}

#[test]
fn hello_timeout_rejects_panics_from_inside_the_work() {
    assert!(!is_hello_timeout(
        "the wait for the installed hub's hello panicked: boom"
    ));
    assert!(!is_hello_timeout(
        "could not start a thread for the installed hub's hello: boom"
    ));
}

#[test]
fn hello_bound_reserves_action_and_report_margin() {
    assert_eq!(
        hello_bound(Duration::from_secs(30)),
        Some(Duration::from_secs(10))
    );
    assert_eq!(
        hello_bound(doors::hub::CONNECT_DEADLINE + Duration::from_millis(250)),
        None
    );
    assert_eq!(
        hello_bound(
            doors::hub::CONNECT_DEADLINE + Duration::from_millis(250) + Duration::from_secs(3)
        ),
        Some(Duration::from_secs(3))
    );
}

/// A fake-clock action that consumes nearly the full connect race does
/// not consume the hello receive's bound, which starts when the socket
/// accepts. The worker is named and the clock advances only at its
/// observed parks.
#[test]
fn connect_attempt_starts_hello_bound_after_the_action() {
    let setup = Setup::new();
    let home = setup.home().to_path_buf();
    fs::create_dir_all(home.join("run")).unwrap();
    let clock = FakeClock::with_sleep(Sleep::Waits);
    let origin = clock.origin();
    let hello_within = Duration::from_secs(10);
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let thread_clock = Arc::clone(&clock);
    let thread_home = home.clone();
    let worker = thread::Builder::new()
        .name("hub-start-hello-bound".to_owned())
        .spawn(move || {
            id_tx.send(thread::current().id()).unwrap();
            let result = connect_attempt(
                &thread_home,
                &mut || Ok(()),
                thread_clock.as_ref(),
                hello_within,
            );
            done_tx.send(result).unwrap();
        })
        .unwrap();
    let worker_id = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&id_rx)
        .expect("the connector reports its worker id");
    let poll = Duration::from_millis(10);
    let action_deadline = Deadline::after(Duration::from_secs(30));
    let mut next_poll = origin + poll;
    for _ in 0..497 {
        clock.advance_when_parked(Some(next_poll), Some(worker_id), poll, &action_deadline);
        next_poll += poll;
    }
    assert!(
        clock
            .mark_parked(next_poll, Deadline::after(MUST_SUCCEED_WITHIN))
            .is_some(),
        "the connector parks just before the race deadline"
    );

    let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = Arc::new(listener);
    let accepted = Arc::new(Mutex::new(None));
    let poll_listener = Arc::clone(&listener);
    let poll_accepted = Arc::clone(&accepted);
    clock.advance_when_parked(Some(next_poll), Some(worker_id), poll, &action_deadline);
    fakes::wait_until(
        "the connector to reach the hub listener",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || match poll_listener.accept() {
            Ok((stream, _)) => {
                *poll_accepted.lock().unwrap() = Some(stream);
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("accepting the connector: {error}"),
        },
    );
    // The accept happened at the fake clock's current instant. This is
    // the receive deadline expected from connect_within.
    let accepted_at = clock.now();
    let expected = accepted_at + hello_within;
    let observed = clock.try_advance_when_parked(
        Some(expected),
        Some(worker_id),
        Duration::from_secs(6),
        MUST_SUCCEED_WITHIN,
    );
    let (matched, actual) = match observed {
        Ok(_) => (true, None),
        Err(Miss::Mismatch {
            watched: (_, Some(until)),
            ..
        }) => (false, Some(until)),
        Err(Miss::Mismatch { .. } | Miss::NoPark) => (false, None),
    };
    if !matched {
        // Let the old shared deadline expire so the worker and socket
        // are cleaned up before asserting the regression failure.
        let until = actual.unwrap_or(expected);
        clock.advance_when_parked(
            Some(until),
            Some(worker_id),
            until.saturating_duration_since(clock.now()),
            &Deadline::after(MUST_SUCCEED_WITHIN),
        );
    }
    let mut server = accepted.lock().unwrap().take().unwrap();
    if matched {
        server
                .write_all(
                    br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#,
                )
                .unwrap();
        server.write_all(b"\n").unwrap();
    }
    drop(server);
    let result =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done_rx, "the connector's hello result");
    worker.join().unwrap();
    assert!(matched, "hello parks at {expected:?}, found {actual:?}");
    let (_, line) = result.expect("the connected hub answers its hello");
    assert_eq!(line.kind(), HubEventKind::HubHello);
}

/// A missing `hub.log` reports missing, while an unreadable one
/// reports the error instead of misdiagnosing it as missing: a
/// directory at the log path fails the read with a non-`NotFound`
/// error on every platform.
#[test]
fn report_distinguishes_missing_from_unreadable_hub_log() {
    let setup = Setup::new();
    let missing = report(&setup, None, "", "");
    assert!(
        missing.contains("hub.log missing"),
        "a fresh home holds no hub.log: {missing}"
    );
    assert!(
        !missing.contains("unreadable"),
        "absence is not an error: {missing}"
    );
    fs::create_dir_all(setup.home().join("logs").join("hub.log")).unwrap();
    let unreadable = report(&setup, None, "", "");
    assert!(
        unreadable.contains("hub.log unreadable"),
        "a directory at the log path is not missing: {unreadable}"
    );
    assert!(
        !unreadable.contains("hub.log missing"),
        "an error reports distinctly from absence: {unreadable}"
    );
}

/// A hub that exits before saying hello reports root and per-attempt
/// hello bounds alongside the exit, without a shared started-wait.
#[test]
fn serve_installed_retry_exhausted_reports_wait_accounting() {
    let setup = Setup::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serve_installed(&setup, 1, &mut || 1, &mut |_| Command::new("true"))
    }));
    let message = outcome
        .err()
        .and_then(|payload| payload.downcast::<String>().ok().map(|boxed| *boxed))
        .expect("the exited hub exhausts its one attempt");
    assert!(
        message.contains("hello wait entered"),
        "the panic names the hello wait's entry bounds: {message}"
    );
    assert!(
        message.contains("helper root left"),
        "the panic names the helper root bound at entry: {message}"
    );
    assert!(
        message.contains("failing root left"),
        "the panic names the root bound at the failure: {message}"
    );
    assert!(
        message.contains("hello attempts+"),
        "the panic names the per-attempt hello bounds: {message}"
    );
    assert!(
        message.contains("exited"),
        "the panic names the hub's exit: {message}"
    );
}

#[test]
fn hello_timeout_rejects_the_guard_text() {
    assert!(!is_hello_timeout("waited for the installed hub's hello"));
    assert!(!is_hello_timeout(
        "waited for the installed hub's hello (hello wait entered with helper root left 40s)"
    ));
    assert!(!is_hello_timeout("waited  for the installed hub's hello"));
    assert!(!is_hello_timeout(
        "waited a while for the installed hub's hello"
    ));
}

#[test]
fn truncate_section_marks_only_a_cut() {
    let below: String = "x".repeat(CAPTURE_CHARS - 1);
    assert_eq!(truncate_section(&below), below);
    let at: String = "y".repeat(CAPTURE_CHARS);
    assert_eq!(truncate_section(&at), at);
    let above: String = "z".repeat(CAPTURE_CHARS + 1);
    assert_eq!(
        truncate_section(&above),
        format!("{}...", "z".repeat(CAPTURE_CHARS))
    );
    let wide: String = "é".repeat(CAPTURE_CHARS + 1);
    let cut = truncate_section(&wide);
    assert_eq!(cut.chars().count(), CAPTURE_CHARS + 3);
    assert!(cut.ends_with("..."), "a cut ends in its mark");
}

/// Spawns a stub child in its own process group that parks opening
/// `fifo` for write: the open blocks until a reader arrives, and no
/// reader arrives during the probe, so the child stays hung regardless
/// of thread scheduling.
fn spawn_fifo_stub(deadline: Deadline, fifo: &Path) -> HubProc {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("exec > \"$1\"")
        .arg("fifo-stub")
        .arg(fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    HubProc::spawn_command(deadline, &mut command)
}

/// Reads the `give_up` panic's text: only the enriched panic can follow
/// it, so its text also proves the live child was reaped.
fn give_up_text(setup: &Setup, process: HubProc, capture: &CapturedStderr, why: &str) -> String {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        give_up(setup, process, capture, why);
    }));
    outcome
        .err()
        .and_then(|payload| payload.downcast::<String>().ok().map(|boxed| *boxed))
        .expect("give_up panics")
}

/// The enriched give-up names every capture section: the caller's entry
/// line, the live child's state, the hub.log state, the stderr
/// snapshot, the resolved directories, and the live thread state of the
/// FIFO-hung stub. The probe drives no hello_slice bound: the ticket's
/// subject is the capture report, so it tests the reporter, not the
/// scheduling that reaches it. The kill in `give_up` reaps the stub
/// before the panic, and the home's TempDir removes the FIFO, so a
/// failing probe never wedges later tests.
#[test]
fn give_up_reports_stderr_dirs_and_thread_state() {
    let setup = Setup::new();
    let fifo = setup.home().join("stub.fifo");
    fakes::fifo(&fifo);
    let process = spawn_fifo_stub(setup.deadline, &fifo);
    let pid = process.child.id();
    let capture = CapturedStderr::drained_text("hub stderr line 1\nhub stderr line 2\n".to_owned());
    let message = give_up_text(
        &setup,
        process,
        &capture,
        "waited for the installed hub's hello (test)",
    );
    assert!(
        message.contains("waited for the installed hub's hello (test)"),
        "the panic keeps the caller's entry line: {message}"
    );
    assert!(
        message.contains("was still running"),
        "the panic names the live child's state: {message}"
    );
    assert!(
        message.contains("hub.log missing"),
        "a fresh home holds no hub.log: {message}"
    );
    assert!(
        message.contains("hub stderr (live snapshot, last 20 lines):"),
        "the panic carries the stderr section: {message}"
    );
    assert!(
        message.contains("hub stderr line 2"),
        "the panic carries the snapshot's tail: {message}"
    );
    assert!(
        message.contains(&format!("state dir {}", setup.home().join("run").display())),
        "the panic names the resolved state directory: {message}"
    );
    assert!(
        message.contains(&format!("log dir {}", setup.home().join("logs").display())),
        "the panic names the resolved log directory: {message}"
    );
    assert!(
        message.contains(&format!("pid {pid}")),
        "the panic names the captured pid: {message}"
    );
    #[cfg(target_os = "linux")]
    assert!(
        message.contains("wchan"),
        "the panic names the stub's park state: {message}"
    );
    fs::remove_file(&fifo).unwrap_or(());
}

/// An empty snapshot on a running hub reports that nothing was captured
/// yet, never as a clean drain; an empty hub.log reports distinctly
/// from a missing one.
#[test]
fn give_up_marks_empty_stderr_and_empty_log() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("logs")).unwrap();
    fs::write(setup.home().join("logs").join("hub.log"), "").unwrap();
    let fifo = setup.home().join("stub.fifo");
    fakes::fifo(&fifo);
    let process = spawn_fifo_stub(setup.deadline, &fifo);
    let capture = CapturedStderr::drained_text(String::new());
    let message = give_up_text(
        &setup,
        process,
        &capture,
        "waited for the installed hub's hello (test)",
    );
    assert!(
        message.contains("(still running: no stderr captured yet)"),
        "an empty snapshot on a running hub is not a clean drain: {message}"
    );
    assert!(
        message.contains("hub.log empty (0 bytes)"),
        "an empty hub.log reports distinctly from a missing one: {message}"
    );
    fs::remove_file(&fifo).unwrap_or(());
}

/// A long valid home truncates with the cut mark instead of pushing
/// the report's head out; a short one passes through whole.
#[test]
fn dirs_section_truncates_a_long_home() {
    let long = Path::new("/h").join("x".repeat(CAPTURE_CHARS + 1));
    let cut = crate::hub_start::dirs_section(&long);
    assert!(
        cut.starts_with("state dir "),
        "the truncation keeps the head: {cut}"
    );
    assert!(cut.ends_with("..."), "a cut ends in its mark");
    assert_eq!(
        crate::hub_start::dirs_section(Path::new("/h")),
        "state dir /h/run, log dir /h/logs"
    );
}

/// A pid with no task directory reads as its own marker, never as
/// empty output and never as a panic.
#[test]
fn hub_threads_names_a_gone_pid() {
    let pid = u32::MAX;
    let threads = hub_threads(pid);
    assert!(
        threads.contains(&format!("pid {pid}")),
        "the capture names the pid it read: {threads}"
    );
    #[cfg(target_os = "linux")]
    assert!(
        threads.contains("pid gone"),
        "a vanished pid prints its marker as text: {threads}"
    );
}

/// An exited but unreaped child has no live threads: the capture names
/// the zombie as text, and the give-up still reports the exit with an
/// empty snapshot as nothing captured, never as a clean drain.
#[cfg(target_os = "linux")]
#[test]
fn hub_threads_names_a_zombie_as_text() {
    let setup = Setup::new();
    let mut command = Command::new("true");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let process = HubProc::spawn_command(setup.deadline, &mut command);
    let pid = process.child.id();
    // A reap-free exit probe: `waitid` with `WNOWAIT` reports the exit
    // while leaving the child a zombie for `hub_threads` to name, which
    // `try_wait` itself would reap away. Blocking code runs on a thread
    // and the test receives under its own bound, which fails naming the
    // wait instead of hanging the runner.
    let (exited_tx, exited_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let waited = rustix::process::waitid(
                rustix::process::WaitId::Pid(rustix::process::Pid::from_child(&process.child)),
                rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT,
            );
            exited_tx.send(waited.map(|_| ())).unwrap_or(());
        });
        // The scope joins the probe afterwards; the stub exits at once,
        // so even the failure path joins promptly instead of hanging.
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&exited_rx, "the true stub to exit")
            .unwrap_or_else(|error| panic!("reap-free wait for the true stub: {error}"));
    });
    let threads = hub_threads(pid);
    assert!(
        threads.contains("no live threads"),
        "an exited unreaped child names its zombie state: {threads}"
    );
    let capture = CapturedStderr::drained_text(String::new());
    let message = give_up_text(&setup, process, &capture, "waited (test)");
    assert!(
        message.contains("exited"),
        "the panic names the hub's exit: {message}"
    );
    assert!(
        message.contains("(no stderr captured)"),
        "an empty snapshot on an exited hub is not a clean drain: {message}"
    );
}

/// A `session_status` has no position against the session's other lines
/// (`docs/testing.md`, "Event streams"), so it may arrive before the
/// subscribe acknowledgement:
/// `close_session` matches the acknowledgement by its command id.
#[test]
fn close_session_reads_its_acknowledgement_by_command_id() {
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let (mine, peer) = UnixStream::pair().unwrap();
    let (sent_tx, sent) = mpsc::channel();
    thread::spawn(move || {
        let mut peer = peer;
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let subscribe = read_line(&mut reader, deadline, "the subscribe line")
            .unwrap()
            .unwrap();
        sent_tx.send(subscribe).unwrap_or(());
        for line in [
            "{\"kind\":\"session_status\",\"payload\":{\"state\":\"idle\"}}\n",
            "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_close_sub\"}}\n",
        ] {
            write_line(&mut peer, deadline, line.as_bytes(), "the fixed lines").unwrap();
        }
        if let Ok(Some(close)) = read_line(&mut reader, deadline, "the close line") {
            sent_tx.send(close).unwrap_or(());
        }
    });
    let client = Socket::from(deadline, mine);
    close_session(&client);
    let ids: Vec<String> = [(); 2]
        .iter()
        .map(|()| {
            let line = deadline.recv_or_fail(&sent, "a line the peer read");
            serde_json::from_str::<Value>(&line).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(ids, ["c_close_sub", "c_close"]);
}

/// `recv_answer` returns the line for its command id, keeps the lines it
/// passed for the next `recv` in order, and drops `attention` lines.
#[test]
fn recv_answer_skips_a_leading_status_and_keeps_it_for_the_next_recv() {
    let client = replay_peer(vec![
        "{\"kind\":\"attention\",\"payload\":{}}\n",
        "{\"kind\":\"session_status\",\"payload\":{\"state\":\"idle\"}}\n",
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_other\"}}\n",
        "{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_want\"}}\n",
    ]);
    let answer = recv_answer(&client, "c_want", "the wanted answer");
    assert_eq!(answer["payload"]["command_id"], "c_want");
    let status = recv(&client, "the kept status");
    assert_eq!(status["kind"], "session_status");
    let other = recv(&client, "the kept other answer");
    assert_eq!(other["payload"]["command_id"], "c_other");
}
