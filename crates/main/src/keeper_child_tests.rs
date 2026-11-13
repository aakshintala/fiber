use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::events::EventKind;
use contract::keeper::{FromKeeper, HostStdin, Spawn, Stream, ToKeeper};
use contract::{JobId, SessionId};
use fakes::Deadline;

fn runtime(
    home: &std::path::Path,
    out: UnixStream,
    done: mpsc::Sender<std::io::Result<()>>,
) -> Arc<super::Runtime> {
    super::Runtime::new(
        home.to_path_buf(),
        SessionId("s_0123456789abcdef".into()),
        fakes::clock::FakeClock::new(),
        out,
        done,
        super::Hooks::default(),
    )
}

#[test]
fn accept_error_keeps_acceptor_alive() {
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;

    let home = fakes::TempDir::new("keeper-accept-error");
    let socket = home.path().join("keeper.sock");
    let listener = UnixListener::bind(&socket).expect("listener");
    let id = SessionId("s_0123456789abcdef".into());
    let (control, _peer) = UnixStream::pair().expect("control");
    let (done, _) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let accept_error = Box::new(move || {
        ready_tx.send(()).ok()?;
        release_rx.lock().ok()?.recv().ok()?;
        Some(std::io::ErrorKind::ConnectionAborted)
    });
    let clock = fakes::clock::FakeClock::new();
    let runtime = super::Runtime::new(
        home.path().to_path_buf(),
        id.clone(),
        clock.clone(),
        control,
        done,
        super::Hooks {
            accept_error: Some(accept_error),
            ..super::Hooks::default()
        },
    );
    runtime.listen(socket.clone(), listener);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&ready_rx, "injected accept pause");
    let writer = UnixStream::connect(&socket).expect("connect to listener");
    let reader = writer.try_clone().expect("clone reader");
    let (send, frames) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let frame = crate::keeper::read_reply(&mut reader).ok().flatten();
        match send.send(frame) {
            Ok(()) | Err(_) => {}
        }
    });
    let mut peer = Peer { writer, frames };
    say(&mut peer, &ToKeeper::Identify(id));
    release_tx.send(()).expect("release accept error");
    clock
        .advance_when_parked_in(Duration::from_millis(10), Duration::from_secs(10))
        .expect("accept backoff parks before clock advances");
    let identity = hear(&peer, "identity after accept error");
    let listening = super::lock(&runtime.listening);
    listening
        .as_ref()
        .expect("listener state")
        .stop
        .store(true, Ordering::Release);
    drop(listening);
    let _wake = UnixStream::connect(&socket);
    assert!(
        matches!(&identity, Some(FromKeeper::Identity { .. })),
        "expected Identity after accept error, heard {}",
        describe(&identity)
    );
}

#[test]
fn dropping_a_keeper_log_unlinks_its_socket_and_releases_its_lock() {
    let home = fakes::TempDir::new("keeper-socket-release");
    let sessions = home.path().join("projects/p/sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    drop(log::Log::create(&sessions, id.clone(), clock.clone()).unwrap());
    let log = log::Log::open_as(&sessions, id.clone(), clock.clone(), "keeper 4242").unwrap();
    let dir = log.dir().to_path_buf();
    let (socket, _listener) = doors::bind(home.path(), &dir).unwrap();
    let held = super::HeldLog {
        socket: socket.clone(),
        log,
    };
    assert!(socket.exists());
    assert_eq!(log::lock_label(&dir).as_deref(), Some("keeper 4242"));
    // `docs/state.md`, "What each part holds": the writer unlinks its socket at exit.
    // Keep the listener alive: dropping it alone would not unlink the path.
    drop(held);
    assert!(!socket.exists());
    assert_eq!(log::lock_label(&dir), None);
    let _resumed = log::Log::open(&sessions, id, clock).unwrap();
}

#[test]
fn keeper_disconnected_started_reply_does_not_abort_control_drain() {
    // A session can die with a Start already queued. BrokenPipe while
    // answering that Start must not bypass lifeline_eof and abandon its jobs.
    let (client, control) = UnixStream::pair().expect("control");
    client
        .shutdown(std::net::Shutdown::Read)
        .expect("session stopped reading");
    let home = fakes::TempDir::new("keeper-disconnected-reply");
    let (done, _) = mpsc::channel();
    let runtime = runtime(home.path(), control, done);
    let (send, result) = mpsc::channel();
    std::thread::spawn(move || {
        send.send(runtime.send(FromKeeper::Started {
            id: "queued".into(),
            pgid: 4242,
        }))
        .expect("reply result");
    });
    assert!(
        Deadline::after(Duration::from_secs(10))
            .recv_or_fail(&result, "reply to gone session")
            .is_ok()
    );
}

#[test]
fn keeper_start_output_and_exit_are_ordered_and_shutdown_does_not_take_a_log() {
    let home = fakes::TempDir::new("fiber-keeper-child");
    let (life, mut lifeline) = std::io::pipe().expect("lifeline");
    let (client, control) = UnixStream::pair().expect("control");
    let (done, result) = mpsc::channel();
    let path = home.path().to_path_buf();
    std::thread::spawn(move || {
        done.send(super::serve(
            path,
            SessionId("s_0123456789abcdef".into()),
            Arc::new(fakes::clock::SystemClock),
            life,
            control,
        ))
        .expect("result");
    });
    let mut writer = client.try_clone().expect("writer");
    let (lines, read) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(client).lines() {
            let Ok(line) = line else { break };
            let parsed: FromKeeper = serde_json::from_str(&line).expect("keeper line");
            if lines.send(parsed).is_err() {
                break;
            }
        }
    });
    let spec = Spawn {
        id: "call_one".into(),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "printf output".into()],
        cwd: std::env::temp_dir(),
        env: Vec::new(),
        stdin: HostStdin::Null,
        timeout_ms: None,
        output_cap: 1024,
        sandbox: None,
    };
    serde_json::to_writer(&mut writer, &ToKeeper::Start(spec)).expect("start");
    writer.write_all(b"\n").expect("newline");
    let deadline = Deadline::after(Duration::from_secs(10));
    assert!(matches!(
        deadline.recv_or_fail(&read, "started"),
        FromKeeper::Started { .. }
    ));
    let mut output = Vec::new();
    loop {
        match deadline.recv_or_fail(&read, "output then exit") {
            FromKeeper::Output { bytes, .. } => {
                use base64::Engine as _;
                output.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(bytes)
                        .expect("base64"),
                );
            }
            FromKeeper::Exit { exit, .. } => {
                assert_eq!(exit.code, Some(0));
                break;
            }
            FromKeeper::Started { .. }
            | FromKeeper::Failed { .. }
            | FromKeeper::Identity { .. }
            | FromKeeper::Handover { .. }
            | FromKeeper::AdoptDone(_) => panic!("unexpected reply"),
        }
    }
    assert_eq!(output, b"output");
    writer
        .write_all(b"{\"shutdown\":true}\n")
        .expect("shutdown");
    writer
        .shutdown(std::net::Shutdown::Write)
        .expect("control eof");
    lifeline.write_all(b"").expect("lifeline open");
    drop(lifeline);
    assert!(
        Deadline::after(Duration::from_secs(10))
            .recv_or_fail(&result, "keeper exit")
            .is_ok()
    );
    assert!(!home.path().join("run").exists());
}

struct Keeper {
    home: Arc<fakes::TempDir>,
    sessions: std::path::PathBuf,
    id: SessionId,
    writer: UnixStream,
    lifeline: Option<std::io::PipeWriter>,
    read: mpsc::Receiver<FromKeeper>,
    result: mpsc::Receiver<std::io::Result<()>>,
}

fn serve_keeper(name: &str, id: SessionId, clock: Arc<fakes::clock::FakeClock>) -> Keeper {
    serve_keeper_with(name, id, clock, super::Hooks::default(), None).0
}

/// Serves a keeper, in a new home or in `shared`'s, and hands back its
/// runtime so a test can hold the same locks the keeper's threads take.
fn serve_keeper_with(
    name: &str,
    id: SessionId,
    clock: Arc<fakes::clock::FakeClock>,
    hooks: super::Hooks,
    shared: Option<&Keeper>,
) -> (Keeper, Arc<super::Runtime>) {
    // The takeover binds the session socket under this home: root it under
    // a short parent so the socket path fits SUN_LEN on macOS
    // (SOCKET_PATH_MAX is 103 there). A system temp dir such as the macOS
    // TMPDIR is long enough that only the longest prefix fails, which reads
    // as a missing bound watcher with a leaked job.
    let home = match shared {
        Some(keeper) => Arc::clone(&keeper.home),
        None => Arc::new(fakes::TempDir::under(std::path::Path::new("/tmp"), name)),
    };
    let sessions = home.path().join("projects/p/sessions");
    if shared.is_none() {
        drop(log::Log::create(&sessions, id.clone(), clock.clone()).expect("session log"));
    }
    let (life, lifeline) = std::io::pipe().expect("lifeline");
    let (client, control) = UnixStream::pair().expect("control");
    let (lines, read) = mpsc::channel();
    std::thread::spawn({
        let reader = client.try_clone().expect("reader");
        move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                let parsed: FromKeeper = serde_json::from_str(&line).expect("keeper line");
                if lines.send(parsed).is_err() {
                    break;
                }
            }
        }
    });
    let (done, ending) = mpsc::channel();
    let (finished, result) = mpsc::channel();
    let keeper_clock: Arc<dyn contract::clock::Clock> = clock;
    let runtime = super::Runtime::new(
        home.path().to_path_buf(),
        id.clone(),
        keeper_clock,
        control.try_clone().expect("out"),
        done,
        hooks,
    );
    let driver = Arc::clone(&runtime);
    std::thread::spawn(move || {
        finished
            .send(super::drive(&driver, life, control, &ending))
            .expect("keeper result");
    });
    (
        Keeper {
            home,
            sessions,
            id,
            writer: client,
            lifeline: Some(lifeline),
            read,
            result,
        },
        runtime,
    )
}

