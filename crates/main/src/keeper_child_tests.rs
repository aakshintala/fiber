use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::events::EventKind;
use contract::keeper::{FromKeeper, HostStdin, Spawn, ToKeeper};
use contract::{JobId, SessionId};
use fakes::Deadline;

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
    let runtime = super::Runtime {
        home: home.path().to_path_buf(),
        id: SessionId("s_0123456789abcdef".into()),
        clock: fakes::clock::FakeClock::new(),
        out: std::sync::Mutex::new(control),
        log: std::sync::Mutex::new(None),
        supervisor: std::sync::Mutex::new(jobs::keeper::Supervisor::new()),
        processes: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        logged: std::sync::Mutex::new(std::collections::HashSet::new()),
        done,
    };
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
            FromKeeper::Started { .. } | FromKeeper::Failed { .. } => panic!("unexpected reply"),
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
    home: fakes::TempDir,
    sessions: std::path::PathBuf,
    id: SessionId,
    writer: UnixStream,
    lifeline: Option<std::io::PipeWriter>,
    read: mpsc::Receiver<FromKeeper>,
    result: mpsc::Receiver<std::io::Result<()>>,
}

fn serve_keeper(name: &str, id: SessionId, clock: Arc<fakes::clock::FakeClock>) -> Keeper {
    // The takeover binds the session socket under this home: root it under
    // a short parent so the socket path fits SUN_LEN on macOS
    // (SOCKET_PATH_MAX is 103 there). A system temp dir such as the macOS
    // TMPDIR is long enough that only the longest prefix fails, which reads
    // as a missing bound watcher with a leaked job.
    let home = fakes::TempDir::under(std::path::Path::new("/tmp"), name);
    let sessions = home.path().join("projects/p/sessions");
    drop(log::Log::create(&sessions, id.clone(), clock.clone()).expect("session log"));
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
    let (done, result) = mpsc::channel();
    let path = home.path().to_path_buf();
    let keeper_id = id.clone();
    let keeper_clock: Arc<dyn contract::clock::Clock> = clock;
    std::thread::spawn(move || {
        done.send(super::serve(path, keeper_id, keeper_clock, life, control))
            .expect("keeper result");
    });
    Keeper {
        home,
        sessions,
        id,
        writer: client,
        lifeline: Some(lifeline),
        read,
        result,
    }
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
        FromKeeper::Failed { .. } | FromKeeper::Output { .. } | FromKeeper::Exit { .. } => {
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
            FromKeeper::Started { .. } | FromKeeper::Failed { .. } | FromKeeper::Exit { .. } => {}
        }
    }
}

fn adopt_call(keeper: &Keeper, call: &str, job: &str) {
    let out = keeper.home.path().join(format!("{call}.out"));
    std::fs::write(&out, b"").expect("output file");
    let mut writer = keeper.writer.try_clone().expect("writer");
    serde_json::to_writer(
        &mut writer,
        &ToKeeper::Adopt {
            id: call.into(),
            job_id: JobId(job.into()),
            output_path: out,
            errors_path: None,
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
            FromKeeper::Failed { .. } => panic!("expected job end"),
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
}

#[test]
fn keeper_crashed_socket_drops_an_overlong_line_and_keeps_running() {
    // The first read is bounded to the frozen line plus its newline, so
    // more bytes with no newline drop the connection without parsing,
    // as a torn tail does (`docs/invocation.md`, "Commands and flags").
    // More than the frozen line plus its newline, held open: without the
    // bound the read would wait past the deadline for a newline.
    let id = SessionId("s_6123456789abcdef".into());
    let clock = fakes::clock::FakeClock::new();
    let mut keeper = serve_keeper("keeper-crash-overlong", id, clock.clone());
    let pgid = start_shell(&keeper, "sleepy", "sleep 60");
    adopt_call(&keeper, "sleepy", "j_sleepy");
    crash(&mut keeper);
    let socket = crashed_socket(&keeper);
    let mut peer = await_crashed_socket(&socket);
    peer.write_all(&vec![b'x'; "{\"shutdown\":true}".len() + 9])
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
            FromKeeper::Failed { .. } => panic!("expected exit"),
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
        let runtime = Arc::new(super::Runtime {
            home: home.path().to_path_buf(),
            id: SessionId("s_0123456789abcdef".into()),
            clock: fakes::clock::FakeClock::new(),
            out: std::sync::Mutex::new(out),
            log: std::sync::Mutex::new(None),
            supervisor: std::sync::Mutex::new(jobs::keeper::Supervisor::new()),
            processes: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            logged: std::sync::Mutex::new(std::collections::HashSet::new()),
            done,
        });
        assert!(
            runtime.control(reader).is_ok(),
            "torn tail of {} bytes must not fail control",
            tail.len()
        );
    }
}
