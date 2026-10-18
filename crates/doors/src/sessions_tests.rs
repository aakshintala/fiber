//! Socket discovery, summary protocol and the one injected-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions and fixtures"
)]

use super::*;
use contract::events::{SessionState, SessionStatus};
use contract::shapes::{Tokens, Usage};
use fakes::CancelToken;
use fakes::clock::FakeClock;
use fakes::{Deadline, TempDir};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

const ID: &str = "s_0000000000000001";
const OTHER: &str = "s_0000000000000002";

fn status(name: &str) -> SessionStatus {
    SessionStatus {
        name: name.into(),
        workspace: "/w".into(),
        parent: None,
        model: "m".into(),
        thinking: None,
        state: SessionState::Idle,
        since: 0,
        started: 0,
        calls: 0,
        turns: Some(0),
        step: None,
        git: None,
        context: None,
        spend: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        delegates: 0,
        jobs: 0,
        project: "p".into(),
        clients: 0,
    }
}

fn ack(command: &str) -> Value {
    serde_json::json!({"kind":"command_accepted", "schema_version":contract::SCHEMA_VERSION, "payload":{"command_id":command}})
}

fn summary(s: &SessionStatus) -> Value {
    serde_json::json!({"kind":"session_status", "payload":s})
}

struct Home {
    temp: TempDir,
    clock: Arc<FakeClock>,
}

impl Home {
    fn new() -> Self {
        Self {
            temp: TempDir::new("sl"),
            clock: FakeClock::new(),
        }
    }
    fn run(&self) -> PathBuf {
        config::run_dir(self.temp.path())
    }
    fn client(&self) -> SessionSockets {
        SessionSockets::new(self.temp.path(), self.clock.clone())
    }
    fn listener(&self, id: &str) -> UnixListener {
        fs::create_dir_all(self.run()).unwrap();
        UnixListener::bind(self.run().join(id)).unwrap()
    }
    fn start(&self, cancel: Arc<CancelToken>) -> Receiver<Result<Vec<Running>, ListError>> {
        let client = self.client();
        let (tx, rx) = mpsc::channel();
        thread::spawn(
            move || {
                if let Ok(()) = tx.send(client.list(cancel.as_ref())) {}
            },
        );
        rx
    }
    fn advance(&self, deadline: &Deadline) {
        self.clock.advance_when_parked(
            Some(self.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap()),
            None,
            LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap(),
            deadline,
        );
    }
}

// Socket I/O is bounded by one wall deadline, including the peer's own
// waits. The release sender belongs to the test and drops on assertion failure.
struct Peer {
    subscribed: Receiver<String>,
    ended: Receiver<()>,
    release: Option<Sender<()>>,
}

impl Peer {
    fn new(
        home: &Home,
        id: &str,
        script: impl FnOnce(&str, &mut UnixStream) + Send + 'static,
    ) -> Self {
        let listener = home.listener(id);
        let (sub_tx, subscribed) = mpsc::channel();
        let (end_tx, ended) = mpsc::channel();
        thread::spawn(move || {
            let deadline = Deadline::after(fakes::deadline::WAITS);
            listener.set_nonblocking(true).unwrap();
            let mut fds = [PollFd::new(&listener, PollFlags::IN)];
            let bound = rustix::event::Timespec::try_from(deadline.left()).unwrap();
            rustix::event::poll(&mut fds, Some(&bound)).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(deadline.left())).unwrap();
            stream.set_write_timeout(Some(deadline.left())).unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let line: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(line["command"], "subscribe");
            assert_eq!(line["args"], serde_json::json!({"level":"summary"}));
            let command = line["id"].as_str().unwrap();
            assert!(command.starts_with("c_"));
            if let Ok(()) = sub_tx.send(command.to_owned()) {}
            script(command, &mut stream);
            stream.set_read_timeout(Some(deadline.left())).unwrap();
            let mut rest = Vec::new();
            match stream.read_to_end(&mut rest) {
                Ok(_) => assert!(rest.is_empty()),
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
                Err(e) => panic!("peer EOF: {e}"),
            }
            end_tx.send(()).unwrap();
        });
        Self {
            subscribed,
            ended,
            release: None,
        }
    }
    fn answer(home: &Home, id: &str, s: SessionStatus) -> Self {
        Self::new(home, id, move |id, stream| {
            writeln!(stream, "{}", ack(id)).unwrap();
            writeln!(stream, "{}", summary(&s)).unwrap();
        })
    }
    fn hold(home: &Home, id: &str, partial: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let (ready, written) = mpsc::channel();
        let mut peer = Self::new(home, id, move |id, stream| {
            if partial {
                writeln!(stream, "{}", ack(id)).unwrap();
                stream.write_all(b"{\"kind\":\"session_status\"").unwrap();
            }
            ready.send(id.to_owned()).unwrap();
            // The test owns tx, including on unwind.
            if let Ok(()) = rx.recv() {}
        });
        peer.subscribed = written;
        peer.release = Some(tx);
        peer
    }
    fn release(&mut self) {
        self.release.take();
    }
    fn subscribed(&self, deadline: &Deadline) -> String {
        deadline.recv_or_fail(&self.subscribed, "peer read summary subscription")
    }
    fn ended(&self, deadline: &Deadline) {
        deadline.recv_or_fail(&self.ended, "peer saw EOF");
    }
}

