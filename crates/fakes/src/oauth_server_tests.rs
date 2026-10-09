use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

/// POSTs a form to `path` and returns the status and the body. A read that
/// outlasts its deadline fails the test rather than hanging it.
fn post(server: &OauthServer, path: &str, form: &str) -> (u16, String) {
    let addr = server.url().trim_start_matches("http://").to_owned();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\ncontent-type: application/x-www-form-urlencoded\r\ncontent-length: {}\r\n\r\n{form}",
        form.len()
    )
    .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let status = text.split(' ').nth(1).unwrap().parse().unwrap();
    let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
    (status, body)
}

#[test]
fn replies_come_in_script_order_whatever_the_path() {
    let server = OauthServer::start(vec![
        OauthReply::token("at", "rt", 3600),
        OauthReply::pending(),
        OauthReply::slow_down(),
        OauthReply::denied(),
        OauthReply::expired(),
        OauthReply::raw(200, "not json"),
    ]);

    let paths = [
        "/token",
        "/device/token",
        "/token",
        "/x",
        "/device/token",
        "/token",
    ];
    let replies: Vec<_> = paths.iter().map(|path| post(&server, path, "")).collect();

    let token: serde_json::Value = serde_json::from_str(&replies[0].1).unwrap();
    assert_eq!(replies[0].0, 200);
    assert_eq!(token["access_token"], "at");
    assert_eq!(token["refresh_token"], "rt");
    assert_eq!(token["token_type"], "Bearer");
    assert_eq!(token["expires_in"], 3600);
    let errors: Vec<_> = replies[1..5]
        .iter()
        .map(|(status, body)| {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            (*status, body["error"].as_str().unwrap().to_owned())
        })
        .collect();
    assert_eq!(
        errors,
        [
            (400, "authorization_pending".to_owned()),
            (400, "slow_down".to_owned()),
            (400, "access_denied".to_owned()),
            (400, "expired_token".to_owned()),
        ]
    );
    assert_eq!(replies[5], (200, "not json".to_owned()));
}

