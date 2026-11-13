//! Session-side keeper lifetime and pending-start retirement.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use super::*;
use contract::keeper::{HostStdin, KeptJob, Stream};
use fakes::clock::FakeClock;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::peer::Pid;

fn deadline() -> fakes::Deadline {
    fakes::Deadline::after(Duration::from_secs(10))
}
fn spec(id: &str) -> Spawn {
    Spawn {
        id: id.into(),
        program: "/bin/sh".into(),
        args: vec![],
        cwd: "/".into(),
        env: vec![],
        stdin: HostStdin::Null,
        timeout_ms: None,
        output_cap: 100,
        sandbox: None,
    }
}
fn fixture(
    fail_first: bool,
) -> (
    Arc<Host>,
    Arc<FakeClock>,
    mpsc::Receiver<UnixStream>,
    Arc<AtomicUsize>,
) {
    fixture_with_pause(fail_first, None)
}
fn fixture_with_pause(
    fail_first: bool,
    pause: Option<Arc<TestPause>>,
) -> (
    Arc<Host>,
    Arc<FakeClock>,
    mpsc::Receiver<UnixStream>,
    Arc<AtomicUsize>,
) {
    let clock = FakeClock::new();
    let (send, peers) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let counts = count.clone();
    let spawner: Spawner = Arc::new(move || {
        let n = counts.fetch_add(1, Ordering::SeqCst);
        if fail_first && n == 0 {
            return Err(io::Error::other("injected spawn failure"));
        }
        let (stream, peer) = UnixStream::pair()?;
        send.send(peer).unwrap();
        Ok(Connection {
            stream,
            lifeline: Box::new(()),
            child: None,
        })
    });
    (
        match pause {
            Some(pause) => Host::new_with_pause(spawner, clock.clone(), Some(pause)),
            None => Host::new(spawner, clock.clone()),
        },
        clock,
        peers,
        count,
    )
}
fn start(
    host: &Arc<Host>,
    id: &str,
) -> (
    thread::ThreadId,
    mpsc::Receiver<Result<Started, HostError>>,
    mpsc::Receiver<HostEvent>,
) {
    let host = host.clone();
    let id = id.to_owned();
    let (send, result) = mpsc::channel();
    let (events, output) = mpsc::sync_channel(contract::keeper::HOST_EVENT_QUEUE);
    let worker = thread::spawn(move || match send.send(host.start(spec(&id), events)) {
        Ok(()) | Err(_) => {}
    });
    (worker.thread().id(), result, output)
}
fn line(peer: &UnixStream) -> serde_json::Value {
    let mut peer = peer.try_clone().unwrap();
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let parsed = (|| {
            BufReader::new(&mut peer).read_line(&mut text)?;
            serde_json::from_str(&text).map_err(io::Error::other)
        })();
        match send.send(parsed) {
            Ok(()) | Err(_) => {}
        }
    });
    deadline()
        .recv_or_fail(&recv, "keeper control line")
        .unwrap()
}
/// What `peer` receives until the session closes the connection, read on a
/// worker so the wait has a deadline.
fn rest_of(peer: &UnixStream) -> String {
    let mut peer = peer.try_clone().unwrap();
    let (send, rest) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let read = peer.read_to_string(&mut text);
        match send.send(read.map(|_| text)) {
            Ok(()) | Err(_) => {}
        }
    });
    deadline()
        .recv_or_fail(&rest, "the end of the connection")
        .unwrap()
}
fn reply(peer: &mut UnixStream, value: serde_json::Value) {
    serde_json::to_writer(&mut *peer, &value).unwrap();
    peer.write_all(b"\n").unwrap();
}
fn started(peer: &mut UnixStream, id: &str) {
    reply(peer, serde_json::json!({"started":{"id":id,"pgid":4242}}));
}

#[test]
fn keeper_is_lazy_reused_and_shutdown_is_explicit() {
    let (host, _, peers, count) = fixture(false);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let (_, first, _) = start(&host, "one");
    let mut peer = deadline().recv(&peers).unwrap();
    assert_eq!(line(&peer)["start"]["id"], "one");
    started(&mut peer, "one");
    assert_eq!(deadline().recv(&first).unwrap().unwrap().pgid, 4242);
    let (_, second, _) = start(&host, "two");
    assert_eq!(line(&peer)["start"]["id"], "two");
    started(&mut peer, "two");
    assert!(deadline().recv(&second).unwrap().is_ok());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    host.send_shutdown();
    assert_eq!(line(&peer), serde_json::json!({"shutdown":true}));
    drop(host);
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    let mut rest = String::new();
    BufReader::new(peer).read_to_string(&mut rest).unwrap();
    assert!(rest.is_empty(), "Drop must not write a shutdown");
}