#[test]
fn missing_run_is_empty_and_unreadable_run_is_an_error() {
    let home = Home::new();
    assert_eq!(home.client().list(&CancelToken::new()), Ok(vec![]));
    fs::create_dir(home.run()).unwrap();
    fs::set_permissions(home.run(), fs::Permissions::from_mode(0o000)).unwrap();
    let result = home.client().list(&CancelToken::new());
    fs::set_permissions(home.run(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(ListError::Io { path, .. }) if path == home.run()));
}

#[test]
fn only_valid_session_names_are_connected_and_stale_paths_are_omitted() {
    let home = Home::new();
    let invalid = [
        "hub",
        "s_000000000000000",
        "s_00000000000000000",
        "s_AAAAAAAAAAAAAAAA",
        "notes",
        "s_gggggggggggggggg",
    ];
    let listeners: Vec<_> = invalid.iter().map(|id| home.listener(id)).collect();
    let stale = home.listener(OTHER);
    drop(stale);
    fs::write(home.run().join("s_0000000000000003"), "not a socket").unwrap();
    let s = status("answer");
    let peer = Peer::answer(&home, ID, s.clone());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = deadline
        .recv_or_fail(&home.start(Arc::new(CancelToken::new())), "listing")
        .unwrap();
    assert_eq!(
        result,
        vec![Running {
            id: SessionId(ID.into()),
            status: Some(s)
        }]
    );
    peer.ended(&deadline);
    for listener in listeners {
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn every_answer_is_returned_in_id_order_without_advancing_time() {
    let home = Home::new();
    let ids = ["s_0000000000000003", OTHER, ID];
    let peers: Vec<_> = ids
        .iter()
        .map(|id| Peer::answer(&home, id, status(id)))
        .collect();
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = deadline
        .recv_or_fail(&home.start(Arc::new(CancelToken::new())), "all answers")
        .unwrap();
    let expected: Vec<_> = ids
        .iter()
        .rev()
        .map(|id| Running {
            id: SessionId((*id).into()),
            status: Some(status(id)),
        })
        .collect();
    assert_eq!(result, expected);
    for peer in peers {
        peer.ended(&deadline);
    }
}

#[test]
fn a_silent_peer_does_not_delay_an_answers_close_and_is_listed_at_cutoff() {
    let home = Home::new();
    let mut hold = Peer::hold(&home, OTHER, false);
    let s = status("ready");
    let answer = Peer::answer(&home, ID, s.clone());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = home.start(Arc::new(CancelToken::new()));
    hold.subscribed(&deadline);
    answer.ended(&deadline);
    home.advance(&deadline);
    assert_eq!(
        deadline.recv_or_fail(&result, "cutoff").unwrap(),
        vec![
            Running {
                id: SessionId(ID.into()),
                status: Some(s)
            },
            Running {
                id: SessionId(OTHER.into()),
                status: None
            }
        ]
    );
    hold.release();
    hold.ended(&deadline);
}

#[test]
fn an_unfinished_status_is_not_an_answer() {
    let home = Home::new();
    let mut peer = Peer::hold(&home, ID, true);
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = home.start(Arc::new(CancelToken::new()));
    peer.subscribed(&deadline);
    home.advance(&deadline);
    assert_eq!(
        deadline.recv_or_fail(&result, "partial cutoff").unwrap(),
        vec![Running {
            id: SessionId(ID.into()),
            status: None
        }]
    );
    peer.release();
    peer.ended(&deadline);
}

#[test]
fn the_first_json_line_must_accept_our_command_at_our_version() {
    for case in 0..6 {
        let home = Home::new();
        let peer = Peer::new(&home, ID, move |id, stream| {
            let mut first = ack(id);
            match case {
                0 => first["kind"] = "command_rejected".into(),
                1 => first["payload"]["command_id"] = "c_other".into(),
                2 => first["schema_version"] = (contract::SCHEMA_VERSION + 1).into(),
                3 => {}
                4 => {}
                5 => first["kind"] = "extensions_loaded".into(),
                _ => unreachable!(),
            }
            writeln!(stream, "{first}").unwrap();
            if case == 4 {
                writeln!(stream, "{}", serde_json::json!({"kind":"session_status", "payload":{"name":"missing workspace"}})).unwrap();
            }
            if case == 3 {
                stream.shutdown(std::net::Shutdown::Write).unwrap();
            }
        });
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let result = deadline
            .recv_or_fail(
                &home.start(Arc::new(CancelToken::new())),
                "invalid protocol",
            )
            .unwrap();
        assert_eq!(
            result,
            vec![Running {
                id: SessionId(ID.into()),
                status: None
            }],
            "case {case}"
        );
        peer.ended(&deadline);
    }
}

#[test]
fn unrelated_and_non_json_lines_are_skipped_after_the_ack() {
    let home = Home::new();
    let peer = Peer::new(&home, ID, |id, stream| {
        writeln!(stream, "not JSON").unwrap();
        writeln!(stream, "{}", ack(id)).unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::json!({"kind":"extensions_loaded", "payload":{}})
        )
        .unwrap();
        writeln!(stream, "not JSON either").unwrap();
        writeln!(stream, "{}", summary(&status("ready"))).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    assert_eq!(
        deadline
            .recv_or_fail(
                &home.start(Arc::new(CancelToken::new())),
                "status after other lines"
            )
            .unwrap(),
        vec![Running {
            id: SessionId(ID.into()),
            status: Some(status("ready"))
        }]
    );
    peer.ended(&deadline);
}

#[test]
fn cancellation_before_discovery_connects_to_nothing() {
    let home = Home::new();
    let listener = home.listener(ID);
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(home.client().list(&cancel), Err(ListError::Cancelled));
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancellation_wakes_the_poll_without_advancing_time() {
    let home = Home::new();
    let mut peer = Peer::hold(&home, ID, false);
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = home.start(cancel.clone());
    peer.subscribed(&deadline);
    assert!(home.clock.await_parked(
        home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap(),
        fakes::deadline::WAITS
    ));
    cancel.cancel();
    assert_eq!(
        deadline.recv_or_fail(&result, "cancelled poll"),
        Err(ListError::Cancelled)
    );
    peer.release();
    peer.ended(&deadline);
}

#[test]
fn each_subscription_uses_a_fresh_command_id() {
    let home = Home::new();
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let first = Peer::answer(&home, ID, status("first"));
    let result = home.start(Arc::new(CancelToken::new()));
    let a = first.subscribed(&deadline);
    assert!(
        deadline.recv_or_fail(&result, "first listing").unwrap()[0]
            .status
            .is_some()
    );
    first.ended(&deadline);
    fs::remove_file(home.run().join(ID)).unwrap();
    let second = Peer::answer(&home, ID, status("second"));
    let result = home.start(Arc::new(CancelToken::new()));
    let b = second.subscribed(&deadline);
    assert!(
        deadline.recv_or_fail(&result, "second listing").unwrap()[0]
            .status
            .is_some()
    );
    assert_ne!(a, b);
    assert!(a.starts_with("c_"));
    assert!(b.starts_with("c_"));
    second.ended(&deadline);
}

#[test]
fn the_line_cap_allows_the_boundary_and_rejects_one_more_unterminated_byte() {
    for size in [MAX_LINE - 1, MAX_LINE, MAX_LINE + 1] {
        let home = Home::new();
        let base = serde_json::to_vec(&summary(&status(""))).unwrap().len();
        let mut s = status(&"x".repeat(size - base));
        s.parent = None;
        let expected = s.clone();
        let peer = Peer::new(&home, ID, move |id, stream| {
            writeln!(stream, "{}", ack(id)).unwrap();
            let bytes = serde_json::to_vec(&summary(&s)).unwrap();
            assert_eq!(bytes.len(), size);
            stream.write_all(&bytes).unwrap();
            if size <= MAX_LINE {
                stream.write_all(b"\n").unwrap();
            }
        });
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let result = deadline
            .recv_or_fail(&home.start(Arc::new(CancelToken::new())), "line cap")
            .unwrap();
        assert_eq!(result.len(), 1);
        match result[0].status.as_ref() {
            Some(s) => {
                assert!(size <= MAX_LINE);
                assert_eq!(s.name.len(), expected.name.len());
                assert!(s == &expected);
            }
            None => assert_eq!(size, MAX_LINE + 1),
        }
        peer.ended(&deadline);
    }
}

// Signals only on the poll after the read seam has seen the whole prefix.
struct PrefixClock {
    clock: Arc<FakeClock>,
    prefix_read: Arc<std::sync::atomic::AtomicBool>,
    polled: Sender<()>,
}

impl Clock for PrefixClock {
    fn now(&self) -> Instant {
        self.clock.now()
    }
    fn wall(&self) -> std::time::SystemTime {
        self.clock.wall()
    }
    fn sleep(&self, duration: Duration) {
        self.clock.sleep(duration);
    }
    fn subscribe(&self, wake: std::sync::Weak<dyn Wake>) {
        self.clock.subscribe(wake);
    }
    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.clock.wait_until(until, &mut |bound| {
            if self
                .prefix_read
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.polled.send(()).unwrap();
            }
            wait(bound);
        });
    }
}

#[test]
fn a_literal_sized_split_summary_survives_below_one_mib() {
    let home = Home::new();
    let expected = status(&"x".repeat(8192));
    let sent_status = expected.clone();
    let (release, held) = mpsc::channel();
    let peer = Peer::new(&home, ID, move |id, stream| {
        writeln!(stream, "{}", ack(id)).unwrap();
        let bytes = serde_json::to_vec(&summary(&sent_status)).unwrap();
        stream.write_all(&bytes[..4096]).unwrap();
        if held.recv().is_ok() {
            stream.write_all(&bytes[4096..]).unwrap();
            stream.write_all(b"\n").unwrap();
        }
    });
    let prefix_read = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (polled, next_poll) = mpsc::channel();
    let client = SessionSockets::new(
        home.temp.path(),
        Arc::new(PrefixClock {
            clock: home.clock.clone(),
            prefix_read: prefix_read.clone(),
            polled,
        }),
    );
    let cutoff = home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let mut seen = Vec::new();
        let answer =
            client.list_until_with_read(cutoff, &CancelToken::new(), &mut |stream, chunk| {
                let n = stream.read(chunk)?;
                seen.extend_from_slice(&chunk[..n]);
                if let Some(end) = seen.iter().position(|byte| *byte == b'\n')
                    && seen.len() - end > 4096
                {
                    prefix_read.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(n)
            });
        done.send(answer).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    deadline.recv_or_fail(&next_poll, "prefix processed and next poll entered");
    release.send(()).unwrap();
    let entries = deadline
        .recv_or_fail(&result, "complete split summary")
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, SessionId(ID.into()));
    let actual = entries[0].status.as_ref().expect("complete status");
    assert_eq!(actual.name.len(), 8192);
    assert!(actual == &expected, "complete status content");
    peer.ended(&deadline);
}

#[test]
fn would_block_after_readiness_preserves_the_subscription() {
    let home = Home::new();
    let expected = status("ready after WouldBlock");
    let peer = Peer::answer(&home, ID, expected.clone());
    let client = home.client();
    let cutoff = home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let mut injected = false;
        let answer =
            client.list_until_with_read(cutoff, &CancelToken::new(), &mut |stream, chunk| {
                if !injected {
                    injected = true;
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                stream.read(chunk)
            });
        done.send((injected, answer)).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let (injected, answer) = deadline.recv_or_fail(&result, "summary after WouldBlock");
    assert!(injected);
    assert_eq!(
        answer,
        Ok(vec![Running {
            id: SessionId(ID.into()),
            status: Some(expected),
        }])
    );
    peer.ended(&deadline);
}

#[test]
fn a_hard_read_error_closes_the_peer_without_advancing_time() {
    let home = Home::new();
    // The peer stays open, waiting for EOF, so neither EOF nor a clock
    // advance can rescue a read error incorrectly treated as WouldBlock.
    let peer = Peer::new(&home, ID, |id, stream| {
        let mut bytes = serde_json::to_vec(&ack(id)).unwrap();
        bytes.push(b'\n');
        stream.write_all(&bytes).unwrap();
    });
    let client = home.client();
    let cutoff = home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let mut injected = false;
        let answer =
            client.list_until_with_read(cutoff, &CancelToken::new(), &mut |stream, chunk| {
                if !injected {
                    injected = true;
                    return Err(io::ErrorKind::ConnectionReset.into());
                }
                stream.read(chunk)
            });
        done.send((injected, answer)).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    peer.ended(&deadline);
    let (injected, answer) = deadline.recv_or_fail(&result, "hard read error closes listing");
    assert!(injected);
    assert_eq!(
        answer,
        Ok(vec![Running {
            id: SessionId(ID.into()),
            status: None
        }])
    );
}

#[test]
fn discovery_at_the_cutoff_connects_to_nothing_and_cancel_takes_priority() {
    let home = Home::new();
    let listeners: Vec<_> = [ID, OTHER, "s_0000000000000003"]
        .into_iter()
        .map(|id| home.listener(id))
        .collect();
    assert_eq!(
        home.client()
            .list_until(home.clock.now(), &CancelToken::new()),
        Ok(vec![])
    );
    for listener in listeners {
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        home.client()
            .list_until(home.clock.now() + LIST_DEADLINE, &cancel),
        Err(ListError::Cancelled)
    );
}

#[test]
fn unending_short_lines_cannot_outlive_the_cutoff_or_cancellation() {
    for (json, cancel_call) in [(false, false), (true, false), (false, true)] {
        let home = Home::new();
        let (sent, written) = mpsc::channel();
        let peer = Peer::new(&home, ID, move |id, stream| {
            writeln!(stream, "{}", ack(id)).unwrap();
            let line = if json {
                b"{\"kind\":\"notice\",\"payload\":{}}\n".as_slice()
            } else {
                b"x\n".as_slice()
            };
            for _ in 0..1_000 {
                stream.write_all(line).unwrap();
            }
            sent.send(()).unwrap();
            while stream.write_all(line).is_ok() {}
        });
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let cancel = Arc::new(CancelToken::new());
        let result = home.start(cancel.clone());
        deadline.recv_or_fail(&written, "first 1000 flood lines written");
        if cancel_call {
            assert!(home.clock.await_parked(
                home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap(),
                fakes::deadline::WAITS
            ));
            cancel.cancel();
            assert_eq!(
                deadline.recv_or_fail(&result, "flood cancelled"),
                Err(ListError::Cancelled)
            );
        } else {
            home.advance(&deadline);
            assert_eq!(
                deadline.recv_or_fail(&result, "flood cutoff").unwrap(),
                vec![Running {
                    id: SessionId(ID.into()),
                    status: None
                }]
            );
        }
        peer.ended(&deadline);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_backlog_is_not_an_accepted_connection() {
    let home = Home::new();
    let listener = home.listener(ID);
    let address = rustix::net::SocketAddrUnix::new(home.run().join(ID)).unwrap();
    let mut filled = Vec::new();
    let deadline = Deadline::after(fakes::deadline::WAITS);
    loop {
        assert!(!deadline.left().is_zero(), "backlog filled within deadline");
        let socket = crate::hub::nonblocking_socket().unwrap();
        match rustix::net::connect(&socket, &address) {
            Ok(()) => filled.push(socket),
            Err(rustix::io::Errno::AGAIN) => break,
            Err(e) => panic!("filling backlog: {e}"),
        }
    }
    assert_eq!(home.client().list(&CancelToken::new()), Ok(vec![]));
    drop(listener);
}