fn start_shell(keeper: &Keeper, call: &str, shell: &str) -> u32 {
    let spec = Spawn {
        id: call.into(),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), shell.into()],
        cwd: std::env::temp_dir(),
        env: Vec::new(),
        stdin: HostStdin::Null,
        timeout_ms: None,
        output_cap: 1024,
        sandbox: None,
    };
    let mut writer = keeper.writer.try_clone().expect("writer");
    serde_json::to_writer(&mut writer, &ToKeeper::Start(spec)).expect("start");
    writer.write_all(b"\n").expect("newline");
    match Deadline::after(Duration::from_secs(10)).recv_or_fail(&keeper.read, "started") {
        FromKeeper::Started { pgid, .. } => pgid,
        FromKeeper::Failed { .. }
        | FromKeeper::Output { .. }
        | FromKeeper::Exit { .. }
        | FromKeeper::Identity { .. }
        | FromKeeper::Handover { .. }
        | FromKeeper::AdoptDone(_) => {
            panic!("expected started")
        }
    }
}

fn await_shell_ready(keeper: &Keeper, marker: &str) {
    // The child prints the marker after installing its trap; one deadline
    // covers every line of this single wait.
    let deadline = Deadline::after(Duration::from_secs(10));
    loop {
        match deadline.recv_or_fail(&keeper.read, "shell ready") {
            FromKeeper::Output { bytes, .. } => {
                use base64::Engine as _;
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(bytes)
                    .expect("base64");
                if String::from_utf8_lossy(&decoded).contains(marker) {
                    return;
                }
            }
            FromKeeper::Started { .. }
            | FromKeeper::Failed { .. }
            | FromKeeper::Exit { .. }
            | FromKeeper::Identity { .. }
            | FromKeeper::Handover { .. }
            | FromKeeper::AdoptDone(_) => {}
        }
    }
}

fn adopt_call(keeper: &Keeper, call: &str, job: &str) {
    adopt_with(keeper, call, job, None, false);
}

/// Adopts `call` as `job`, with the stderr file and monitor intent given.
fn adopt_with(
    keeper: &Keeper,
    call: &str,
    job: &str,
    errors: Option<&std::path::Path>,
    lines: bool,
) {
    let out = keeper.home.path().join(format!("{call}.out"));
    std::fs::write(&out, b"").expect("output file");
    if let Some(errors) = errors {
        std::fs::write(errors, b"").expect("errors file");
    }
    let mut writer = keeper.writer.try_clone().expect("writer");
    serde_json::to_writer(
        &mut writer,
        &ToKeeper::Adopt {
            id: call.into(),
            job_id: JobId(job.into()),
            output_path: out,
            errors_path: errors.map(std::path::Path::to_path_buf),
            lines,
        },
    )
    .expect("adopt");
    writer.write_all(b"\n").expect("newline");
}

fn crash(keeper: &mut Keeper) {
    keeper
        .writer
        .shutdown(std::net::Shutdown::Write)
        .expect("control eof");
    drop(keeper.lifeline.take());
}

fn session_dir(keeper: &Keeper) -> std::path::PathBuf {
    let (_, dir) = log::find_session(keeper.home.path(), &keeper.id).expect("session dir");
    dir
}

fn crashed_socket(keeper: &Keeper) -> std::path::PathBuf {
    let dir = session_dir(keeper);
    keeper
        .home
        .path()
        .join("run")
        .join(dir.file_name().expect("session dir name"))
}

/// The crashed socket accepts once the takeover binds it: the bind is the
/// signal, so connect retries until the deadline instead of sleeping.
fn await_crashed_socket(path: &std::path::Path) -> UnixStream {
    let deadline = Deadline::after(Duration::from_secs(10));
    loop {
        if let Ok(stream) = UnixStream::connect(path) {
            return stream;
        }
        if deadline.left().is_zero() {
            panic!("waited for the keeper to bind {}", path.display());
        }
        std::thread::yield_now();
    }
}

#[test]
fn keeper_crashed_socket_stop_ends_the_job_and_exits() {
    // `docs/invocation.md`, "Commands and flags": the frozen stop line on a
    // crashed session's socket ends every job as a shutdown does, then the
    // keeper exits, unlinking the socket and releasing the lock.
    let id = SessionId("s_4123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-crash-stop", id, clock.clone());
    let pgid = start_shell(&keeper, "sleepy", "sleep 60");
    adopt_call(&keeper, "sleepy", "j_sleepy");
    crash(&mut keeper);
    let socket = crashed_socket(&keeper);
    let mut stop = await_crashed_socket(&socket);
    // The exact frozen bytes (`docs/invocation.md`, "Commands and flags").
    stop.write_all(b"{\"shutdown\":true}\n")
        .expect("frozen stop");
    // The job's reap is the end signal: the stop landed shutdown-style.
    let deadline = Deadline::after(Duration::from_secs(10));
    loop {
        match deadline.recv_or_fail(&keeper.read, "job end") {
            FromKeeper::Exit { exit, .. } => {
                assert_eq!(
                    exit.signal,
                    Some(signal_hook::consts::SIGTERM),
                    "stop signals first"
                );
                break;
            }
            FromKeeper::Output { .. } | FromKeeper::Started { .. } => {}
            FromKeeper::Failed { .. }
            | FromKeeper::Identity { .. }
            | FromKeeper::Handover { .. }
            | FromKeeper::AdoptDone(_) => panic!("expected job end"),
        }
    }
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "keeper exit")
        .expect("keeper exits on the frozen stop");
    assert!(!support::group::alive(pgid), "the stopped job is reaped");
    assert!(!socket.exists(), "the socket is unlinked on exit");
    let _resumed =
        log::Log::open(&keeper.sessions, keeper.id.clone(), clock.clone()).expect("lock released");
}

#[test]
fn keeper_crashed_socket_drops_other_lines_and_keeps_running() {
    // Every other line is dropped unread, so a probe's read sees EOF
    // exactly as before; only the frozen bytes stop the keeper. The wrong
    // boolean is the closest non-match: matching it would stop the keeper.
    let id = SessionId("s_5123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-crash-drop", id, clock.clone());
    let pgid = start_shell(&keeper, "sleepy", "sleep 60");
    let watchdog = fakes::Watchdog::group(pgid);
    adopt_call(&keeper, "sleepy", "j_sleepy");
    crash(&mut keeper);
    let socket = crashed_socket(&keeper);
    await_crashed_socket(&socket);
    let probes: Vec<Vec<u8>> = vec![
        b"{\"shutdown\":false}\n".to_vec(),
        b"{\"id\":\"c_1\",\"command\":\"close\",\"args\":{\"now\":true}}\n".to_vec(),
        b"{\"nope\":1}\n".to_vec(),
        b"{\"shut".to_vec(),
    ];
    for probe in probes {
        let mut peer = await_crashed_socket(&socket);
        peer.write_all(&probe).expect("probe line");
        if !probe.ends_with(b"\n") {
            peer.shutdown(std::net::Shutdown::Write).expect("probe eof");
        }
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        let mut eof = Vec::new();
        peer.read_to_end(&mut eof).expect("probe read");
        assert!(
            eof.is_empty(),
            "a dropped line reads as EOF, got {} bytes",
            eof.len()
        );
        assert!(
            keeper.result.try_recv().is_err(),
            "the keeper still runs after a dropped line"
        );
        assert!(
            support::group::alive(pgid),
            "the job still runs after a dropped line"
        );
        assert!(socket.exists(), "the socket stays bound");
    }
    let mut stop = await_crashed_socket(&socket);
    stop.write_all(b"{\"shutdown\":true}\n")
        .expect("frozen stop");
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "keeper exit")
        .expect("keeper exits after dropped-line probes");
    // A single probe races a transient child just after the keeper exits.
    assert_test_group_empty(pgid, watchdog);
}

