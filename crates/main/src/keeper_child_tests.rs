use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::SessionId;
use contract::keeper::{FromKeeper, HostStdin, Spawn, ToKeeper};
use fakes::Deadline;

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
