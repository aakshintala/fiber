use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::SessionId;
use contract::keeper::{FromKeeper, HostStdin, Spawn, ToKeeper};
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
    // docs/state.md, "Sockets": the writer unlinks its own socket at exit.
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