#[test]
fn keeper_crashed_socket_drops_an_overlong_line_and_keeps_running() {
    // The first read is one frame capped at the wire cap, so past-cap
    // bytes with no newline drop the connection without parsing, as a
    // torn tail does (`docs/invocation.md`, "Commands and flags").
    // Past the cap, held open: without the cap the read would wait past
    // the deadline for a newline.
    let id = SessionId("s_6123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-crash-overlong", id, clock.clone());
    let pgid = start_shell(&keeper, "sleepy", "sleep 60");
    let watchdog = fakes::Watchdog::group(pgid);
    adopt_call(&keeper, "sleepy", "j_sleepy");
    crash(&mut keeper);
    let socket = crashed_socket(&keeper);
    let mut peer = await_crashed_socket(&socket);
    peer.write_all(&vec![b'x'; support::framing::MAX_LINE + 8])
        .expect("overlong line");
    // The keeper drops the connection, so the held peer's read ends.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut eof = Vec::new();
        tx.send(peer.read_to_end(&mut eof).map(|_| eof))
            .unwrap_or(());
    });
    let eof = Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&rx, "the overlong drop")
        .expect("peer read");
    assert!(eof.is_empty(), "a dropped line reads as EOF");
    assert!(
        keeper.result.try_recv().is_err(),
        "the keeper still runs after an overlong line"
    );
    assert!(
        support::group::alive(pgid),
        "the job still runs after an overlong line"
    );
    // A later frozen stop on a fresh connection still stops it.
    let mut stop = await_crashed_socket(&socket);
    stop.write_all(b"{\"shutdown\":true}\n")
        .expect("frozen stop");
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "the keeper exit")
        .expect("keeper exits on the frozen stop");
    assert!(!socket.exists(), "the socket is unlinked on exit");
    // A single probe races a transient child just after the keeper exits.
    assert_test_group_empty(pgid, watchdog);
}

fn assert_test_group_empty(pgid: u32, watchdog: fakes::Watchdog) {
    assert!(
        fakes::group_empties(pgid, Deadline::after(Duration::from_secs(10))),
        "the test reaps its shell job"
    );
    watchdog.stand_down(Deadline::after(Duration::from_secs(10)));
}

fn job_completed_lines(keeper: &Keeper) -> Vec<contract::Envelope> {
    log::read(&session_dir(keeper))
        .expect("log lines")
        .into_iter()
        .filter(|line| line.kind() == EventKind::JobCompleted)
        .collect()
}

#[test]
fn keeper_bound_fires_two_hours_after_the_crash_and_writes_keeper_timeout() {
    let id = SessionId("s_0123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-bound-timeout", id, clock.clone());
    let pgid = start_shell(&keeper, "sleepy", "sleep 60");
    adopt_call(&keeper, "sleepy", "j_sleepy");
    crash(&mut keeper);

    // No hook is set, so the watcher parks exactly one production bound past the crash.
    // `docs/tools.md`, "The keeper": the keeper waits 2 hours.
    assert_eq!(
        super::KEEPER_BOUND,
        Duration::from_secs(7_200),
        "production bound is 2 hours"
    );
    let bound_at = clock.origin() + super::KEEPER_BOUND;
    let parked = clock
        .mark_parked(bound_at, Deadline::after(Duration::from_secs(10)))
        .expect("the watcher parks at the bound");
    // No park count is promised: name the watcher thread and drive it directly.
    let watcher = parked.threads().into_iter().next().expect("watcher thread");

    // Halfway there a wake must not fire the bound: the watcher re-parks.
    clock.advance_when_parked(
        Some(bound_at),
        Some(watcher),
        super::KEEPER_BOUND / 2,
        &Deadline::after(Duration::from_secs(10)),
    );
    let parked = clock
        .mark_parked(bound_at, Deadline::after(Duration::from_secs(10)))
        .expect("the watcher re-parks at the bound");
    assert_eq!(parked.threads(), vec![watcher]);
    // Nothing fired: the watcher's confirmed re-park above proves the bound
    // did not fire, so the exit channel must still be empty.
    assert!(
        keeper.result.try_recv().is_err(),
        "the keeper still runs before the bound"
    );
    assert!(
        support::group::alive(pgid),
        "the job still runs before the bound"
    );
    assert!(
        job_completed_lines(&keeper).is_empty(),
        "no ending is written before the bound"
    );

    // At the bound the stop lands, the reap writes the timeout, and the keeper exits.
    clock.advance_when_parked(
        Some(bound_at),
        Some(watcher),
        super::KEEPER_BOUND / 2,
        &Deadline::after(Duration::from_secs(10)),
    );
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "keeper exit")
        .expect("keeper exits cleanly");
    let completed = job_completed_lines(&keeper);
    // `docs/tools.md`, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout.
    assert_eq!(completed.len(), 1, "one ending for the one held job");
    let payload = &completed[0].payload;
    assert_eq!(payload["job_id"], "j_sleepy");
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["error"]["code"], "keeper_timeout");
    assert_eq!(
        payload["error"]["message"],
        "The session stayed gone for 2 hours after a crash, so its keeper stopped the job."
    );
    assert_eq!(payload["process"]["signal"], "SIGTERM");
    assert_eq!(payload["process"]["timed_out"], false);
    assert!(
        payload.get("output_tail").is_none(),
        "no tail without hooks"
    );
    assert!(payload.get("notice").is_none(), "no notice without hooks");
    assert!(payload.get("changed_by").is_none(), "no hook editors");
    assert!(
        payload.get("protected_paths").is_none(),
        "no restore happened"
    );
    let dir = session_dir(&keeper);
    let socket = keeper
        .home
        .path()
        .join("run")
        .join(dir.file_name().expect("session dir name"));
    assert!(!socket.exists(), "the socket is unlinked on exit");
    let _resumed =
        log::Log::open(&keeper.sessions, keeper.id.clone(), clock.clone()).expect("lock released");
}

#[test]
fn keeper_bound_ignores_term_and_completes_through_escalation() {
    let id = SessionId("s_1123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-bound-escalation", id, clock.clone());
    let pgid = start_shell(
        &keeper,
        "stubborn",
        "trap '' TERM; echo ready; while true; do sleep 1; done",
    );
    // The child prints readiness only after installing its trap, so the
    // bound's TERM cannot land before the trap exists.
    await_shell_ready(&keeper, "ready");
    adopt_call(&keeper, "stubborn", "j_stubborn");
    crash(&mut keeper);

    let bound_at = clock.origin() + super::KEEPER_BOUND;
    let parked = clock
        .mark_parked(bound_at, Deadline::after(Duration::from_secs(10)))
        .expect("the watcher parks at the bound");
    let watcher = parked.threads().into_iter().next().expect("watcher thread");
    clock.advance_when_parked(
        Some(bound_at),
        Some(watcher),
        super::KEEPER_BOUND,
        &Deadline::after(Duration::from_secs(10)),
    );
    // TERM cannot end the loop: the escalation timer's confirmed park proves
    // the kill has not fired, so nothing is written yet.
    let kill_at = bound_at + support::group::GRACE;
    let parked = clock
        .mark_parked(kill_at, Deadline::after(Duration::from_secs(10)))
        .expect("the timer parks at the kill");
    // No park count is promised: name the timer thread and drive it directly.
    let timer = parked.threads().into_iter().next().expect("timer thread");
    assert!(
        keeper.result.try_recv().is_err(),
        "the keeper still runs after an ignored stop"
    );
    assert!(support::group::alive(pgid), "the job survives its stop");
    assert!(
        job_completed_lines(&keeper).is_empty(),
        "no ending is written before the reap"
    );
    // Past the grace the timer escalates to KILL: the reap writes the timeout.
    clock.advance_when_parked(
        Some(kill_at),
        Some(timer),
        support::group::GRACE,
        &Deadline::after(Duration::from_secs(10)),
    );
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "keeper exit")
        .expect("keeper exits cleanly");
    let completed = job_completed_lines(&keeper);
    // `docs/tools.md`, "The keeper": writes each one's job_completed as failed
    // with code keeper_timeout.
    assert_eq!(completed.len(), 1, "one ending for the one held job");
    assert_eq!(completed[0].payload["error"]["code"], "keeper_timeout");
    assert_eq!(completed[0].payload["process"]["signal"], "SIGKILL");
}

#[test]
fn keeper_prewound_reap_keeps_the_normal_completion() {
    let id = SessionId("s_2123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-bound-prewound", id, clock.clone());
    start_shell(&keeper, "quick", "exit 3");
    adopt_call(&keeper, "quick", "j_quick");
    // The process ends while the session still lives: the exit reply is the reap.
    let deadline = Deadline::after(Duration::from_secs(10));
    loop {
        match deadline.recv_or_fail(&keeper.read, "exit") {
            FromKeeper::Exit { exit, .. } => {
                assert_eq!(exit.code, Some(3));
                break;
            }
            FromKeeper::Output { .. } | FromKeeper::Started { .. } => {}
            FromKeeper::Failed { .. }
            | FromKeeper::Identity { .. }
            | FromKeeper::Handover { .. }
            | FromKeeper::AdoptDone(_) => panic!("expected exit"),
        }
    }
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "keeper exit")
        .expect("keeper exits cleanly");
    let completed = job_completed_lines(&keeper);
    // `docs/tools.md`, "The keeper": a job_completed for each job that ends.
    assert_eq!(completed.len(), 1, "one ending for the one held job");
    let expected = tools::kept_end(
        JobId("j_quick".into()),
        &contract::keeper::Exit {
            code: Some(3),
            signal: None,
            timed_out: false,
            output_cap: false,
        },
    );
    assert_eq!(
        completed[0].payload,
        serde_json::to_value(expected)
            .expect("completion value")
            .as_object()
            .expect("completion payload")
            .clone()
    );
}

