//! Pairing output, QR payloads and URL validation against a fake hub socket.
use super::*;
use contract::{ErrorCode, HubLine};
use fakes::Deadline;
use serde_json::{Map, json};
use std::io::{BufRead as _, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);
const CODE: &str = "7k2m9qx4bd";

fn run_wait(
    answer: Value,
    address: Option<Address>,
    terminal: bool,
) -> (Result<(), CliError>, Vec<u8>, Value) {
    let (client, server) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    server.set_read_timeout(Some(WAIT)).unwrap();
    let (command_tx, command_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut read = BufReader::new(server);
        let mut text = String::new();
        read.read_line(&mut text).unwrap();
        command_tx
            .send(serde_json::from_str::<Value>(&text).unwrap())
            .unwrap_or(());
        writeln!(read.get_mut(), "{answer}").unwrap();
    });
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let mut client = Some(client);
        let mut connect = || {
            Ok((
                client.take().unwrap(),
                HubLine {
                    kind: "hub_hello".to_owned(),
                    ts: 1,
                    schema_version: 1,
                    payload: Map::new(),
                },
            ))
        };
        let result = run(&mut out, "phone", address.as_ref(), terminal, &mut connect);
        tx.send((result, out)).unwrap_or(());
    });
    let deadline = Deadline::after(WAIT);
    let (result, out) = deadline.recv_or_fail(&rx, "pair command returns");
    (
        result,
        out,
        deadline.recv_or_fail(&command_rx, "pairing request"),
    )
}
fn accepted() -> Value {
    json!({"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_pairing_code","result":{"code":CODE}}})
}

