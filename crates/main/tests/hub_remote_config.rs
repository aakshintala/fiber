//! Configuration over an authenticated hub port belongs to the hub's machine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
#[path = "support/hub_start.rs"]
mod hub_start;
mod support;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Shutdown, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use support::*;

#[derive(Debug)]
enum Frame {
    Text(Value),
    Close(u16),
    Eof,
}
struct Ws {
    stream: TcpStream,
    rx: mpsc::Receiver<Frame>,
}
impl Ws {
    fn connect(setup: &Setup, port: u16) -> Self {
        let deadline = setup.deadline;
        let mut stream = bounded(deadline, "the hub TCP port", move || {
            TcpStream::connect(("127.0.0.1", port))
        })
        .unwrap();
        stream.set_write_timeout(Some(deadline.left())).unwrap();
        stream.set_read_timeout(Some(deadline.left())).unwrap();
        stream.write_all(format!("GET /hub HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").as_bytes()).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        assert_eq!(first, "HTTP/1.1 101 Switching Protocols\r\n");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            assert!(!line.is_empty(), "handshake closed before its headers");
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Some(frame) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
            tx.send(Frame::Eof).unwrap_or(());
        });
        Self { stream, rx }
    }
    fn send(&mut self, setup: &Setup, value: Value) {
        let bytes = value.to_string().into_bytes();
        let mut frame = vec![0x81];
        if bytes.len() < 126 {
            frame.push(0x80 | u8::try_from(bytes.len()).unwrap());
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_be_bytes());
        }
        let mask = [1, 2, 3, 4];
        frame.extend_from_slice(&mask);
        frame.extend(
            bytes
                .iter()
                .zip(mask.iter().cycle())
                .map(|(byte, mask)| byte ^ mask),
        );
        self.stream
            .set_write_timeout(Some(setup.deadline.left()))
            .expect("set the websocket write timeout to the test's remaining deadline");
        self.stream
            .write_all(&frame)
            .expect("write the masked client frame");
    }
    fn next(&self, setup: &Setup) -> Frame {
        setup
            .deadline
            .recv_or_fail(&self.rx, "the next websocket frame")
    }
    fn text(&self, setup: &Setup) -> Value {
        match self.next(setup) {
            Frame::Text(value) => value,
            frame @ (Frame::Close(_) | Frame::Eof) => panic!("expected a JSON line, got {frame:?}"),
        }
    }
}
impl Drop for Ws {
    fn drop(&mut self) {
        self.stream.shutdown(Shutdown::Both).unwrap_or(());
    }
}
fn read_frame(read: &mut BufReader<TcpStream>) -> Option<Frame> {
    let mut head = [0; 2];
    read.read_exact(&mut head).ok()?;
    let [opcode, len] = head;
    let mut length = usize::from(len & 127);
    if length == 126 {
        let mut bytes = [0; 2];
        read.read_exact(&mut bytes).ok()?;
        length = usize::from(u16::from_be_bytes(bytes));
    } else if length == 127 {
        let mut bytes = [0; 8];
        read.read_exact(&mut bytes).ok()?;
        length = usize::try_from(u64::from_be_bytes(bytes)).ok()?;
    }
    let mut payload = vec![0; length];
    read.read_exact(&mut payload).ok()?;
    match opcode & 15 {
        1 => Some(Frame::Text(serde_json::from_slice(&payload).unwrap())),
        8 => {
            let bytes: [u8; 2] = payload.get(..2)?.try_into().ok()?;
            Some(Frame::Close(u16::from_be_bytes(bytes)))
        }
        _ => None,
    }
}