#[test]
fn keeper_no_job_crash_spawns_no_watcher() {
    let id = SessionId("s_3123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-bound-no-job", id, clock.clone());
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "keeper exit")
        .expect("keeper exits cleanly");
    let bound_at = clock.origin() + super::KEEPER_BOUND;
    // The keeper's exit above proves registration closed: serve spawns the
    // watcher before returning its result, so no park can arrive later.
    assert!(
        !clock.parked().contains(&Some(bound_at)),
        "no watcher parks at the bound"
    );
}

#[test]
fn takeover_start_marker_failure_carries_on_and_poisons() {
    // RULING-4628-8's test as unblocked by RULING-4628-9: fail the
    // start-marker append for real at the open fd, then check the keeper
    // carries on past it and the next durable append reports the stopped log.
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let hooks = super::Hooks {
        held: Some(held),
        before_start_append: Some(Box::new(|held: &log::Log| held.break_writes())),
        ..super::Hooks::default()
    };
    let (mut keeper, runtime) = serve_keeper_with(
        "keeper-marker-poison",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    // The takeover carries on past the failed marker append: the held hook
    // fires and the driver thread reports no error.
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the keeper to hold the log");
    assert!(
        keeper.result.try_recv().is_err(),
        "the keeper carries on after a failed marker append"
    );
    // The failed marker write stopped the log, so the next durable append
    // reports the poison instead of writing.
    let append = super::lock(&runtime.log);
    let held_log = append.as_ref().expect("the keeper holds the log");
    let failed = held_log.log.append(
        &contract::events::Event::StepStarted(contract::events::Empty {}),
        None,
        None,
    );
    assert!(
        matches!(failed, Err(log::Error::Poisoned { .. })),
        "a stopped log reports Poisoned, got {failed:?}"
    );
}

#[test]
fn takeover_appends_started_then_bound() {
    // `docs/events.md`, "Jobs" and `docs/tools.md`, "The keeper" promise the
    // takeover order: the start record lands before the bound record.
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let hooks = super::Hooks {
        held: Some(held),
        ..super::Hooks::default()
    };
    let (mut keeper, _runtime) = serve_keeper_with(
        "keeper-takeover-order",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the keeper to hold the log");
    let lines = log::read(&session_dir(&keeper)).expect("log lines");
    let mut seen_started = false;
    let mut bound_after_started = false;
    for line in &lines {
        if line.kind() == EventKind::KeeperStarted {
            seen_started = true;
        } else if seen_started && line.kind() == EventKind::KeeperBound {
            bound_after_started = true;
        }
    }
    assert!(
        seen_started && bound_after_started,
        "the takeover appends keeper_started before keeper_bound: {:?}",
        lines.iter().map(|line| line.kind()).collect::<Vec<_>>()
    );
}

#[test]
fn takeover_window_shows_started_without_bound() {
    // A keeper dead between the start append and `bind` leaves the start
    // record with no bound record: the barrier lands the takeover in that
    // window, like the held-keeper tests land theirs.
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let (before, gate) = barrier();
    let hooks = super::Hooks {
        held: Some(held),
        before_bind: Some(before),
        ..super::Hooks::default()
    };
    let (mut keeper, _runtime) = serve_keeper_with(
        "keeper-died-before-bind",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    gate.armed.store(true, Ordering::SeqCst);
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&gate.arrived, "the takeover to reach bind");
    let lines = log::read(&session_dir(&keeper)).expect("log lines");
    let kinds: Vec<EventKind> = lines.iter().map(|line| line.kind()).collect();
    assert!(
        kinds.contains(&EventKind::KeeperStarted),
        "the window shows the start record: {kinds:?}"
    );
    assert!(
        !kinds.contains(&EventKind::KeeperBound),
        "the window shows no bound record: {kinds:?}"
    );
    gate.go.send(()).expect("release the takeover");
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the keeper to hold the log");
}

#[test]
fn keeper_control_ignores_torn_tail_and_proceeds_to_takeover() {
    // The session can die mid-`send` (body and framing `\n` are separate
    // syscalls), leaving a final chunk with no newline. That torn tail proves
    // the peer died mid-write, exactly when the keeper must take over: it is
    // ignored, never parsed. Observed tails: a 1-byte `{` and a ~715-byte
    // prefix of a `Start` line.
    let spec = Spawn {
        id: "call_one".into(),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "x".repeat(600)],
        cwd: std::env::temp_dir(),
        env: Vec::new(),
        stdin: HostStdin::Null,
        timeout_ms: None,
        output_cap: 1024,
        sandbox: None,
    };
    let full = serde_json::to_vec(&ToKeeper::Start(spec)).expect("start line");
    assert!(full.len() > 715, "prefix fixture must exceed 715 bytes");
    let tails: Vec<Vec<u8>> = vec![b"{".to_vec(), full[..715].to_vec()];
    for tail in tails {
        let (mut writer, reader) = UnixStream::pair().expect("control");
        writer.write_all(&tail).expect("torn tail");
        writer
            .shutdown(std::net::Shutdown::Write)
            .expect("control eof");
        let home = fakes::TempDir::new("keeper-torn-tail");
        let (_out_client, out) = UnixStream::pair().expect("out");
        let (done, _) = mpsc::channel();
        let runtime = runtime(home.path(), out, done);
        assert!(
            runtime.control(reader).is_ok(),
            "torn tail of {} bytes must not fail control",
            tail.len()
        );
    }
}

// ---- the handover (`docs/tools.md`, "The keeper": a resume takes the log back) ----

/// A candidate on the keeper's socket: the frames it hears, `None` at its end.
struct Peer {
    writer: UnixStream,
    frames: mpsc::Receiver<Option<FromKeeper>>,
}

fn socket_of(keeper: &Keeper) -> std::path::PathBuf {
    let (_, dir) = log::find_session(keeper.home.path(), &keeper.id).expect("session dir");
    keeper
        .home
        .path()
        .join("run")
        .join(dir.file_name().expect("session dir name"))
}

fn connect(keeper: &Keeper) -> Peer {
    let writer = UnixStream::connect(socket_of(keeper)).expect("connect to the keeper");
    let reader = writer.try_clone().expect("reader");
    let (send, frames) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            match crate::keeper::read_reply(&mut reader) {
                Ok(Some(frame)) => {
                    if send.send(Some(frame)).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => {
                    // A closed channel only means the test stopped listening.
                    match send.send(None) {
                        Ok(()) | Err(_) => {}
                    }
                    break;
                }
            }
        }
    });
    Peer { writer, frames }
}

fn say(peer: &mut Peer, request: &ToKeeper) {
    serde_json::to_writer(&mut peer.writer, request).expect("request");
    peer.writer.write_all(b"\n").expect("newline");
}

fn hear(peer: &Peer, what: &str) -> Option<FromKeeper> {
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&peer.frames, what)
}

fn ready_signal() -> (Box<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
    let (send, received) = mpsc::channel();
    (
        Box::new(move || match send.send(()) {
            Ok(()) | Err(_) => {}
        }),
        received,
    )
}

/// Kills a job's process group when the test ends, whatever its result.
struct Reaper(u32);
impl Drop for Reaper {
    fn drop(&mut self) {
        match support::group::signal(self.0, rustix::process::Signal::KILL) {
            Ok(()) | Err(_) => {}
        }
    }
}

fn identify(peer: &mut Peer, id: &SessionId) {
    say(peer, &ToKeeper::Identify(id.clone()));
    match hear(peer, "identity") {
        Some(FromKeeper::Identity { session_id }) => assert_eq!(&session_id, id),
        other => panic!("expected the keeper's identity, heard {}", describe(&other)),
    }
}

fn describe(frame: &Option<FromKeeper>) -> String {
    match frame {
        Some(frame) => serde_json::to_string(frame).expect("frame"),
        None => "the connection end".into(),
    }
}

fn handover(peer: &mut Peer) -> Vec<contract::keeper::KeptJob> {
    say(peer, &ToKeeper::Handover(true));
    match hear(peer, "handover") {
        Some(FromKeeper::Handover { running }) => running,
        other => panic!("expected the handover, heard {}", describe(&other)),
    }
}

fn lock_holder(keeper: &Keeper) -> Option<String> {
    log::lock_label(&session_dir(keeper))
}

/// A crashed keeper that holds a job and the log, ready for a candidate.
fn held_keeper(
    name: &str,
    shell: &str,
) -> (
    Keeper,
    Arc<super::Runtime>,
    Reaper,
    Arc<fakes::clock::FakeClock>,
) {
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let hooks = super::Hooks {
        held: Some(held),
        ..super::Hooks::default()
    };
    let (mut keeper, runtime) = serve_keeper_with(
        name,
        SessionId("s_0123456789abcdef".into()),
        clock.clone(),
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", shell);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the keeper to hold the log");
    (keeper, runtime, Reaper(pgid), clock)
}

#[test]
fn an_identity_only_probe_cannot_release_the_log() {
    let (keeper, _runtime, _reaper, _) = held_keeper("keeper-probe", "sleep 60");
    let id = keeper.id.clone();
    let mut probe = connect(&keeper);
    identify(&mut probe, &id);
    drop(probe);
    // `docs/events.md`, "Writing": a probe that only asks who holds the log
    // takes nothing: the lock and the socket stay with the keeper.
    let mut again = connect(&keeper);
    identify(&mut again, &id);
    assert!(lock_holder(&keeper).is_some_and(|label| label.starts_with("keeper ")));
    assert!(socket_of(&keeper).exists());
}