#[test]
fn a_request_past_the_script_gets_script_exhausted() {
    let server = OauthServer::start(vec![OauthReply::pending()]);

    assert_eq!(post(&server, "/token", "").0, 400);
    assert_eq!(
        post(&server, "/token", ""),
        (500, r#"{"error":"script_exhausted"}"#.to_owned())
    );
    assert_eq!(
        post(&server, "/token", ""),
        (500, r#"{"error":"script_exhausted"}"#.to_owned())
    );
}

#[test]
fn the_first_and_the_thousandth_request_past_the_script_get_script_exhausted() {
    let server = OauthServer::start(vec![OauthReply::pending()]);
    post(&server, "/token", "");

    let exhausted = (500, r#"{"error":"script_exhausted"}"#.to_owned());
    assert_eq!(post(&server, "/token", ""), exhausted);
    for _ in 0..998 {
        post(&server, "/token", "");
    }
    assert_eq!(post(&server, "/token", ""), exhausted);
    assert_eq!(server.request_count(), 1001);
}

#[test]
fn an_empty_script_answers_script_exhausted_at_once() {
    let server = OauthServer::start(Vec::new());

    assert_eq!(
        post(&server, "/token", "a=b"),
        (500, r#"{"error":"script_exhausted"}"#.to_owned())
    );
    assert_eq!(server.request_count(), 1);
}

#[test]
fn each_request_is_recorded_with_its_path_and_decoded_form() {
    let server = OauthServer::start(vec![OauthReply::pending(), OauthReply::pending()]);
    assert_eq!(server.request_count(), 0);

    post(
        &server,
        "/device/token",
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=a+b%2Bc&empty=&flag",
    );
    post(&server, "/token", "refresh_token=r%zz&x=%2");

    assert_eq!(server.request_count(), 2);
    let requests = server.requests();
    assert_eq!(requests[0].path, "/device/token");
    assert_eq!(
        requests[0].form,
        [
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:device_code".to_owned()
            ),
            ("device_code".to_owned(), "a b+c".to_owned()),
            ("empty".to_owned(), String::new()),
            ("flag".to_owned(), String::new()),
        ]
    );
    assert_eq!(requests[1].path, "/token");
    assert_eq!(
        requests[1].form,
        [
            ("refresh_token".to_owned(), "r%zz".to_owned()),
            ("x".to_owned(), "%2".to_owned()),
        ]
    );
}

#[test]
fn a_body_with_no_fields_records_an_empty_form() {
    let server = OauthServer::start(vec![OauthReply::pending()]);

    post(&server, "/token", "");

    assert_eq!(server.requests()[0].form, []);
}

#[test]
fn a_plus_after_a_percent_is_not_a_hex_digit() {
    assert_eq!(percent_decode("%+1"), "% 1");
    assert_eq!(percent_decode("%1+"), "%1 ");
    assert_eq!(percent_decode("%e2%82%ac"), "\u{20ac}");
}

#[test]
fn concurrent_requests_each_take_one_reply_and_are_all_recorded() {
    let server = OauthServer::start(vec![
        OauthReply::pending(),
        OauthReply::pending(),
        OauthReply::pending(),
        OauthReply::pending(),
    ]);

    let statuses: Vec<u16> = thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|n| {
                let server = &server;
                scope.spawn(move || post(server, "/token", &format!("n={n}")).0)
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert_eq!(statuses, [400; 4]);
    assert_eq!(server.request_count(), 4);
    let mut seen: Vec<_> = server
        .requests()
        .into_iter()
        .map(|r| r.form[0].1.clone())
        .collect();
    seen.sort();
    assert_eq!(seen, ["0", "1", "2", "3"]);
}

#[test]
fn url_is_a_loopback_base() {
    let server = OauthServer::start(Vec::new());

    assert!(server.url().starts_with("http://127.0.0.1:"));
}

#[test]
fn percent_decode_handles_each_kind_of_byte() {
    assert_eq!(percent_decode("a+b"), "a b");
    assert_eq!(percent_decode("a%2Fb%2f"), "a/b/");
    assert_eq!(percent_decode("abc"), "abc");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%4"), "%4");
    assert_eq!(percent_decode("%zz1"), "%zz1");
    assert_eq!(percent_decode("%4z"), "%4z");
    assert_eq!(percent_decode("%%41"), "%A");
}

/// How long a test waits for something that should happen.
const WAIT: Duration = Duration::from_secs(5);

/// How long a test watches for something that should not happen.
const QUIET: Duration = Duration::from_millis(200);

/// POSTs on its own thread; the status and body arrive on the receiver.
fn post_later(server: &OauthServer) -> mpsc::Receiver<(u16, String)> {
    let (tx, rx) = mpsc::channel();
    let url = server.url();
    thread::spawn(move || {
        let addr = url.trim_start_matches("http://").to_owned();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream
            .write_all(b"POST /token HTTP/1.1\r\nHost: localhost\r\ncontent-length: 3\r\n\r\na=b")
            .unwrap();
        let mut text = String::new();
        // A drop that closes the connection unanswered sends nothing.
        if stream.read_to_string(&mut text).is_err() || text.is_empty() {
            return;
        }
        let status = text.split(' ').nth(1).unwrap().parse().unwrap();
        let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
        tx.send((status, body)).unwrap();
    });
    rx
}

#[test]
fn hold_records_the_request_and_replies_only_after_release() {
    let server = OauthServer::start(vec![OauthReply::token("at", "rt", 3600)]);
    server.hold();
    let rx = post_later(&server);

    assert!(
        server.await_requests(1, WAIT),
        "the held request is recorded"
    );
    assert_eq!(
        server.requests()[0].form,
        [("a".to_owned(), "b".to_owned())]
    );
    assert!(rx.recv_timeout(QUIET).is_err(), "no reply while held");

    server.release();
    let (status, body) = rx.recv_timeout(WAIT).expect("release sends the reply");
    assert_eq!(status, 200);
    assert!(body.contains(r#""access_token":"at""#), "{body}");
}

#[test]
fn release_with_nothing_held_leaves_replies_immediate() {
    let server = OauthServer::start(vec![OauthReply::pending()]);
    server.release();

    assert_eq!(post(&server, "/token", "").0, 400);
}

#[test]
fn await_requests_is_false_at_its_deadline_and_true_once_they_arrive() {
    let server = OauthServer::start(vec![OauthReply::pending()]);

    assert!(server.await_requests(0, Duration::ZERO));
    assert!(!server.await_requests(1, QUIET));
    post(&server, "/token", "");
    assert!(server.await_requests(1, Duration::ZERO));
    assert!(!server.await_requests(2, QUIET));
}

#[test]
fn dropping_a_holding_server_releases_its_client() {
    let server = OauthServer::start(vec![OauthReply::pending()]);
    server.hold();
    let rx = post_later(&server);
    assert!(server.await_requests(1, WAIT));

    drop(server);
    let (status, _) = rx
        .recv_timeout(WAIT)
        .expect("the drop sends the held reply");
    assert_eq!(status, 400);
}

#[test]
fn each_request_records_its_raw_body() {
    let server = OauthServer::start(vec![OauthReply::pending(), OauthReply::pending()]);
    post(&server, "/device/token", r#"{"client_id":"app_x"}"#);
    post(&server, "/token", "");

    let requests = server.requests();
    assert_eq!(requests[0].body, r#"{"client_id":"app_x"}"#);
    assert_eq!(requests[1].body, "");
}

#[test]
fn jwt_builds_three_parts_with_the_claims_in_the_middle() {
    let token = jwt(&serde_json::json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acct_1" },
        "exp": 1791403200,
    }));
    let parts: Vec<&str> = token.split('.').collect();
    let [header, claims, signature] = parts.as_slice() else {
        panic!("not three parts: {token}");
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&base64url_decode(header)).unwrap(),
        serde_json::json!({"alg": "none"})
    );
    let decoded: serde_json::Value = serde_json::from_slice(&base64url_decode(claims)).unwrap();
    assert_eq!(
        decoded["https://api.openai.com/auth"]["chatgpt_account_id"],
        "acct_1"
    );
    assert_eq!(decoded["exp"], 1791403200);
    assert_eq!(*signature, "");
    assert!(
        !token.contains('=') && !token.contains('+') && !token.contains('/'),
        "{token}"
    );
}

/// Decodes base64url without padding, the inverse of the test helper's
/// encoding, so the test reads what `jwt` wrote.
fn base64url_decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut bits: u32 = 0;
    let mut held = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => panic!("not base64url: {text}"),
        } as u32;
        bits = (bits << 6) | value;
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push(((bits >> held) & 0xff) as u8);
        }
    }
    out
}
