//! Device listing and revocation over a fake hub socket.
use super::*;
use contract::{ErrorCode, HubLine};
use fakes::Deadline;
use serde_json::{Map, json};
use std::io::BufRead as _;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);
fn run_wait(answer: Value, device: Option<&str>) -> (Result<(), CliError>, Vec<u8>, Value) {
    let (client, server) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    server.set_read_timeout(Some(WAIT)).unwrap();
    let (command_tx, command_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut read = BufReader::new(server);
        let mut line = String::new();
        read.read_line(&mut line).unwrap();
        command_tx
            .send(serde_json::from_str::<Value>(&line).unwrap())
            .unwrap_or(());
        writeln!(read.get_mut(), "{answer}").unwrap();
    });
    let device = device.map(str::to_owned);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
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
        let mut out = Vec::new();
        let result = match device {
            Some(device) => revoke(&mut out, &device, &mut connect),
            None => list(&mut out, &mut connect),
        };
        tx.send((result, out)).unwrap_or(());
    });
    let deadline = Deadline::after(WAIT);
    let (result, out) = deadline.recv_or_fail(&rx, "token command returns");
    (
        result,
        out,
        deadline.recv_or_fail(&command_rx, "the token request"),
    )
}
fn accepted(devices: Value) -> Value {
    json!({"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_devices","result":{"devices":devices}}})
}
#[test]
fn token_list_prints_the_header_and_utc_times_with_missing_as_dash() {
    let (result, out, command) = run_wait(
        accepted(
            json!([{"name":"phone","paired_at":1700000000000u64,"last_connected_at":1700000001000u64},{"name":"tablet"}]),
        ),
        None,
    );
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "name    paired                last connected\nphone   2023-11-14T22:13:20Z  2023-11-14T22:13:21Z\ntablet  -                     -\n"
    );
    assert_eq!(
        command,
        json!({"id":"c_devices","command":"devices","args":{}})
    );
}
#[test]
fn token_list_with_no_devices_prints_the_header() {
    let (result, out, _) = run_wait(accepted(json!([])), None);
    result.unwrap();
    assert_eq!(out, b"name  paired  last connected\n");
}
#[test]
fn token_revoke_prints_the_device() {
    let answer = json!({"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_revoke"}});
    let (result, out, command) = run_wait(answer, Some("phone"));
    result.unwrap();
    assert_eq!(out, b"phone\n");
    assert_eq!(
        command,
        json!({"id":"c_revoke","command":"revoke","args":{"device":"phone"}})
    );
}
#[test]
fn a_rejected_revoke_prints_its_message() {
    let answer = json!({"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":"c_revoke","code":"not_found","message":"No device named phone is paired."}});
    let (result, out, _) = run_wait(answer, Some("phone"));
    let failure = result.unwrap_err();
    assert!(
        matches!(&failure, CliError::Rejected { message, .. } if message == "No device named phone is paired."),
        "{failure}"
    );
    assert_eq!(failure.code(), ErrorCode::NotFound);
    assert!(out.is_empty());
}
#[test]
fn malformed_device_lists_are_refused_before_printing() {
    for devices in [
        json!(1),
        json!([{}]),
        json!([{"name":1}]),
        json!([{"name":"phone","paired_at":"bad"}]),
        json!([{"name":"phone","last_connected_at":-1}]),
    ] {
        let (result, out, _) = run_wait(accepted(devices), None);
        let error = result.unwrap_err();
        assert!(matches!(error, CliError::DevicesMalformed), "{error}");
        assert_eq!(error.code(), ErrorCode::IoFailed);
        assert!(out.is_empty());
    }
}
/// Runs an exit-code wrapper against one hub answer, or an unreachable hub.
fn exit_code(
    answer: Option<Value>,
    run: fn(&mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>) -> i32,
) -> i32 {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut connect: Box<dyn FnMut() -> Result<doors::hub::Hub, doors::Error>> = match answer {
            Some(answer) => {
                let (client, server) = UnixStream::pair().unwrap();
                client.set_read_timeout(Some(WAIT)).unwrap();
                server.set_read_timeout(Some(WAIT)).unwrap();
                thread::spawn(move || {
                    let mut read = BufReader::new(server);
                    let mut line = String::new();
                    read.read_line(&mut line).unwrap();
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
        tx.send(run(&mut *connect)).unwrap_or(());
    });
    Deadline::after(WAIT).recv_or_fail(&rx, "the token wrapper returns its exit code")
}
#[test]
fn token_list_exits_zero_once_the_list_is_printed() {
    assert_eq!(
        exit_code(Some(accepted(json!([]))), |connect| hub_token_list(connect)),
        0
    );
}
#[test]
fn token_list_exits_nonzero_when_the_hub_is_unreachable() {
    assert_ne!(exit_code(None, |connect| hub_token_list(connect)), 0);
}
#[test]
fn token_revoke_exits_zero_once_the_device_is_revoked() {
    let answer = json!({"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_revoke"}});
    assert_eq!(
        exit_code(Some(answer), |connect| hub_token_revoke("phone", connect)),
        0
    );
}
#[test]
fn token_revoke_exits_nonzero_when_the_hub_is_unreachable() {
    assert_ne!(
        exit_code(None, |connect| hub_token_revoke("phone", connect)),
        0
    );
}