#[test]
fn mismatched_identity_keeps_lock() {
    let (keeper, _runtime, _reaper, _) = held_keeper("keeper-mismatch", "sleep 60");
    let mut wrong = connect(&keeper);
    say(
        &mut wrong,
        &ToKeeper::Identify(SessionId("s_ffffffffffffffff".into())),
    );
    // The keeper answers a session it does not hold with nothing but the end
    // of the connection, so no identity can be mistaken for a match.
    assert_eq!(hear(&wrong, "the rejection").map(|_| ()), None);
    assert!(lock_holder(&keeper).is_some_and(|label| label.starts_with("keeper ")));
    assert!(socket_of(&keeper).exists());
    assert!(matches!(
        log::Log::open(
            &keeper.sessions,
            keeper.id.clone(),
            fakes::clock::FakeClock::new()
        ),
        Err(log::Error::Held { .. })
    ));
}

#[test]
fn a_handover_request_before_the_identity_is_rejected() {
    let (keeper, _runtime, _reaper, _) = held_keeper("keeper-unidentified", "sleep 60");
    let mut peer = connect(&keeper);
    say(&mut peer, &ToKeeper::Handover(true));
    assert_eq!(hear(&peer, "the rejection").map(|_| ()), None);
    assert!(lock_holder(&keeper).is_some());
}

#[test]
fn malformed_oversized_and_torn_candidates_leave_the_keeper_alone() {
    let (keeper, _runtime, _reaper, _) = held_keeper("keeper-malformed", "sleep 60");
    let id = keeper.id.clone();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not json", b"not json\n".to_vec()),
        (
            "an ordinary start",
            br#"{"stop":"job"}"#.to_vec().into_iter().chain(*b"\n").collect(),
        ),
        ("a false handover", b"{\"handover\":false}\n".to_vec()),
        ("a torn line", b"{\"identify\":\"s_0123".to_vec()),
        // A complete request with no line feed is a torn tail: never parsed.
        (
            "a request cut before its line feed",
            format!("{{\"identify\":\"{}\"}}", id.0).into_bytes(),
        ),
    ];
    for (name, bytes) in cases {
        let mut peer = connect(&keeper);
        peer.writer.write_all(&bytes).expect(name);
        peer.writer
            .shutdown(std::net::Shutdown::Write)
            .expect("end of the request");
        assert_eq!(hear(&peer, name).map(|_| ()), None, "{name}");
    }
    // A line past the wire's cap is refused without being parsed or kept.
    let flood = connect(&keeper);
    let writer = flood.writer.try_clone().expect("flood writer");
    let sender = std::thread::spawn(move || {
        let mut writer = writer;
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..(support::framing::MAX_LINE >> 20) + 2 {
            if writer.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    assert_eq!(hear(&flood, "the oversize rejection").map(|_| ()), None);
    // The keeper already refused the oversized line and closed its end, so
    // on macOS this shutdown fails with `NotConnected` (errno 57); any
    // other error is still a failure.
    match flood.writer.shutdown(std::net::Shutdown::Both) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
        Err(error) => panic!("end of the flood: {error:?}"),
    }
    assert!(sender.join().is_ok());
    // The keeper still holds the log and still answers a real candidate.
    assert!(lock_holder(&keeper).is_some_and(|label| label.starts_with("keeper ")));
    let mut real = connect(&keeper);
    identify(&mut real, &id);
}

#[test]
fn a_handover_releases_the_log_and_the_socket_and_names_the_jobs() {
    let (keeper, _runtime, _reaper, _) = held_keeper("keeper-handover", "sleep 60");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    let running = handover(&mut peer);
    // `docs/tools.md`, "The keeper": the keeper unlinks the socket, releases
    // the lock and acknowledges, so the ack is the proof the lock is free.
    assert!(!socket_of(&keeper).exists());
    assert_eq!(lock_holder(&keeper), None);
    let session = log::Log::open(&keeper.sessions, id, fakes::clock::FakeClock::new())
        .expect("the resumed session takes the lock");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].id, "job");
    assert_eq!(running[0].job_id, JobId("j_job".into()));
    assert_eq!(running[0].output_path, keeper.home.path().join("job.out"));
    assert_eq!(running[0].errors_path, None);
    assert!(!running[0].tty);
    assert!(!running[0].lines);
    // The keeper wrote nothing: the session is the one writer.
    assert!(job_completed_lines(&keeper).is_empty());
    drop(session);
}

#[test]
fn release_waits_behind_a_held_log_mutex() {
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let (registered, registered_rx) = ready_signal();
    // The append ends when `freed` is set; the candidate reports what it
    // read of it on taking the log.
    let freed = Arc::new(AtomicBool::new(false));
    let (acquired_at, acquired_rx) = mpsc::channel();
    let acquired = {
        let freed = Arc::clone(&freed);
        Box::new(
            move || match acquired_at.send(freed.load(Ordering::SeqCst)) {
                Ok(()) | Err(_) => {}
            },
        )
    };
    let hooks = super::Hooks {
        held: Some(held),
        registered: Some(registered),
        acquired: Some(acquired),
        ..super::Hooks::default()
    };
    let (mut keeper, runtime) = serve_keeper_with(
        "keeper-release-order",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the log to be held");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    // An append in progress holds this mutex; the release queues behind it.
    let append = super::lock(&runtime.log);
    say(&mut peer, &ToKeeper::Handover(true));
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&registered_rx, "the candidate");
    // `docs/events.md`, "Writing": a second Fiber process cannot open the
    // session while the keeper still holds it.
    assert!(matches!(
        log::Log::open(&keeper.sessions, id.clone(), fakes::clock::FakeClock::new()),
        Err(log::Error::Held { .. })
    ));
    freed.store(true, Ordering::SeqCst);
    drop(append);
    assert!(
        Deadline::after(Duration::from_secs(10))
            .recv_or_fail(&acquired_rx, "the candidate to take the log"),
        "the candidate took the log while an append held it"
    );
    assert!(matches!(
        hear(&peer, "the acknowledgement"),
        Some(FromKeeper::Handover { running }) if running.len() == 1
    ));
    // Released only by the acknowledgement: now the opener succeeds.
    log::Log::open(&keeper.sessions, id, fakes::clock::FakeClock::new())
        .expect("the lock is free after the acknowledgement");
}

#[test]
fn last_exit_pending_handover_acks_empty() {
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let (registered, registered_rx) = ready_signal();
    let (resume, resumed) = mpsc::channel::<()>();
    let resumed = std::sync::Mutex::new(resumed);
    let hooks = super::Hooks {
        held: Some(held),
        // The worker stops here, after it registered and before it asks
        // for the log mutex, until the test lets it go.
        registered: Some(Box::new(move || {
            registered();
            match super::lock(&resumed).recv() {
                Ok(()) | Err(_) => {}
            }
        })),
        ..super::Hooks::default()
    };
    let (mut keeper, _runtime) = serve_keeper_with(
        "keeper-pending-empty",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the log to be held");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    say(&mut peer, &ToKeeper::Handover(true));
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&registered_rx, "the candidate");
    // The last job ends while the candidate waits: its ending is written,
    // the log released, and the candidate told there is nothing to take.
    support::group::signal(pgid, rustix::process::Signal::TERM).expect("end the job");
    Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&keeper.result, "the keeper to exit")
        .expect("clean exit");
    assert!(matches!(
        hear(&peer, "the empty acknowledgement"),
        Some(FromKeeper::Handover { running }) if running.is_empty()
    ));
    // The durable ending was there before the acknowledgement arrived.
    assert_eq!(job_completed_lines(&keeper).len(), 1);
    assert_eq!(lock_holder(&keeper), None);
    // Let the worker go: it finds its request already answered, and says
    // nothing more.
    resume.send(()).expect("resume the worker");
    assert_eq!(hear(&peer, "the connection end").map(|_| ()), None);
}

fn hang_up(peer: Peer) {
    // The reader thread holds a duplicate of this socket: only a shutdown
    // ends the connection for the keeper.
    peer.writer
        .shutdown(std::net::Shutdown::Both)
        .expect("hang up");
}

fn adopt_and_finish(peer: &mut Peer) {
    say(peer, &ToKeeper::Adopted(true));
    assert!(matches!(
        hear(peer, "adopt_done"),
        Some(FromKeeper::AdoptDone(true))
    ));
}

