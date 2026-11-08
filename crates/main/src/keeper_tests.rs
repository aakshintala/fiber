//! Session-side keeper lifetime and pending-start retirement.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use super::*;
use contract::keeper::{HostStdin, Stream};
use fakes::clock::FakeClock;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    (Host::new(spawner, clock.clone()), clock, peers, count)
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
    let (events, output) = mpsc::channel();
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
    let (events, _) = mpsc::channel();
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
        // docs/tools.md, "The keeper": a candidate cannot issue arbitrary
        // frames before an authenticated handover, and a Start client never
        // negotiates one.
        assert!(client.reply(frame).is_err());
    }
}
