use std::io::{Read, Write};
use std::net::TcpStream;
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