#[test]
fn the_keeper_writes_files_until_adopted_then_every_byte_has_one_home() {
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let hooks = super::Hooks {
        held: Some(held),
        ..super::Hooks::default()
    };
    let (mut keeper, runtime) = serve_keeper_with(
        "keeper-switch",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let out_fifo = keeper.home.path().join("out.fifo");
    let err_fifo = keeper.home.path().join("err.fifo");
    fakes::fifo(&out_fifo);
    fakes::fifo(&err_fifo);
    // Read-write ends: opening never blocks, and the readers see the end of
    // input only when the test lets go of them.
    let open = |path: &std::path::Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("fifo")
    };
    let (mut out_writer, mut err_writer) = (open(&out_fifo), open(&err_fifo));
    let pgid = start_shell(
        &keeper,
        "job",
        &format!(
            "cat '{}' & cat '{}' >&2 & wait",
            out_fifo.display(),
            err_fifo.display()
        ),
    );
    let _reaper = Reaper(pgid);
    let errors = keeper.home.path().join("job.err");
    adopt_with(&keeper, "job", "j_job", Some(&errors), false);
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the log to be held");
    let output = keeper.home.path().join("job.out");
    let file_is = |path: std::path::PathBuf, text: &'static str| {
        move || std::fs::read(&path).is_ok_and(|bytes| bytes == text.as_bytes())
    };
    out_writer.write_all(b"one\n").expect("one");
    fakes::wait_until(
        "the first bytes in the file",
        Deadline::after(Duration::from_secs(10)),
        file_is(output.clone(), "one\n"),
    );
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    let running = handover(&mut peer);
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].errors_path.as_deref(), Some(errors.as_path()));
    // The session holds the lock, but has not said it is ready: the keeper
    // still writes the files, stdout and stderr apart.
    out_writer.write_all(b"two\n").expect("two");
    err_writer.write_all(b"e1\n").expect("e1");
    fakes::wait_until(
        "the bytes of the preparation window in the files",
        Deadline::after(Duration::from_secs(10)),
        {
            let (output, errors) = (output.clone(), errors.clone());
            move || {
                std::fs::read(&output).is_ok_and(|bytes| bytes == b"one\ntwo\n")
                    && std::fs::read(&errors).is_ok_and(|bytes| bytes == b"e1\n")
            }
        },
    );
    // The switch takes each job's state lock: a byte read while it is held
    // waits for the lock, and lands on one side of the switch or the other.
    let process = super::lock(&runtime.processes)
        .get("job")
        .cloned()
        .expect("the job");
    let state = super::lock(&process.state);
    say(&mut peer, &ToKeeper::Adopted(true));
    out_writer.write_all(b"three\n").expect("three");
    drop(state);
    let mut frames = Vec::new();
    let mut done = false;
    let collect = |peer: &Peer,
                   frames: &mut Vec<FromKeeper>,
                   done: &mut bool,
                   until: &dyn Fn(&[FromKeeper]) -> bool| {
        let deadline = Deadline::after(Duration::from_secs(10));
        while !*done || !until(frames) {
            match deadline.recv_or_fail(&peer.frames, "a frame after the switch") {
                Some(FromKeeper::AdoptDone(true)) => *done = true,
                Some(frame) => frames.push(frame),
                None => panic!("the keeper hung up"),
            }
        }
    };
    collect(&peer, &mut frames, &mut done, &|_| true);
    out_writer.write_all(b"four\n").expect("four");
    err_writer.write_all(b"e2\n").expect("e2");
    let text = |frames: &[FromKeeper], wanted: Stream| -> Vec<u8> {
        frames
            .iter()
            .filter_map(|frame| match frame {
                FromKeeper::Output { stream, bytes, .. } if *stream == wanted => {
                    use base64::Engine as _;
                    Some(
                        base64::engine::general_purpose::STANDARD
                            .decode(bytes)
                            .expect("base64"),
                    )
                }
                FromKeeper::Output { .. }
                | FromKeeper::Started { .. }
                | FromKeeper::Failed { .. }
                | FromKeeper::Exit { .. }
                | FromKeeper::Identity { .. }
                | FromKeeper::Handover { .. }
                | FromKeeper::AdoptDone(_) => None,
            })
            .flatten()
            .collect()
    };
    collect(&peer, &mut frames, &mut done, &|frames| {
        text(frames, Stream::Out).ends_with(b"four\n")
            && text(frames, Stream::Err).ends_with(b"e2\n")
    });
    let mut stdout = std::fs::read(&output).expect("output file");
    stdout.extend(text(&frames, Stream::Out));
    let mut stderr = std::fs::read(&errors).expect("errors file");
    stderr.extend(text(&frames, Stream::Err));
    // Every byte exactly once and in order, whichever side of the switch
    // the contested byte landed on.
    assert_eq!(String::from_utf8_lossy(&stdout), "one\ntwo\nthree\nfour\n");
    assert_eq!(String::from_utf8_lossy(&stderr), "e1\ne2\n");
    assert!(
        std::fs::read(&output)
            .expect("output file")
            .starts_with(b"one\ntwo\n"),
        "the preparation window's bytes are in the file"
    );
    // The ending follows the last of the output.
    drop((out_writer, err_writer));
    let exit = loop {
        match hear(&peer, "the job ending") {
            Some(FromKeeper::Exit { id, exit }) => break (id, exit),
            Some(FromKeeper::Output { .. }) => panic!("output after the last byte"),
            Some(_) => {}
            None => panic!("the keeper hung up"),
        }
    };
    assert_eq!(exit.0, "job");
    assert_eq!(exit.1.code, Some(0));
    assert!(
        job_completed_lines(&keeper).is_empty(),
        "the session writes the ending"
    );
}

#[test]
fn connection_loss_between_release_and_adoption_takes_the_log_back() {
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let hooks = super::Hooks {
        held: Some(held),
        ..super::Hooks::default()
    };
    let (mut keeper, _runtime) = serve_keeper_with(
        "keeper-lost-before-adopted",
        SessionId("s_0123456789abcdef".into()),
        clock,
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the log to be held");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    assert_eq!(handover(&mut peer).len(), 1);
    assert_eq!(lock_holder(&keeper), None);
    // The session goes away before it is ready: a crash again.
    hang_up(peer);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the log to be held again");
    assert!(lock_holder(&keeper).is_some_and(|label| label.starts_with("keeper ")));
    assert!(socket_of(&keeper).exists());
    assert!(support::group::alive(pgid), "the job is still held");
    assert!(job_completed_lines(&keeper).is_empty());
    // A later resume still finds its job.
    let mut again = connect(&keeper);
    identify(&mut again, &id);
    assert_eq!(handover(&mut again).len(), 1);
}

#[test]
fn a_late_handover_request_still_works_for_a_connection_that_waited() {
    // The keeper does not time out a candidate that is waiting for the log
    // mutex, only one that has not finished asking.
    let (keeper, runtime, _reaper, _) = held_keeper("keeper-slow-append", "sleep 60");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    let append = super::lock(&runtime.log);
    say(&mut peer, &ToKeeper::Handover(true));
    drop(append);
    assert!(matches!(
        hear(&peer, "the acknowledgement"),
        Some(FromKeeper::Handover { running }) if running.len() == 1
    ));
}

#[test]
fn a_candidate_negotiation_ends_at_its_deadline() {
    let (keeper, _runtime, _reaper, clock) = held_keeper("keeper-negotiation-deadline", "sleep 60");
    let id = keeper.id.clone();
    let until = clock.origin() + crate::keeper::KEEPER_START;
    let deadline = || Deadline::after(Duration::from_secs(10));
    let mut peer = connect(&keeper);
    let worker = clock
        .mark_parked(until, deadline())
        .expect("the negotiation parks at its deadline")
        .threads()
        .into_iter()
        .next()
        .expect("negotiation watcher");
    identify(&mut peer, &id);
    // One millisecond before the deadline the request is still open.
    clock.advance_when_parked(
        Some(until),
        Some(worker),
        crate::keeper::KEEPER_START.saturating_sub(Duration::from_millis(1)),
        &deadline(),
    );
    clock
        .mark_parked(until, deadline())
        .expect("the negotiation parks again");
    // Parked at the clock, the negotiation can send nothing until it moves.
    assert!(peer.frames.try_recv().is_err());
    // At the deadline the connection ends and nothing was taken.
    clock.advance_when_parked(
        Some(until),
        Some(worker),
        Duration::from_millis(1),
        &deadline(),
    );
    assert_eq!(hear(&peer, "the closed connection").map(|_| ()), None);
    assert!(lock_holder(&keeper).is_some_and(|label| label.starts_with("keeper ")));
    assert!(socket_of(&keeper).exists());
}

#[test]
fn a_complete_request_one_millisecond_before_the_deadline_takes_the_handover() {
    let (keeper, _runtime, _reaper, clock) = held_keeper("keeper-negotiation-edge", "sleep 60");
    let id = keeper.id.clone();
    let until = clock.origin() + crate::keeper::KEEPER_START;
    let deadline = || Deadline::after(Duration::from_secs(10));
    let mut peer = connect(&keeper);
    let worker = clock
        .mark_parked(until, deadline())
        .expect("the negotiation parks at its deadline")
        .threads()
        .into_iter()
        .next()
        .expect("negotiation watcher");
    clock.advance_when_parked(
        Some(until),
        Some(worker),
        crate::keeper::KEEPER_START.saturating_sub(Duration::from_millis(1)),
        &deadline(),
    );
    clock
        .mark_parked(until, deadline())
        .expect("the negotiation parks again");
    identify(&mut peer, &id);
    assert_eq!(handover(&mut peer).len(), 1);
}

#[test]
fn second_eof_ignores_stale_bound_watcher() {
    let clock = fakes::clock::FakeClock::new();
    let (held, held_rx) = ready_signal();
    let (watcher_done, watchers) = mpsc::channel::<u64>();
    let hooks = super::Hooks {
        held: Some(held),
        watcher_done: Some(Box::new(move |generation| {
            match watcher_done.send(generation) {
                Ok(()) | Err(_) => {}
            }
        })),
        ..super::Hooks::default()
    };
    let (mut keeper, _runtime) = serve_keeper_with(
        "keeper-stale-bound",
        SessionId("s_0123456789abcdef".into()),
        clock.clone(),
        hooks,
        None,
    );
    let pgid = start_shell(&keeper, "job", "sleep 60");
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    let deadline = || Deadline::after(Duration::from_secs(10));
    deadline().recv_or_fail(&held_rx, "the log to be held");
    let first_bound = clock.origin() + super::KEEPER_BOUND;
    let first = clock
        .mark_parked(first_bound, deadline())
        .expect("the first watcher parks at its bound")
        .threads()
        .into_iter()
        .next()
        .expect("first watcher");
    // Time passes while the first watcher waits.
    clock.advance_when_parked(
        Some(first_bound),
        Some(first),
        super::KEEPER_BOUND / 2,
        &deadline(),
    );
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    assert_eq!(handover(&mut peer).len(), 1);
    adopt_and_finish(&mut peer);
    // The transfer ends the crash's bound: its watcher is woken and gone.
    let ended = deadline().recv_or_fail(&watchers, "the first watcher to end");
    assert_eq!(ended, 1);
    // The resumed session crashes too; the new bound runs from this crash.
    hang_up(peer);
    deadline().recv_or_fail(&held_rx, "the log to be held again");
    let second_bound = clock.origin() + super::KEEPER_BOUND / 2 + super::KEEPER_BOUND;
    let second = clock
        .mark_parked(second_bound, deadline())
        .expect("the second watcher parks at its bound")
        .threads()
        .into_iter()
        .next()
        .expect("second watcher");
    // Past the first crash's bound, short of the second's: nothing expires.
    clock.advance_when_parked(
        Some(second_bound),
        Some(second),
        super::KEEPER_BOUND / 2,
        &deadline(),
    );
    clock
        .mark_parked(second_bound, deadline())
        .expect("the second watcher parks again");
    assert!(
        support::group::alive(pgid),
        "the old bound must not stop the job"
    );
    assert!(keeper.result.try_recv().is_err(), "the keeper still runs");
    assert!(job_completed_lines(&keeper).is_empty());
    // The second bound does stop it.
    clock.advance_when_parked(
        Some(second_bound),
        Some(second),
        super::KEEPER_BOUND / 2,
        &deadline(),
    );
    deadline()
        .recv_or_fail(&keeper.result, "the keeper to end at the second bound")
        .expect("clean exit");
    let completed = job_completed_lines(&keeper);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["error"]["code"], "keeper_timeout");
}