#[test]
fn keeper_spawn_failure_retires_and_retries() {
    let (host, _, peers, count) = fixture(true);
    let (_, result, _) = start(&host, "bad");
    assert!(deadline().recv(&result).unwrap().is_err());
    assert!(lock(&host.slot).is_none());
    let (_, result, _) = start(&host, "retry");
    let mut peer = deadline().recv(&peers).unwrap();
    assert_eq!(line(&peer)["start"]["id"], "retry");
    started(&mut peer, "retry");
    assert!(deadline().recv(&result).unwrap().is_ok());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(lock(&lock(&host.slot).as_ref().unwrap().pending).is_empty());
}

#[test]
fn keeper_timeout_retires_and_late_started_cannot_install_a_waiter() {
    let (host, clock, peers, count) = fixture(false);
    let at = clock.now() + KEEPER_START;
    let (worker, result, _) = start(&host, "expired");
    let mut peer = deadline().recv(&peers).unwrap();
    assert_eq!(line(&peer)["start"]["id"], "expired");
    // start owns slot while awaiting its acknowledgement; inspect through the demux's client.
    let client = deadline()
        .recv(&lock(&host.clients))
        .unwrap()
        .upgrade()
        .unwrap();
    assert_eq!(lock(&client.pending).len(), 1);
    clock.advance_when_parked(Some(at), Some(worker), KEEPER_START, &deadline());
    assert!(deadline().recv(&result).unwrap().is_err());
    assert!(lock(&client.pending).is_empty());
    started(&mut peer, "expired");
    // A following start/reply on the same ordered connection fences the late reply.
    let (_, retry, _) = start(&host, "retry");
    assert_eq!(line(&peer)["start"]["id"], "retry");
    started(&mut peer, "retry");
    assert!(deadline().recv(&retry).unwrap().is_ok());
    assert!(!lock(&client.waiters).contains_key("expired"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn keeper_failed_reply_retires_and_retry_succeeds() {
    let (host, _, peers, _) = fixture(false);
    let (_, result, _) = start(&host, "bad");
    let mut peer = deadline().recv(&peers).unwrap();
    line(&peer);
    reply(
        &mut peer,
        serde_json::json!({"failed":{"id":"bad","error":"injected"}}),
    );
    assert!(deadline().recv(&result).unwrap().is_err());
    let client = deadline()
        .recv(&lock(&host.clients))
        .unwrap()
        .upgrade()
        .unwrap();
    assert!(lock(&client.pending).is_empty());
    let (_, result, _) = start(&host, "good");
    line(&peer);
    started(&mut peer, "good");
    assert!(deadline().recv(&result).unwrap().is_ok());
}

#[test]
fn keeper_control_eof_drops_runs_retires_starts_and_respawns() {
    let (host, _, peers, count) = fixture(false);
    let (_, first, output) = start(&host, "live");
    let mut peer = deadline().recv(&peers).unwrap();
    line(&peer);
    started(&mut peer, "live");
    assert!(deadline().recv(&first).unwrap().is_ok());
    let (_, pending, _) = start(&host, "pending");
    line(&peer);
    let client = deadline()
        .recv(&lock(&host.clients))
        .unwrap()
        .upgrade()
        .unwrap();
    drop(peer);
    assert!(deadline().recv(&pending).unwrap().is_err());
    assert!(deadline().recv(&output).is_err());
    assert!(lock(&client.pending).is_empty());
    let (_, retry, _) = start(&host, "retry");
    let mut peer = deadline().recv(&peers).unwrap();
    line(&peer);
    started(&mut peer, "retry");
    assert!(deadline().recv(&retry).unwrap().is_ok());
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn keeper_write_failure_uses_pending_retirement() {
    let (host, _, _, _) = fixture(false);
    let (stream, peer) = UnixStream::pair().unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let client = Client::new(
        Connection {
            stream,
            lifeline: Box::new(()),
            child: None,
        },
        &host,
    )
    .unwrap();
    let (events, _) = mpsc::sync_channel(contract::keeper::HOST_EVENT_QUEUE);
    assert!(host.start_on(&client, spec("broken"), events).is_err());
    assert!(lock(&client.pending).is_empty());
    drop(peer);
}

#[test]
fn keeper_confirms_a_durable_hosted_job_end_once() {
    let (host, clock, peers, _) = fixture(false);
    let home = fakes::TempDir::new("main-keeper-confirmed");
    let log = log::Log::create(home.path(), SessionId("s_0123456789abcdef".into()), clock).unwrap();
    let _observer = host.observe(&log);
    let (_, result, _) = start(&host, "call");
    let mut peer = deadline().recv(&peers).unwrap();
    line(&peer);
    started(&mut peer, "call");
    assert!(deadline().recv(&result).unwrap().is_ok());
    let job = JobId("j_0123456789abcdef".into());
    host.adopt("call", &job, &home.path().join("out"), None, false)
        .unwrap();
    assert_eq!(line(&peer)["adopt"]["id"], "call");
    let event = contract::events::Event::JobCompleted(tools::kept_end(
        job,
        &contract::keeper::Exit {
            code: Some(0),
            signal: None,
            timed_out: false,
            output_cap: false,
        },
    ));
    log.append(&event, None, None).unwrap();
    assert_eq!(line(&peer), serde_json::json!({"confirmed":"call"}));
    log.append(&event, None, None).unwrap();
    host.send_shutdown();
    assert_eq!(
        line(&peer),
        serde_json::json!({"shutdown":true}),
        "a repeated end must not confirm twice"
    );
}

#[test]
fn keeper_demux_preserves_output_then_exit() {
    let (host, _, peers, _) = fixture(false);
    let (_, first, output) = start(&host, "live");
    let mut peer = deadline().recv(&peers).unwrap();
    line(&peer);
    started(&mut peer, "live");
    assert!(deadline().recv(&first).unwrap().is_ok());
    reply(
        &mut peer,
        serde_json::json!({"output":{"id":"live","stream":"out","bytes":"aGk="}}),
    );
    reply(
        &mut peer,
        serde_json::json!({"exit":{"id":"live","code":0,"signal":null,"timed_out":false,"output_cap":false}}),
    );
    let dl = deadline();
    assert!(
        matches!(dl.recv_or_fail(&output, "keeper output"), HostEvent::Output { stream: Stream::Out, bytes } if bytes == b"hi")
    );
    assert!(
        matches!(dl.recv_or_fail(&output, "keeper exit"), HostEvent::Exit(exit) if exit.code == Some(0))
    );
}

#[test]
fn ordinary_client_rejects_handover_frames() {
    let (host, _, _, _) = fixture(false);
    let (stream, _peer) = UnixStream::pair().unwrap();
    let client = Client::new(
        Connection {
            stream,
            lifeline: Box::new(()),
            child: None,
        },
        &host,
    )
    .unwrap();
    for frame in [
        FromKeeper::Identity {
            session_id: SessionId("s_0123456789abcdef".into()),
        },
        FromKeeper::Handover {
            running: Vec::new(),
        },
        FromKeeper::AdoptDone(true),
    ] {
        // `docs/tools.md`, "The keeper": a candidate cannot issue arbitrary
        // frames before an authenticated handover, and a Start client never
        // negotiates one.
        assert!(client.reply(frame).is_err());
    }
}

// ---- taking a handover (`docs/tools.md`, "The keeper": a resume takes the log back) ----

fn session_id() -> SessionId {
    SessionId("s_0123456789abcdef".into())
}

fn kept(id: &str, job: &str) -> KeptJob {
    KeptJob {
        id: id.into(),
        job_id: JobId(job.into()),
        output_path: format!("/artifacts/{job}.log").into(),
        errors_path: None,
        tty: false,
        lines: false,
    }
}

fn dyn_clock(clock: &Arc<FakeClock>) -> Arc<dyn Clock> {
    clock.clone()
}

/// Runs the negotiation on a thread: its id, for naming the park it waits in,
/// and its result.
fn negotiating(
    stream: UnixStream,
    clock: &Arc<FakeClock>,
) -> (thread::ThreadId, mpsc::Receiver<io::Result<Handover>>) {
    let clock = dyn_clock(clock);
    let (send, result) = mpsc::channel();
    let worker = thread::spawn(
        move || match send.send(negotiate(stream, &session_id(), &clock)) {
            Ok(()) | Err(_) => {}
        },
    );
    (worker.thread().id(), result)
}

fn identity(peer: &mut UnixStream, id: &SessionId) {
    reply(peer, serde_json::json!({"identity": {"session_id": id.0}}));
}

fn handover_of(peer: &mut UnixStream, running: serde_json::Value) {
    reply(peer, serde_json::json!({"handover": {"running": running}}));
}

fn kept_json(id: &str, job: &str) -> serde_json::Value {
    serde_json::json!({"id": id, "job_id": job, "output_path": format!("/artifacts/{job}.log"),
        "errors_path": null, "tty": false, "lines": false})
}

#[test]
fn takeover_reads_the_kernel_credential_from_the_connection() {
    let clock = FakeClock::new();
    let (session, mut peer) = UnixStream::pair().unwrap();
    let (_, result) = negotiating(session, &clock);
    assert_eq!(
        line(&peer),
        serde_json::json!({"identify": "s_0123456789abcdef"})
    );
    identity(&mut peer, &session_id());
    assert_eq!(line(&peer), serde_json::json!({"handover": true}));
    handover_of(&mut peer, serde_json::json!([kept_json("call_0", "j_one")]));
    let handover = match deadline().recv(&result).unwrap() {
        Ok(handover) => handover,
        Err(error) => panic!("negotiation failed: {error}"),
    };
    // The socket pair's other end is this process: the kernel says so. No
    // frame and no argument carried it.
    assert_eq!(handover.pid(), Pid(std::process::id()));
    assert_eq!(handover.running(), [kept("call_0", "j_one")]);
}

#[test]
fn a_mismatched_identity_releases_nothing() {
    let clock = FakeClock::new();
    let (session, mut peer) = UnixStream::pair().unwrap();
    let (_, result) = negotiating(session, &clock);
    line(&peer);
    identity(&mut peer, &SessionId("s_ffffffffffffffff".into()));
    assert!(deadline().recv(&result).unwrap().is_err());
    // The session closed the connection without asking for the handover.
    let rest = rest_of(&peer);
    assert!(rest.is_empty(), "sent {rest:?} after a mismatch");
}

#[test]
fn a_reply_that_is_not_an_identity_releases_nothing() {
    let clock = FakeClock::new();
    for answer in [
        serde_json::json!({"handover": {"running": []}}),
        serde_json::json!({"adopt_done": true}),
        serde_json::json!({"started": {"id": "call_0", "pgid": 1}}),
    ] {
        let (session, mut peer) = UnixStream::pair().unwrap();
        let (_, result) = negotiating(session, &clock);
        line(&peer);
        reply(&mut peer, answer);
        assert!(deadline().recv(&result).unwrap().is_err());
        assert!(rest_of(&peer).is_empty());
    }
}

#[test]
fn a_keeper_that_hangs_up_or_tears_a_reply_is_refused() {
    let clock = FakeClock::new();
    let (session, peer) = UnixStream::pair().unwrap();
    let (_, result) = negotiating(session, &clock);
    line(&peer);
    drop(peer);
    assert!(deadline().recv(&result).unwrap().is_err());
    // A reply cut before its line feed is a torn tail, never parsed.
    let (session, mut peer) = UnixStream::pair().unwrap();
    let (_, result) = negotiating(session, &clock);
    line(&peer);
    peer.write_all(br#"{"identity":{"session_id":"s_0123456789abcdef"}}"#)
        .unwrap();
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(deadline().recv(&result).unwrap().is_err());
}

#[test]
fn the_negotiation_deadline_spans_identity_and_acknowledgement() {
    // One deadline covers both answers: 1 ms before it a complete exchange
    // succeeds, at it the connection ends.
    let origin_for = |clock: &Arc<FakeClock>| clock.now() + KEEPER_START;
    for (late, ok) in [
        (KEEPER_START.saturating_sub(Duration::from_millis(1)), true),
        (KEEPER_START, false),
    ] {
        let clock = FakeClock::new();
        let until = origin_for(&clock);
        let (session, mut peer) = UnixStream::pair().unwrap();
        let (_, result) = negotiating(session, &clock);
        assert_eq!(line(&peer)["identify"], "s_0123456789abcdef");
        identity(&mut peer, &session_id());
        assert_eq!(line(&peer)["handover"], true);
        let worker = clock
            .mark_parked(until, deadline_fake())
            .expect("the negotiation parks at its deadline")
            .threads()
            .into_iter()
            .next()
            .expect("deadline watcher");
        clock.advance_when_parked(Some(until), Some(worker), late, &deadline_fake());
        if ok {
            clock
                .mark_parked(until, deadline_fake())
                .expect("still parked");
            handover_of(&mut peer, serde_json::json!([]));
            assert!(deadline().recv(&result).unwrap().is_ok());
        } else {
            assert_eq!(
                deadline().recv(&result).unwrap().err().map(|e| e.kind()),
                Some(io::ErrorKind::TimedOut)
            );
        }
    }
}

fn deadline_fake() -> fakes::Deadline {
    fakes::Deadline::after(Duration::from_secs(10))
}

/// A host that took a handover from a scripted keeper: the host, its clock,
/// the keeper's end, and the attached handover.
fn attached_host(
    running: Vec<KeptJob>,
    trailing: &[u8],
) -> (
    Arc<Host>,
    Arc<FakeClock>,
    mpsc::Receiver<UnixStream>,
    UnixStream,
    Attached,
) {
    attached_host_with_pause(running, trailing, None)
}
fn attached_host_with_pause(
    running: Vec<KeptJob>,
    trailing: &[u8],
    pause: Option<Arc<TestPause>>,
) -> (
    Arc<Host>,
    Arc<FakeClock>,
    mpsc::Receiver<UnixStream>,
    UnixStream,
    Attached,
) {
    let (host, clock, peers, _) = fixture_with_pause(false, pause);
    let (session, mut peer) = UnixStream::pair().unwrap();
    let (_, result) = negotiating(session, &clock);
    line(&peer);
    identity(&mut peer, &session_id());
    line(&peer);
    let mut bytes =
        serde_json::to_vec(&serde_json::json!({"handover": {"running": running}})).unwrap();
    bytes.push(b'\n');
    // The trailing frames share the write with the answer, so they sit in the
    // negotiating reader's buffer.
    bytes.extend_from_slice(trailing);
    peer.write_all(&bytes).unwrap();
    let handover = match deadline().recv(&result).unwrap() {
        Ok(handover) => handover,
        Err(error) => panic!("negotiation failed: {error}"),
    };
    let attached = host.attach_handover(handover).unwrap();
    (host, clock, peers, peer, attached)
}

#[test]
fn frames_read_ahead_of_the_acknowledgement_reach_the_runner() {
    let trailing = b"{\"output\":{\"id\":\"call_0\",\"stream\":\"out\",\"bytes\":\"aGk=\"}}\n\
{\"exit\":{\"id\":\"call_0\",\"code\":0,\"signal\":null,\"timed_out\":false,\"output_cap\":false}}\n";
    let (_host, _clock, _peers, _peer, mut attached) =
        attached_host(vec![kept("call_0", "j_one")], trailing);
    let jobs = attached.take_jobs();
    assert_eq!(jobs.len(), 1);
    let dl = deadline();
    // Output and the ending arrive before the session said it was ready.
    assert!(matches!(dl.recv_or_fail(&jobs[0].events, "output"),
        HostEvent::Output { stream: Stream::Out, bytes } if bytes == b"hi"));
    assert!(matches!(dl.recv_or_fail(&jobs[0].events, "exit"),
        HostEvent::Exit(exit) if exit.code == Some(0)));
}

#[test]
fn ready_sends_adopted_once_and_waits_for_the_acknowledgement() {
    let (_host, clock, _peers, mut peer, attached) =
        attached_host(vec![kept("call_0", "j_one")], b"");
    let until = clock.now() + KEEPER_START;
    let (send, result) = mpsc::channel();
    let waiting = thread::spawn(
        move || match send.send(attached.ready().map(|()| attached)) {
            Ok(()) | Err(_) => {}
        },
    );
    assert_eq!(line(&peer), serde_json::json!({"adopted": true}));
    let worker = waiting.thread().id();
    clock
        .mark_parked(until, deadline_fake())
        .expect("the wait parks at its deadline");
    // 1 ms before the deadline the acknowledgement still counts.
    clock.advance_when_parked(
        Some(until),
        Some(worker),
        KEEPER_START.saturating_sub(Duration::from_millis(1)),
        &deadline_fake(),
    );
    clock
        .mark_parked(until, deadline_fake())
        .expect("parks again");
    reply(&mut peer, serde_json::json!({"adopt_done": true}));
    assert!(deadline().recv(&result).unwrap().is_ok());
}

#[test]
fn ready_times_out_at_the_deadline_and_the_drop_ends_the_connection() {
    let (pause, reader_arrived, release_reader, drop_arrived, release_drop) = TestPause::new();
    let (host, clock, _peers, peer, attached) =
        attached_host_with_pause(vec![kept("call_0", "j_one")], b"", Some(pause));
    assert_eq!(host.handover_peers(), vec![Pid(std::process::id())]);
    let until = clock.now() + KEEPER_START;
    let (send, result) = mpsc::channel();
    let waiting = thread::spawn(
        move || match send.send(attached.ready().map_err(|e| e.kind())) {
            Ok(()) | Err(_) => {}
        },
    );
    assert_eq!(line(&peer)["adopted"], true);
    clock
        .mark_parked(until, deadline_fake())
        .expect("the wait parks at its deadline");
    clock.advance_when_parked(
        Some(until),
        Some(waiting.thread().id()),
        KEEPER_START,
        &deadline_fake(),
    );
    assert_eq!(
        deadline().recv(&result).unwrap(),
        Err(io::ErrorKind::TimedOut)
    );
    // The attachment dropped with the failed wait: the keeper sees the end of
    // the connection and takes the log back, and the host forgets it.
    rest_of(&peer);
    deadline().recv_or_fail(&reader_arrived, "reader pause before dead");
    deadline().recv_or_fail(&drop_arrived, "drop pause before retain");
    release_reader.send(()).unwrap();
    release_drop.send(()).unwrap();
    let (joined, join_result) = mpsc::channel();
    thread::spawn(move || match joined.send(waiting.join()) {
        Ok(()) | Err(_) => {}
    });
    deadline()
        .recv_or_fail(&join_result, "timed-out handover worker to finish")
        .unwrap();
    assert!(host.handover_peers().is_empty());
}

#[test]
fn output_is_routed_before_the_acknowledgement_arrives() {
    let (_host, _clock, _peers, mut peer, mut attached) =
        attached_host(vec![kept("call_0", "j_one")], b"");
    let jobs = attached.take_jobs();
    let (send, result) = mpsc::channel();
    let ready = thread::spawn(
        move || match send.send(attached.ready().map(|()| attached)) {
            Ok(()) | Err(_) => {}
        },
    );
    line(&peer);
    reply(
        &mut peer,
        serde_json::json!({"output":{"id":"call_0","stream":"out","bytes":"aGk="}}),
    );
    assert!(matches!(
        deadline().recv_or_fail(&jobs[0].events, "output before the acknowledgement"),
        HostEvent::Output { bytes, .. } if bytes == b"hi"
    ));
    assert!(result.try_recv().is_err(), "not acknowledged yet");
    reply(&mut peer, serde_json::json!({"adopt_done": true}));
    assert!(deadline().recv(&result).unwrap().is_ok());
    ready.join().unwrap();
}

/// Every line a keeper's end received up to the shutdown, in order.
fn lines_until_shutdown(peer: &UnixStream) -> Vec<serde_json::Value> {
    let reader = peer.try_clone().unwrap();
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for text in BufReader::new(reader).lines() {
            let Ok(text) = text else { break };
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            let last = value == serde_json::json!({"shutdown": true});
            if send.send(value).is_err() || last {
                break;
            }
        }
    });
    let dl = deadline();
    let mut seen = Vec::new();
    loop {
        let value = dl.recv_or_fail(&lines, "a line before the shutdown");
        let last = value == serde_json::json!({"shutdown": true});
        seen.push(value);
        if last {
            return seen;
        }
    }
}

#[test]
fn colliding_call_0_routes_by_owner() {
    // Both keepers name a call `call_0`: the old keeper from before the
    // crash, the new one from this session's own counter.
    let (host, clock, peers, mut old_peer, mut attached) =
        attached_host(vec![kept("call_0", "j_old")], b"");
    let log_home = fakes::TempDir::new("main-keeper-colliding");
    let log = log::Log::create(log_home.path(), session_id(), clock.clone()).unwrap();
    let _observer = host.observe(&log);
    let old = attached.take_jobs().pop().unwrap();
    attached.keep();
    let (_, started_rx, fresh_events) = start(&host, "call_0");
    let mut fresh_peer = deadline().recv(&peers).unwrap();
    assert_eq!(line(&fresh_peer)["start"]["id"], "call_0");
    started(&mut fresh_peer, "call_0");
    assert!(deadline().recv(&started_rx).unwrap().is_ok());
    let fresh = JobId("j_new".into());
    host.adopt(
        "call_0",
        &fresh,
        &log_home.path().join("fresh.out"),
        None,
        false,
    )
    .unwrap();
    // Replies arrive in the reverse order of the attachment: the fresh
    // keeper's ending first. Each lands only on its own job.
    let exit = |code: i32| serde_json::json!({"exit":{"id":"call_0","code":code,"signal":null,"timed_out":false,"output_cap":false}});
    reply(&mut fresh_peer, exit(1));
    reply(&mut old_peer, exit(2));
    let dl = deadline();
    assert!(matches!(dl.recv_or_fail(&fresh_events, "fresh exit"),
        HostEvent::Exit(exit) if exit.code == Some(1)));
    assert!(matches!(dl.recv_or_fail(&old.events, "old exit"),
        HostEvent::Exit(exit) if exit.code == Some(2)));
    // Control goes to the keeper that owns the job.
    assert!(old.owner.stop("call_0"));
    old.owner.write("call_0", "old\n").unwrap();
    assert!(host.stop("call_0"));
    host.write("call_0", "fresh\n").unwrap();
    // Durable endings confirm on the connection that holds the job.
    let ended = |job: &JobId| {
        contract::events::Event::JobCompleted(tools::kept_end(
            job.clone(),
            &contract::keeper::Exit {
                code: Some(0),
                signal: None,
                timed_out: false,
                output_cap: false,
            },
        ))
    };
    log.append(&ended(&fresh), None, None).unwrap();
    log.append(&ended(&old.job.job_id), None, None).unwrap();
    host.send_shutdown();
    assert_eq!(
        lines_until_shutdown(&old_peer),
        vec![
            serde_json::json!({"stop": "call_0"}),
            serde_json::json!({"write": {"id": "call_0", "text": "old\n"}}),
            serde_json::json!({"confirmed": "call_0"}),
            serde_json::json!({"shutdown": true}),
        ]
    );
    assert_eq!(
        lines_until_shutdown(&fresh_peer),
        vec![
            serde_json::json!({"adopt": {"id": "call_0", "job_id": "j_new",
                "output_path": log_home.path().join("fresh.out"), "errors_path": null, "lines": false}}),
            serde_json::json!({"stop": "call_0"}),
            serde_json::json!({"write": {"id": "call_0", "text": "fresh\n"}}),
            serde_json::json!({"confirmed": "call_0"}),
            serde_json::json!({"shutdown": true}),
        ]
    );
}

#[test]
fn an_old_keeper_connection_ending_leaves_the_fresh_slot_live() {
    let (host, _clock, peers, old_peer, mut attached) =
        attached_host(vec![kept("call_0", "j_old")], b"");
    let old = attached.take_jobs().pop().unwrap();
    attached.keep();
    let (_, first, _) = start(&host, "call_0");
    let mut fresh_peer = deadline().recv(&peers).unwrap();
    line(&fresh_peer);
    started(&mut fresh_peer, "call_0");
    assert!(deadline().recv(&first).unwrap().is_ok());
    old_peer.shutdown(std::net::Shutdown::Both).unwrap();
    // The old job's events end with its keeper's connection...
    assert!(deadline().recv(&old.events).is_err());
    assert!(!old.owner.stop("call_0"));
    assert!(host.handover_peers().is_empty());
    // ...and the fresh slot still serves, without spawning another keeper.
    assert!(host.stop("call_0"));
    assert_eq!(line(&fresh_peer), serde_json::json!({"stop": "call_0"}));
    assert!(peers.try_recv().is_err());
}

#[test]
fn a_fresh_keeper_connection_ending_leaves_the_old_keeper_retained() {
    let (host, _clock, peers, mut old_peer, mut attached) =
        attached_host(vec![kept("call_0", "j_old")], b"");
    let old = attached.take_jobs().pop().unwrap();
    attached.keep();
    let (_, first, _) = start(&host, "call_0");
    let fresh_peer = deadline().recv(&peers).unwrap();
    line(&fresh_peer);
    drop(fresh_peer);
    assert!(deadline().recv(&first).unwrap().is_err());
    assert!(old.owner.stop("call_0"));
    assert_eq!(line(&old_peer), serde_json::json!({"stop": "call_0"}));
    assert_eq!(host.handover_peers(), vec![Pid(std::process::id())]);
    reply(
        &mut old_peer,
        serde_json::json!({"started":{"id":"x","pgid":1}}),
    );
}

#[test]
fn dropping_the_host_sends_no_shutdown_to_a_retained_keeper() {
    let (host, _clock, _peers, peer, mut attached) =
        attached_host(vec![kept("call_0", "j_old")], b"");
    drop(attached.take_jobs());
    attached.keep();
    drop(host);
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    let rest = rest_of(&peer);
    assert!(rest.is_empty(), "Drop must not write a shutdown");
}

// ---- the bounded handover queue (see #4054) ----

#[test]
fn handover_queue_fits_the_growth_budget() {
    // The handover queue holds HOST_EVENT_QUEUE frames behind a stalled
    // runner; one runner pass takes HOST_BURST_FRAMES and leaves the rest
    // queued (`take_burst_takes_at_most_the_bound_and_stops_after_an_ending`).
    assert_eq!(contract::keeper::HOST_EVENT_QUEUE, 64);
    assert_eq!(contract::keeper::HOST_BURST_FRAMES, 16);
}

#[test]
fn a_full_handover_queue_applies_backpressure() {
    let (_host, _clock, _peers, _peer, mut attached) =
        attached_host(vec![kept("call_0", "j_one")], b"");
    let jobs = attached.take_jobs();
    assert_eq!(jobs.len(), 1);
    // The test holds the only receiver and drains nothing: exactly a full
    // queue fits, and one more frame waits instead of queueing without
    // bound. An unbounded channel would take it.
    let sender = lock(&attached.client.waiters)
        .get("call_0")
        .cloned()
        .unwrap();
    for _ in 0..contract::keeper::HOST_EVENT_QUEUE {
        match sender.try_send(HostEvent::Output {
            stream: Stream::Out,
            bytes: vec![1],
        }) {
            Ok(()) => {}
            Err(error) => panic!("room for a bounded burst: {error:?}"),
        }
    }
    assert!(
        matches!(
            sender.try_send(HostEvent::Output {
                stream: Stream::Out,
                bytes: vec![1],
            }),
            Err(mpsc::TrySendError::Full(_))
        ),
        "the handover queue applies backpressure past its bound"
    );
}

#[test]
fn handover_end_releases_client_maps() {
    // Receiving `End` alone cannot clear `client.adopted`: `attached_host`
    // installs no `Host::observe`, and that map is removed only when the
    // observer sees a durable `JobCompleted` (`keeper.rs:225-238`), while
    // `Client::reply(Exit)` removes only `waiters` (`keeper.rs:569-578`).
    // The fixture therefore subscribes the observer and drives a durable
    // completion before asserting both maps are empty.
    let (host, clock, _peers, mut peer, mut attached) =
        attached_host(vec![kept("call_0", "j_one")], b"");
    let log_home = fakes::TempDir::new("main-keeper-handover-end");
    let log = log::Log::create(log_home.path(), session_id(), clock.clone()).unwrap();
    let _observer = host.observe(&log);
    let adopted = attached.take_jobs().pop().unwrap();
    assert_eq!(adopted.job.job_id.0, "j_one");
    let out_home = fakes::TempDir::new("main-keeper-handover-end-out");
    let output_path = out_home.path().join("j_one.log");
    std::fs::write(&output_path, b"").unwrap();
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(&output_path)
        .unwrap();
    let (end_send, end_recv) = mpsc::channel();
    let emit: Arc<dyn contract::emit::Emit> = Arc::new(fakes::Recorder::default());
    let opened = contract::jobs::Opened {
        started: contract::events::JobStarted {
            job_id: contract::JobId("j_one".into()),
            tool: Some("shell".into()),
            extension: None,
            description: "job j_one".into(),
            output_path: "artifacts/j_one.log".into(),

            pgid: None,
            start_time: None,
            kind: None,
        },
        path: output_path.clone(),
        file,
        end: contract::jobs::End(Box::new(move |completed| match end_send.send(completed) {
            Ok(()) | Err(_) => {}
        })),
        emit,
        lines: None,
    };
    let run = tools::AdoptedRun::new(
        opened,
        adopted.events,
        None,
        contract::jobs::Stop(Box::new(|| {})),
        dyn_clock(&clock),
    );
    let runner = thread::spawn(move || run.run());
    // Production-shaped 8 KiB frames through the real sender; the size is
    // informational, the bound is structural (see #4054).
    let sender = lock(&attached.client.waiters)
        .get("call_0")
        .cloned()
        .unwrap();
    const FRAME: usize = 8 * 1024;
    const FRAMES: usize = 256;
    let mut expected = Vec::with_capacity(FRAME * FRAMES);
    for i in 0..FRAMES {
        let byte = b'a'.wrapping_add(u8::try_from(i % 26).unwrap());
        let bytes = vec![byte; FRAME];
        expected.extend_from_slice(&bytes);
        sender
            .send(HostEvent::Output {
                stream: Stream::Out,
                bytes,
            })
            .unwrap();
    }
    // The ending arrives on the keeper connection so `reply(Exit)` removes
    // the waiter and forwards it to the runner.
    reply(
        &mut peer,
        serde_json::json!({"exit":{"id":"call_0","code":0,"signal":null,"timed_out":false,"output_cap":false}}),
    );
    let dl = fakes::Deadline::after(Duration::from_secs(10));
    let completed = dl.recv_or_fail(&end_recv, "the adopted End");
    assert_eq!(completed.job_id.0, "j_one");
    match runner.join() {
        Ok(_) => {}
        Err(_) => panic!("the runner thread panicked"),
    }
    // The observed durable completion releases the adopted entry.
    let exit = contract::keeper::Exit {
        code: Some(0),
        signal: None,
        timed_out: false,
        output_cap: false,
    };
    log.append(
        &contract::events::Event::JobCompleted(tools::kept_end(
            contract::JobId("j_one".into()),
            &exit,
        )),
        None,
        None,
    )
    .unwrap();
    let actual = std::fs::read(&output_path).unwrap();
    assert_eq!(
        actual.len(),
        expected.len(),
        "the file holds exactly the sent bytes: actual len {} expected len {}",
        actual.len(),
        expected.len()
    );
    if let Some(offset) = actual.iter().zip(expected.iter()).position(|(a, b)| a != b) {
        let start = offset.saturating_sub(16);
        let end = (offset + 16).min(actual.len());
        panic!(
            "the file holds exactly the sent bytes: first difference at offset {} (len {}): actual[{start}..{end}] = {:02x?}, expected[{start}..{end}] = {:02x?}",
            offset,
            actual.len(),
            &actual[start..end],
            &expected[start..end]
        );
    }
    assert!(
        lock(&attached.client.waiters).get("call_0").is_none(),
        "waiters held after End: {:?}",
        lock(&attached.client.waiters)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    );
    assert!(
        !lock(&attached.client.adopted).contains_key(&contract::JobId("j_one".into())),
        "adopted held after the observed completion: {:?}",
        lock(&attached.client.adopted)
            .iter()
            .map(|(job, id)| (job.0.clone(), id.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_stalled_job_never_wedges_another_start() {
    let (host, _, peers, _) = fixture(false);
    let (_, first, _) = start(&host, "slow");
    let mut peer = deadline().recv(&peers).unwrap();
    assert_eq!(line(&peer)["start"]["id"], "slow");
    started(&mut peer, "slow");
    assert!(deadline().recv(&first).unwrap().is_ok());
    // Stall the reader past the queue without draining: every later frame
    // waits, while the map's lock stays free.
    for _ in 0..=contract::keeper::HOST_EVENT_QUEUE {
        reply(
            &mut peer,
            serde_json::json!({"output":{"id":"slow","stream":"out","bytes":"YQ=="}}),
        );
    }
    // Admission never waits on the stalled job: its start line goes out at
    // once, even though its answer waits for the reader to catch up.
    let (_, _second, _) = start(&host, "next");
    assert_eq!(line(&peer)["start"]["id"], "next");
}