#[test]
fn pair_off_a_terminal_prints_the_code_alone() {
    let (result, out, command) = run_wait(accepted(), None, false);
    result.unwrap();
    assert_eq!(out, format!("{CODE}\n").as_bytes());
    assert_eq!(
        command,
        json!({"id":"c_pairing_code","command":"pairing_code","args":{"device":"phone"}})
    );
}
#[test]
fn pair_works_with_no_port_configured() {
    let (result, out, command) = run_wait(accepted(), None, false);
    result.unwrap();
    assert_eq!(out, format!("{CODE}\n").as_bytes());
    assert_eq!(command["command"], "pairing_code");
}
#[test]
fn pair_on_a_terminal_prints_the_code_then_the_qr_code() {
    let (result, out, _) = run_wait(accepted(), None, true);
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("{CODE}\n\n{}\n", draw(CODE).unwrap())
    );
    let address = Address::parse("wss://mybox.tailnet.ts.net").unwrap();
    let text = qr_text(Some(&address), CODE);
    let (result, out, _) = run_wait(accepted(), Some(address), true);
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("{CODE}\n\n{}\n", draw(&text).unwrap())
    );
}
#[test]
fn the_qr_text_is_the_address_then_the_code() {
    let address = Address::parse("WSS://mybox.tailnet.ts.net/path").unwrap();
    assert_eq!(
        qr_text(Some(&address), CODE),
        format!("WSS://mybox.tailnet.ts.net/path\n{CODE}")
    );
    assert_eq!(qr_text(None, CODE), CODE);
}
#[test]
fn address_accepts_only_ws_and_wss() {
    for value in [
        "ws://127.0.0.1:4040",
        "wss://mybox.tailnet.ts.net",
        "WSS://h/p",
        "ws://h:1",
        "ws://h:2",
        "ws://h:65534",
        "ws://h:65535",
        "ws://[::1]",
        "ws://[::1]:80",
    ] {
        assert_eq!(Address::parse(value).unwrap().as_str(), value);
    }
    for value in [
        "http://x",
        "ws://",
        "wss://:80",
        "ws://h:0",
        "ws://h:+5",
        "ws://h:65536",
        "ws://h:abc",
        "ws://h:",
        "ws://h x",
        "ws://h\n",
        "mybox",
        "",
        "ws://[]",
        "ws://[::1",
        "ws://[::1]oops",
        "ws://[::1]:",
        "ws://[::1]:0",
    ] {
        assert_eq!(
            Address::parse(value).err().unwrap(),
            "The address must be a ws:// or wss:// URL.",
            "{value:?}"
        );
    }
}
#[test]
fn an_address_too_long_to_draw_fails_with_usage() {
    let long = Address::parse(&format!("wss://{}", "a".repeat(10_000))).unwrap();
    let (result, out, _) = run_wait(accepted(), Some(long), true);
    let failure = result.unwrap_err();
    assert!(matches!(failure, CliError::QrTooLong), "{failure}");
    assert_eq!(failure.code(), ErrorCode::Usage);
    assert!(out.is_empty());
    for (length, success) in [(2319, true), (2320, true), (2321, false)] {
        let address = Address::parse(&format!("wss://{}", "a".repeat(length - 6))).unwrap();
        assert_eq!(
            draw(&qr_text(Some(&address), CODE)).is_ok(),
            success,
            "{length} address bytes"
        );
    }
}
#[test]
fn a_rejected_pairing_code_prints_its_message() {
    let rejected = json!({"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":"c_pairing_code","code":"invalid_arguments","message":"already paired"}});
    let (result, out, _) = run_wait(rejected, None, false);
    let failure = result.unwrap_err();
    assert!(
        matches!(&failure, CliError::Rejected { message, .. } if message == "already paired"),
        "{failure}"
    );
    assert_eq!(failure.code(), ErrorCode::InvalidArguments);
    assert!(out.is_empty());
}
#[test]
fn a_pair_reply_without_a_string_code_is_refused() {
    let mut answer = accepted();
    answer["payload"]["result"]["code"] = Value::from(1);
    let (result, out, _) = run_wait(answer, None, false);
    let error = result.unwrap_err();
    assert!(matches!(error, CliError::PairingAnswerMissing), "{error}");
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert!(out.is_empty());
}
#[test]
fn qr_rendering_is_pinned_with_and_without_an_address() {
    assert_eq!(
        draw(CODE).unwrap(),
        "█████████████████████████████\n█████████████████████████████\n████ ▄▄▄▄▄ █ █▀███ ▄▄▄▄▄ ████\n████ █   █ █▀▀ ▀ █ █   █ ████\n████ █▄▄▄█ ██ ████ █▄▄▄█ ████\n████▄▄▄▄▄▄▄█ █▄█▄█▄▄▄▄▄▄▄████\n████ █ ▄█▄▄▄█▄▄█▀▀▄█▀▄█▄ ████\n█████▀ ▄█▀▄█▀█▀█ █▄█▀█▄▄ ████\n███████▄█▄▄█  █▄██    █ █████\n████ ▄▄▄▄▄ █ ▄▀▀▀ ▄▄▄▀ ▄▀████\n████ █   █ █▀█▀▄███▀█  ▀█████\n████ █▄▄▄█ █▄▀▄ ▀█▀▀█ ▄█▀████\n████▄▄▄▄▄▄▄█▄▄▄▄▄▄███▄▄██████\n█████████████████████████████\n▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀"
    );
    let address = Address::parse("wss://mybox.tailnet.ts.net").unwrap();
    assert_eq!(
        draw(&qr_text(Some(&address), CODE)).unwrap(),
        "█████████████████████████████████████\n█████████████████████████████████████\n████ ▄▄▄▄▄ █ ▀▀█▀███▄▄ ▄██ ▄▄▄▄▄ ████\n████ █   █ █▀▀▀█▀▄█ ▄▀██▀█ █   █ ████\n████ █▄▄▄█ ██▀ ▀▄▄ ▄ ▀▀ ██ █▄▄▄█ ████\n████▄▄▄▄▄▄▄█ █ ▀ ▀ █ ▀▄█▄█▄▄▄▄▄▄▄████\n████ ▀ ▄▀ ▄ █   ▄▄▄██▄   █ ▀▀▄▀▄ ████\n█████▀▀  ▀▄▀█ ▀ ▄▀ █▀▄ █ ▄▀  ▀█▀█████\n████▀▀▄▀▀█▄ ▀▀█▀ ▀█▀  ▀ ▀█▀▀█▀▄ ▀████\n████▄▀▀█▀█▄▀▀█ ▀▄▀▄▀██ ▄▀█▄███▄▄ ████\n██████ ██▀▄▄████▄▀█▀▄ ██▀▄█▀▄█▀▀█████\n████▄█ ▄  ▄██▀▀  ██▄▄ ▄█▄ ▄▄▀  ▀▀████\n█████▄▄▄▄█▄▄ █  ▄ ▀▄█▄█▄ ▄▄▄   ▄ ████\n████ ▄▄▄▄▄ █ ██ █▄██▄█ ▄ █▄█ ▄▄ ▀████\n████ █   █ █▀▄  ▄▀ ▀▀▄██▄ ▄  ▄▄█▀████\n████ █▄▄▄█ █▄ ▄█▄▀██▄▄██ █▀▄█▀█▀▄████\n████▄▄▄▄▄▄▄█▄▄█▄▄███▄██▄▄██▄███▄█████\n█████████████████████████████████████\n▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀"
    );
}
/// Runs the exit-code wrapper against one hub answer, or an unreachable hub.
fn exit_code(answer: Option<Value>) -> i32 {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut connect: Box<dyn FnMut() -> Result<doors::hub::Hub, doors::Error>> = match answer {
            Some(answer) => {
                let (client, server) = UnixStream::pair().unwrap();
                client.set_read_timeout(Some(WAIT)).unwrap();
                server.set_read_timeout(Some(WAIT)).unwrap();
                thread::spawn(move || {
                    let mut read = BufReader::new(server);
                    let mut text = String::new();
                    read.read_line(&mut text).unwrap();
                    writeln!(read.get_mut(), "{answer}").unwrap();
                });
                let mut client = Some(client);
                Box::new(move || {
                    Ok((
                        client.take().unwrap(),
                        HubLine {
                            kind: "hub_hello".to_owned(),
                            ts: 1,
                            schema_version: 1,
                            payload: Map::new(),
                        },
                    ))
                })
            }
            None => Box::new(|| Err(doors::Error::Local(io::Error::other("no hub")))),
        };
        tx.send(hub_pair("phone", None, &mut *connect))
            .unwrap_or(());
    });
    Deadline::after(WAIT).recv_or_fail(&rx, "pair returns its exit code")
}
#[test]
fn pair_exits_zero_once_the_code_is_printed() {
    assert_eq!(exit_code(Some(accepted())), 0);
}
#[test]
fn pair_exits_nonzero_when_the_hub_is_unreachable() {
    assert_ne!(exit_code(None), 0);
}