#[test]
fn a_bound_that_expires_first_leaves_nothing_to_take() {
    let (keeper, _runtime, _reaper, clock) = held_keeper("keeper-bound-first", "sleep 60");
    let deadline = || Deadline::after(Duration::from_secs(10));
    let bound = clock.origin() + super::KEEPER_BOUND;
    let watcher = clock
        .mark_parked(bound, deadline())
        .expect("the watcher parks at the bound")
        .threads()
        .into_iter()
        .next()
        .expect("watcher");
    // One millisecond short of the bound the handover still succeeds.
    clock.advance_when_parked(
        Some(bound),
        Some(watcher),
        super::KEEPER_BOUND.saturating_sub(Duration::from_millis(1)),
        &deadline(),
    );
    clock
        .mark_parked(bound, deadline())
        .expect("the watcher parks again");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    // The bound fires while this candidate asks: the timeout decision wins
    // and the candidate is told there is nothing to take.
    clock.advance_when_parked(
        Some(bound),
        Some(watcher),
        Duration::from_millis(1),
        &deadline(),
    );
    deadline()
        .recv_or_fail(&keeper.result, "the keeper to end at the bound")
        .expect("clean exit");
    let completed = job_completed_lines(&keeper);
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["error"]["code"], "keeper_timeout");
    say(&mut peer, &ToKeeper::Handover(true));
    // Either the keeper answers none or the connection just ends: never a job.
    match hear(&peer, "the answer to a late request") {
        Some(FromKeeper::Handover { running }) => assert!(running.is_empty()),
        None => {}
        other => panic!("unexpected {}", describe(&other)),
    }
}

struct Barrier {
    armed: Arc<AtomicBool>,
    arrived: mpsc::Receiver<()>,
    go: mpsc::Sender<()>,
}

/// A recovery stops before it opens the log, once armed, until released.
fn barrier() -> (Box<dyn Fn() + Send + Sync>, Barrier) {
    let armed = Arc::new(AtomicBool::new(false));
    let (arrive, arrived) = mpsc::channel();
    let (go, released) = mpsc::channel::<()>();
    let released = std::sync::Mutex::new(released);
    let hook_armed = Arc::clone(&armed);
    (
        Box::new(move || {
            if hook_armed.load(Ordering::Acquire) {
                match arrive.send(()) {
                    Ok(()) | Err(_) => {}
                }
                match super::lock(&released).recv() {
                    Ok(()) | Err(_) => {}
                }
            }
        }),
        Barrier { armed, arrived, go },
    )
}

/// The resumed session crashes while it holds a handed-over job and a fresh
/// keeper holds another: both keepers recover, one wins the lock.
fn lock_race(old_wins: bool) {
    let clock = fakes::clock::FakeClock::new();
    let id = SessionId("s_0123456789abcdef".into());
    let deadline = || Deadline::after(Duration::from_secs(10));
    let (old_held, old_held_rx) = ready_signal();
    let (old_before, old_gate) = barrier();
    let (mut old, _) = serve_keeper_with(
        "keeper-lock-race",
        id.clone(),
        clock.clone(),
        super::Hooks {
            held: Some(old_held),
            before_open: Some(old_before),
            ..super::Hooks::default()
        },
        None,
    );
    let old_pgid = start_shell(&old, "old", "sleep 60");
    let _old_reaper = Reaper(old_pgid);
    adopt_call(&old, "old", "j_old");
    crash(&mut old);
    deadline().recv_or_fail(&old_held_rx, "the old keeper to hold the log");
    let mut peer = connect(&old);
    identify(&mut peer, &id);
    assert_eq!(handover(&mut peer).len(), 1);
    adopt_and_finish(&mut peer);
    // The resumed session holds the lock, and starts a fresh keeper.
    let session = log::Log::open(&old.sessions, id.clone(), clock.clone()).expect("session lock");
    let (fresh_held, fresh_held_rx) = ready_signal();
    let (fresh_before, fresh_gate) = barrier();
    let (mut fresh, _) = serve_keeper_with(
        "keeper-lock-race",
        id.clone(),
        clock.clone(),
        super::Hooks {
            held: Some(fresh_held),
            before_open: Some(fresh_before),
            ..super::Hooks::default()
        },
        Some(&old),
    );
    let fresh_pgid = start_shell(&fresh, "new", "sleep 60");
    let _fresh_reaper = Reaper(fresh_pgid);
    adopt_call(&fresh, "new", "j_new");
    old_gate.armed.store(true, Ordering::Release);
    fresh_gate.armed.store(true, Ordering::Release);
    // The session dies: both keepers see their connection end.
    hang_up(peer);
    crash(&mut fresh);
    deadline().recv_or_fail(&old_gate.arrived, "the old keeper to reach the lock");
    deadline().recv_or_fail(&fresh_gate.arrived, "the fresh keeper to reach the lock");
    drop(session);
    let (winner_gate, winner_held, loser_gate, loser, winner_job) = if old_wins {
        (&old_gate, &old_held_rx, &fresh_gate, &fresh, "j_old")
    } else {
        (&fresh_gate, &fresh_held_rx, &old_gate, &old, "j_new")
    };
    let winner = if old_wins { &old } else { &fresh };
    winner_gate.go.send(()).expect("release the winner");
    deadline().recv_or_fail(winner_held, "the winner to hold the log");
    loser_gate.go.send(()).expect("release the loser");
    deadline()
        .recv_or_fail(&loser.result, "the loser to exit")
        .expect("the loser exits cleanly");
    // The loser wrote nothing, took nothing down and left the winner's
    // socket where it was.
    assert!(job_completed_lines(winner).is_empty());
    assert!(socket_of(winner).exists());
    assert!(lock_holder(winner).is_some_and(|label| label.starts_with("keeper ")));
    assert!(support::group::alive(old_pgid));
    assert!(support::group::alive(fresh_pgid));
    // The next resume is handed only the winner's job.
    let mut next = connect(winner);
    identify(&mut next, &id);
    let running = handover(&mut next);
    assert_eq!(
        running
            .iter()
            .map(|job| job.job_id.0.as_str())
            .collect::<Vec<_>>(),
        vec![winner_job]
    );
}

#[test]
fn b2_old_keeper_wins_lock() {
    lock_race(true);
}

#[test]
fn b2_fresh_keeper_wins_lock() {
    lock_race(false);
}

/// One numbered producer block.
fn block(n: u64) -> Vec<u8> {
    format!("block {n:08}\n").into_bytes()
}