/// Runs `git` with `args` in `workspace` for fixture setup: no identity
/// flags, and the failure reports stderr.
fn git_setup(setup: &Setup, workspace: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(workspace)
        .process_group(0)
        .envs(fakes::check_run())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_to_exit(setup.deadline, "git setup", command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn provider(setup: &Setup) {
    let source = setup.root.path().join("provider");
    write_json(
        &source.join("extension.json"),
        &json!({"name":"acme","version":"v0.0.0","fiber":"0.0.0","api":1,"providers":{"acme":["http://x/v1"]}}),
    );
    write_json(
        &source.join("providers/acme.json"),
        &json!({"name":"acme","models":[{"id":"m","protocol":"openai-responses","base_url":"http://x/v1","context_window":1000}]}),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

fn configure(client: &mut Ws, setup: &Setup, args: Value) -> Value {
    client.send(setup, json!({"id":"c","command":"configure","args":args}));
    let answer = client.text(setup);
    assert_eq!(answer["payload"]["command_id"], "c");
    answer
}

fn accepted(answer: Value) -> Value {
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    answer["payload"]["result"].clone()
}

#[test]
fn the_hub_searches_its_own_machine_over_the_port() {
    let setup = Setup::new();
    let remote = fakes::TempDir::new("hub-search-workspace");
    let user_home = fakes::TempDir::new("hub-user-home");
    let workspace = remote.path();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/hub-alpha.rs"), "hub only").unwrap();
    fs::write(workspace.join("client-untracked.rs"), "untracked").unwrap();
    fs::write(user_home.path().join("hub-home-file"), "hub home").unwrap();
    git_setup(&setup, workspace, &["init", "-q"]);
    git_setup(&setup, workspace, &["add", "src/hub-alpha.rs"]);
    let served = hub_start::serve_installed(
        &setup,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &setup.home().join("config.json"),
                &json!({"hub":{"port":port}}),
            );
            let mut command = setup.fiber(&["hub", "serve", "--installed"]);
            command.env("HOME", user_home.path());
            command
        },
    );
    let port = served.port;
    let (stream, hello) = served.hub;
    assert_eq!(hello.kind, "hub_hello");
    let local = Socket::from(setup.deadline, stream);
    let mut unauth = Ws::connect(&setup, port);
    assert_eq!(unauth.text(&setup)["kind"], "hub_hello");
    unauth.send(
        &setup,
        json!({"id":"no","command":"search_files","args":{"workspace":workspace,"text":"alpha"}}),
    );
    let refused = unauth.text(&setup);
    assert_eq!(refused["payload"]["code"], "unauthenticated");
    assert!(!refused.to_string().contains("hub-alpha.rs"));
    assert!(matches!(unauth.next(&setup), Frame::Close(1008)));
    assert!(matches!(unauth.next(&setup), Frame::Eof));
    let paired = run_to_exit(
        setup.deadline,
        "pair search client",
        setup.fiber(&["hub", "pair", "search-client"]),
    );
    assert!(
        paired.status.success(),
        "{}",
        String::from_utf8_lossy(&paired.stderr)
    );
    let code = String::from_utf8(paired.stdout).unwrap().trim().to_owned();
    let mut pairing = Ws::connect(&setup, port);
    assert_eq!(pairing.text(&setup)["kind"], "hub_hello");
    pairing.send(
        &setup,
        json!({"id":"p","command":"pair","args":{"code":code}}),
    );
    let token = accepted(pairing.text(&setup))["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(pairing.text(&setup)["kind"], "device_changed");
    assert_eq!(
        recv(&local, "search client paired")["kind"],
        "device_changed"
    );
    drop(pairing);
    let mut client = Ws::connect(&setup, port);
    assert_eq!(client.text(&setup)["kind"], "hub_hello");
    client.send(
        &setup,
        json!({"id":"a","command":"authenticate","args":{"token":token}}),
    );
    accepted(client.text(&setup));
    for (text, rows) in [
        ("alpha", json!(["src/hub-alpha.rs"])),
        ("untracked", json!([])),
        ("~/hub-home", json!(["~/hub-home-file"])),
    ] {
        client.send(&setup, json!({"id":"search","command":"search_files","args":{"workspace":workspace,"text":text}}));
        let answer = client.text(&setup);
        assert_eq!(answer["payload"]["command_id"], "search");
        assert_eq!(accepted(answer), json!({"rows":rows}));
    }
    drop((client, unauth, local));
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
}

#[test]
fn the_hub_answers_configuration_from_its_own_home_over_the_port() {
    let setup = Setup::new();
    let remote = fakes::TempDir::new("remote-workspace");
    let workspace = remote.path();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/alpha.rs"), "hub only").unwrap();
    fs::write(workspace.join("beta.txt"), "hub only").unwrap();
    git_setup(&setup, workspace, &["init", "-q"]);
    git_setup(&setup, workspace, &["add", "."]);
    git_setup(
        &setup,
        workspace,
        &[
            "-c",
            "user.name=Fiber",
            "-c",
            "user.email=fiber@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    // The client has different files: no operation may read or change them.
    let client_home = setup.workspace().join("client-home");
    write_json(
        &client_home.join("config.json"),
        &json!({"model":"client/model"}),
    );
    let client_before = fs::read(client_home.join("config.json")).unwrap();
    provider(&setup);
    let served = hub_start::serve_installed(
        &setup,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &setup.home().join("config.json"),
                &json!({"hub":{"port":port},"model":"acme/m","tui":{"theme":"dark"},"skills":{"disabled":["s"]},"mcp":{"servers":{"x":{"command":"unused","tools":{"disabled":["t"]}}}}}),
            );
            setup.fiber(&["hub", "serve", "--installed"])
        },
    );
    let port = served.port;
    let (stream, hello) = served.hub;
    assert_eq!(hello.kind, "hub_hello");
    let rule = json!({"decision":"allow","tool":"shell","prefix":"npm test"});
    fs::write(setup.home().join("rules"), format!("{rule}\n")).unwrap();
    let local = Socket::from(setup.deadline, stream);
    let mut unauth = Ws::connect(&setup, port);
    assert_eq!(unauth.text(&setup)["kind"], "hub_hello");
    let refused = configure(
        &mut unauth,
        &setup,
        json!({"operation":"store_key","name":"acme","key":"unauth-secret"}),
    );
    assert_eq!(refused["kind"], "command_rejected");
    assert_eq!(refused["payload"]["code"], "unauthenticated");
    assert!(!refused.to_string().contains("unauth-secret"));
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(matches!(unauth.next(&setup), Frame::Close(1008)));
    assert!(matches!(unauth.next(&setup), Frame::Eof));
    let paired = run_to_exit(
        setup.deadline,
        "pair phone",
        setup.fiber(&["hub", "pair", "phone"]),
    );
    assert!(
        paired.status.success(),
        "{}",
        String::from_utf8_lossy(&paired.stderr)
    );
    let code = String::from_utf8(paired.stdout).unwrap().trim().to_owned();
    let mut pairing = Ws::connect(&setup, port);
    assert_eq!(pairing.text(&setup)["kind"], "hub_hello");
    pairing.send(
        &setup,
        json!({"id":"p","command":"pair","args":{"code":code}}),
    );
    let token = accepted(pairing.text(&setup))["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(pairing.text(&setup)["kind"], "device_changed");
    assert_eq!(recv(&local, "pair announcement")["kind"], "device_changed");
    drop(pairing);
    let mut client = Ws::connect(&setup, port);
    assert_eq!(client.text(&setup)["kind"], "hub_hello");
    client.send(
        &setup,
        json!({"id":"a","command":"authenticate","args":{"token":token}}),
    );
    accepted(client.text(&setup));
    let settings = accepted(configure(
        &mut client,
        &setup,
        json!({"operation":"settings","workspace":workspace}),
    ));
    let rows = settings["rows"].as_array().unwrap();
    let model = rows.iter().find(|r| r["key"] == "model").unwrap();
    assert_eq!(model["value"], "\"acme/m\"");
    assert_eq!(model["file"], json!(setup.home().join("config.json")));
    assert!(rows.iter().all(|r| {
        let key = r["key"].as_str().unwrap();
        !key.starts_with("tui.") && !key.starts_with("keys")
    }));
    let rules = accepted(configure(
        &mut client,
        &setup,
        json!({"operation":"rules","workspace":workspace}),
    ));
    assert_eq!(rules["global"]["rows"][0]["rule"], rule);
    let tools = accepted(configure(
        &mut client,
        &setup,
        json!({"operation":"tool_switches","workspace":workspace}),
    ));
    assert_eq!(tools["groups"][0]["group"], json!({"mcp":"x"}));
    assert_eq!(tools["groups"][0]["everywhere"]["disabled"], json!(["t"]));
    assert_eq!(
        accepted(configure(
            &mut client,
            &setup,
            json!({"operation":"skills_disabled","workspace":workspace})
        ))["everywhere"],
        json!(["s"])
    );
    let stored = configure(
        &mut client,
        &setup,
        json!({"operation":"store_key","name":"acme","key":"sk-hub-only"}),
    );
    assert!(!stored.to_string().contains("sk-hub-only"));
    assert_eq!(
        accepted(stored),
        json!({"path":"credentials/acme/default","replaced":false})
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/default")).unwrap(),
        "sk-hub-only"
    );
    assert!(!client_home.join("credentials/acme/default").exists());
    accepted(configure(
        &mut client,
        &setup,
        json!({"operation":"set","workspace":workspace,"layer":"global","key":"thinking","text":"high"}),
    ));
    let file = setup.home().join("config.json");
    let before = fs::read(&file).unwrap();
    let config: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(config["thinking"], "high");
    assert_eq!(
        configure(
            &mut client,
            &setup,
            json!({"operation":"set","workspace":workspace,"layer":"global","key":"tui.theme","text":"light"})
        )["payload"]["code"],
        "invalid_arguments"
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(
        accepted(configure(
            &mut client,
            &setup,
            json!({"operation":"global_file"})
        )),
        json!({"file":file})
    );
    client.send(
        &setup,
        json!({"id":"read","command":"read_config","args":{"workspace":workspace,"path":file}}),
    );
    let read = accepted(client.text(&setup));
    assert_eq!(read["text"], String::from_utf8(before).unwrap());
    let text = "{\"thinking\":\"low\"}";
    client.send(&setup,json!({"id":"write","command":"write_config","args":{"workspace":workspace,"path":file,"text":text,"version":read["version"]}}));
    accepted(client.text(&setup));
    client.send(&setup,json!({"id":"stale","command":"write_config","args":{"workspace":workspace,"path":file,"text":"lost","version":read["version"]}}));
    let stale = client.text(&setup);
    assert_eq!(stale["kind"], "command_rejected");
    assert_eq!(stale["payload"]["code"], "stale_request");
    assert_eq!(fs::read_to_string(file).unwrap(), text);
    assert_eq!(
        fs::read(client_home.join("config.json")).unwrap(),
        client_before
    );
    assert!(!setup.hub_log().contains("sk-hub-only"));
    assert!(!setup.hub_log().contains("unauth-secret"));
    drop((client, unauth, local));
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
}