#[test]
fn the_switch_moves_every_byte_from_the_file_to_frames() {
    use base64::Engine as _;
    let clock = fakes::clock::FakeClock::new();
    // Pause and release registered before any worker starts. The test owns
    // the release sender, and the test ending drops it, which releases the
    // pause too. The injected clock stays frozen throughout.
    let (arrived_send, arrived) = mpsc::channel();
    let (release_send, release) = mpsc::channel();
    let release = Arc::new(std::sync::Mutex::new(release));
    let (held, held_rx) = ready_signal();
    let hooks = super::Hooks {
        held: Some(held),
        before_switch: Some(Box::new(move || {
            match arrived_send.send(()) {
                Ok(()) | Err(_) => {}
            }
            match release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
            {
                Ok(()) | Err(_) => {}
            }
        })),
        ..super::Hooks::default()
    };
    let (mut keeper, _runtime) = serve_keeper_with(
        "keeper-switch-hook",
        SessionId("s_0123456789abcdef".into()),
        clock.clone(),
        hooks,
        None,
    );
    let out_fifo = keeper.home.path().join("out.fifo");
    fakes::fifo(&out_fifo);
    let mut producer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&out_fifo)
        .expect("fifo");
    let pgid = start_shell(&keeper, "job", &format!("cat '{}'", out_fifo.display()));
    let _reaper = Reaper(pgid);
    adopt_call(&keeper, "job", "j_job");
    crash(&mut keeper);
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&held_rx, "the keeper to hold the log");
    let id = keeper.id.clone();
    let mut peer = connect(&keeper);
    identify(&mut peer, &id);
    let running = handover(&mut peer);
    assert_eq!(running.len(), 1);
    let output = keeper.home.path().join("job.out");
    assert_eq!(running[0].output_path, output);
    // Blocks before the acknowledgement land in the keeper's file. The
    // producer never waits for the handover to complete.
    let mut expected = Vec::new();
    for n in 1..=6 {
        producer.write_all(&block(n)).expect("block");
        expected.extend(block(n));
    }
    let file_is = |wanted: Vec<u8>| {
        let output = output.clone();
        move || std::fs::read(&output).is_ok_and(|bytes| bytes == wanted)
    };
    fakes::wait_until(
        "the pre-switch blocks in the file",
        Deadline::after(Duration::from_secs(10)),
        file_is(expected.clone()),
    );
    // The acknowledgement runs to the switch on a worker, which forwards
    // every frame to the test; the test collects them with deadlines.
    let (forwarded_send, forwarded) = mpsc::channel();
    let (adopted_send, adopted) = mpsc::channel();
    let (worker_send, worker_done) = mpsc::channel();
    std::thread::spawn(move || {
        say(&mut peer, &ToKeeper::Adopted(true));
        let deadline = Deadline::after(Duration::from_secs(10));
        let mut acknowledged = false;
        loop {
            match deadline.recv(&peer.frames) {
                Ok(Some(FromKeeper::AdoptDone(true))) => {
                    acknowledged = true;
                    match adopted_send.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                }
                Ok(Some(frame)) => {
                    let ending = matches!(frame, FromKeeper::Exit { .. });
                    if forwarded_send.send(frame).is_err() {
                        break;
                    }
                    // The job's ending is terminal: nothing after it needs
                    // forwarding, so the worker ends instead of waiting out
                    // its deadline.
                    if ending {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        match worker_send.send(acknowledged) {
            Ok(()) | Err(_) => {}
        }
    });
    // The switch worker is paused: the held block lands in the
    // keeper-owned file, with no corresponding output frame.
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&arrived, "the switch to pause");
    producer.write_all(&block(7)).expect("held block");
    expected.extend(block(7));
    fakes::wait_until(
        "the held block in the keeper-owned file",
        Deadline::after(Duration::from_secs(10)),
        file_is(expected.clone()),
    );
    let mut pre = Vec::new();
    while let Ok(frame) = forwarded.try_recv() {
        pre.push(frame);
    }
    assert!(
        pre.iter()
            .all(|frame| !matches!(frame, FromKeeper::Output { .. })),
        "no output frame crosses the handover before the switch"
    );
    // Release the switch and require the acknowledgement.
    match release_send.send(()) {
        Ok(()) | Err(_) => {}
    }
    Deadline::after(Duration::from_secs(10)).recv_or_fail(&adopted, "the acknowledgement");
    // The next numbered block arrives as an output frame.
    for n in 8..=9 {
        producer.write_all(&block(n)).expect("block");
        expected.extend(block(n));
    }
    let decode = |frame: &FromKeeper| match frame {
        FromKeeper::Output { bytes, .. } => base64::engine::general_purpose::STANDARD
            .decode(bytes)
            .expect("base64"),
        FromKeeper::Started { .. }
        | FromKeeper::Failed { .. }
        | FromKeeper::Exit { .. }
        | FromKeeper::Identity { .. }
        | FromKeeper::Handover { .. }
        | FromKeeper::AdoptDone(_) => Vec::new(),
    };
    let deadline = Deadline::after(Duration::from_secs(10));
    let mut post = Vec::new();
    let mut seen: Vec<u8> = Vec::new();
    while !seen.ends_with(&block(9)) {
        match deadline.recv_or_fail(&forwarded, "a post-switch frame") {
            frame @ FromKeeper::Output { .. } => {
                seen.extend(decode(&frame));
                post.push(frame);
            }
            frame @ FromKeeper::Started { .. }
            | frame @ FromKeeper::Failed { .. }
            | frame @ FromKeeper::Exit { .. }
            | frame @ FromKeeper::Identity { .. }
            | frame @ FromKeeper::Handover { .. }
            | frame @ FromKeeper::AdoptDone(_) => {
                panic!("expected output, heard {}", describe(&Some(frame)))
            }
        }
    }
    drop(producer);
    let ending = loop {
        match deadline.recv_or_fail(&forwarded, "the job ending") {
            frame @ FromKeeper::Exit { .. } => break frame,
            frame @ FromKeeper::Output { .. } => post.push(frame),
            frame @ FromKeeper::Started { .. }
            | frame @ FromKeeper::Failed { .. }
            | frame @ FromKeeper::Identity { .. }
            | frame @ FromKeeper::Handover { .. }
            | frame @ FromKeeper::AdoptDone(_) => {
                panic!("expected the ending, heard {}", describe(&Some(frame)))
            }
        }
    };
    // Every post-switch frame runs through the restored runner, which
    // appends it after the keeper's bytes; the file holds exactly the
    // bytes written, in order.
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(&output)
        .expect("output file");
    let (end_send, completions) = mpsc::channel();
    let emit = Arc::new(fakes::Recorder::default());
    let recorder: Arc<dyn contract::emit::Emit> = emit;
    let opened = contract::jobs::Opened {
        started: contract::events::JobStarted {
            job_id: JobId("j_job".into()),
            tool: Some("shell".into()),
            extension: None,
            description: "job j_job".into(),
            output_path: "artifacts/job.out".into(),

            pgid: None,
            start_time: None,
            kind: None,
        },
        path: output.clone(),
        file,
        end: contract::jobs::End(Box::new(move |completed| match end_send.send(completed) {
            Ok(()) | Err(_) => {}
        })),
        emit: recorder,
        lines: None,
    };
    let (events_send, run_events) = mpsc::channel();
    let runner_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let run = tools::AdoptedRun::new(
        opened,
        run_events,
        None,
        contract::jobs::Stop(Box::new(|| {})),
        runner_clock,
    );
    let (run_send, run_done) = mpsc::channel();
    std::thread::spawn(move || match run_send.send(run.run()) {
        Ok(()) | Err(_) => {}
    });
    for frame in &post {
        match frame {
            FromKeeper::Output { stream, bytes, .. } => events_send
                .send(contract::keeper::HostEvent::Output {
                    stream: *stream,
                    bytes: base64::engine::general_purpose::STANDARD
                        .decode(bytes)
                        .expect("base64"),
                })
                .expect("runner event"),
            FromKeeper::Started { .. }
            | FromKeeper::Failed { .. }
            | FromKeeper::Exit { .. }
            | FromKeeper::Identity { .. }
            | FromKeeper::Handover { .. }
            | FromKeeper::AdoptDone(_) => panic!("post-switch output expected"),
        }
    }
    match ending {
        FromKeeper::Exit { exit, .. } => events_send
            .send(contract::keeper::HostEvent::Exit(exit))
            .expect("runner exit"),
        frame @ FromKeeper::Started { .. }
        | frame @ FromKeeper::Failed { .. }
        | frame @ FromKeeper::Output { .. }
        | frame @ FromKeeper::Identity { .. }
        | frame @ FromKeeper::Handover { .. }
        | frame @ FromKeeper::AdoptDone(_) => {
            panic!("expected the ending, heard {}", describe(&Some(frame)))
        }
    }
    drop(events_send);
    assert!(
        Deadline::after(Duration::from_secs(10))
            .recv_or_fail(&run_done, "the runner to return")
            .is_none(),
        "a reported exit returns no report"
    );
    let completed =
        Deadline::after(Duration::from_secs(10)).recv_or_fail(&completions, "the ending");
    assert_eq!(completed.job_id, JobId("j_job".into()));
    assert_eq!(completed.status, contract::events::Outcome::Completed);
    assert_eq!(std::fs::read(&output).expect("output file"), expected);
    assert!(
        Deadline::after(Duration::from_secs(10))
            .recv_or_fail(&worker_done, "the forwarding worker to finish"),
        "the worker saw the acknowledgement"
    );
}
