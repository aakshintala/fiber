use super::*;
use crate::rpc::Outcome;
use crate::test_support::{FakeTokens, Setup, WITHIN};
use contract::mcp_tokens::{TokenFileError, TokenFiles};
use fakes::{Deadline, OauthReply, OauthServer};
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};

struct TestRefresh;
impl Refresh for TestRefresh {
    fn stopped(&self) -> bool {
        false
    }
    fn pause(&self) {
        std::thread::yield_now();
    }
    fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), ()> {
        use std::io::Read;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        let mut response = agent
            .post(url)
            .send_form(form.iter().copied())
            .map_err(|_| ())?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(crate::pipes::MAX_LINE as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| ())?;
        Ok((status, body))
    }
}
fn fixture(endpoint: &str, expiry: Option<u64>) -> (Setup, Arc<FakeTokens>, Arc<Bearer>) {
    let setup = Setup::new();
    let files = Arc::new(FakeTokens::default());
    let token = Token {
        access_token: contract::Secret::new("at-1".into()),
        refresh_token: Some(contract::Secret::new("rt-1".into())),
        expires_at: expiry,
        token_endpoint: endpoint.into(),
        client_id: "fiber-abc".into(),
    };
    store_token(files.as_ref(), "https://h/x", &token).unwrap();
    files.state.lock().unwrap().writes = 0;
    let bearer = Arc::new(Bearer::new("https://h/x", files.clone(), setup.clock()));
    (setup, files, bearer)
}
fn record(files: &FakeTokens) -> Value {
    files.read(&key("https://h/x")).unwrap().unwrap()
}
#[test]
fn a_token_debug_names_its_fields_and_hides_both_secrets() {
    let token = Token {
        access_token: contract::Secret::new("at-secret-7f3".into()),
        refresh_token: Some(contract::Secret::new("rt-secret-7f3".into())),
        expires_at: Some(1_700_000_000),
        token_endpoint: "https://auth.example/token".into(),
        client_id: "fiber-abc".into(),
    };
    let debug = format!("{token:?}");
    assert!(debug.contains("Token"), "debug names the type: {debug}");
    assert!(
        debug.contains("access_token"),
        "debug names access_token: {debug}"
    );
    assert!(
        debug.contains("refresh_token"),
        "debug names refresh_token: {debug}"
    );
    assert!(
        debug.contains("expires_at"),
        "debug names expires_at: {debug}"
    );
    assert!(
        !debug.contains("at-secret-7f3"),
        "debug hides the access token: {debug}"
    );
    assert!(
        !debug.contains("rt-secret-7f3"),
        "debug hides the refresh token: {debug}"
    );
}

fn auth(bearer: Arc<Bearer>) -> Result<Authorization, Outcome> {
    fakes::within("token authorization", Deadline::after(WITHIN), move || {
        bearer.authorization(&TestRefresh)
    })
}

#[test]
fn no_record_sends_nothing_and_takes_no_lock() {
    let setup = Setup::new();
    let files = Arc::new(FakeTokens::default());
    let bearer = Arc::new(Bearer::new("https://h/x", files.clone(), setup.clock()));
    assert_eq!(auth(bearer).unwrap(), Authorization::None);
    assert_eq!(files.state.lock().unwrap().locks, 0);
}
#[test]
fn expiry_edges_choose_wait_background_or_send() {
    let setup = Setup::new();
    let now = contract::clock::wall_ms(setup.clock().wall()) / 1000;
    for delta in [-1i64, 0, 1, 299, 300, 301] {
        let server = OauthServer::start(vec![OauthReply::token("at-2", "rt-2", 3600)]);
        let expiry = now.checked_add_signed(delta).unwrap();
        let (_setup, files, bearer) = fixture(&server.url(), Some(expiry));
        let got = auth(bearer.clone()).unwrap();
        match delta {
            -1 | 0 => {
                assert_eq!(got, Authorization::Send("Bearer at-2".into()));
                assert_eq!(server.request_count(), 1);
                assert_eq!(record(&files)["generation"], 2);
            }
            1 | 299 | 300 => {
                assert_eq!(got, Authorization::SendAndRefresh("Bearer at-1".into()));
                assert_eq!(server.request_count(), 0);
                assert_eq!(
                    auth(bearer).unwrap(),
                    Authorization::Send("Bearer at-1".into())
                );
            }
            301 => {
                assert_eq!(got, Authorization::Send("Bearer at-1".into()));
                assert_eq!(server.request_count(), 0);
            }
            _ => unreachable!(),
        }
    }
}
#[test]
fn a_valid_nonexpiring_record_sends_without_refresh() {
    let server = OauthServer::start(vec![]);
    let (_setup, _files, bearer) = fixture(&server.url(), None);
    assert_eq!(
        auth(bearer.clone()).unwrap(),
        Authorization::Send("Bearer at-1".into())
    );
    assert_eq!(bearer.stored().unwrap(), Some("Bearer at-1".into()));
    assert_eq!(server.request_count(), 0);
}
#[test]
fn refresh_rotates_and_sends_the_exact_resource_form() {
    let server = OauthServer::start(vec![OauthReply::token("at-2", "rt-2", 3600)]);
    let (setup, files, bearer) = fixture(&server.url(), Some(0));
    assert_eq!(
        auth(bearer).unwrap(),
        Authorization::Send("Bearer at-2".into())
    );
    let now = contract::clock::wall_ms(setup.clock().wall()) / 1000;
    assert_eq!(
        record(&files),
        json!({"url":"https://h/x", "generation":2, "access_token":"at-2", "refresh_token":"rt-2", "expires_at":now + 3600, "token_endpoint":server.url(), "client_id":"fiber-abc"})
    );
    assert_eq!(
        server.requests()[0].form,
        [
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), "rt-1".into()),
            ("client_id".into(), "fiber-abc".into()),
            ("resource".into(), "https://h/x".into())
        ]
    );
}
#[test]
fn refresh_saves_rotated_tokens_when_stopped_after_post() {
    struct StoppedAfterPost(AtomicBool);
    impl Refresh for StoppedAfterPost {
        fn stopped(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn pause(&self) {
            TestRefresh.pause();
        }
        fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), ()> {
            let reply = TestRefresh.post(url, form)?;
            self.0.store(true, Ordering::Release);
            Ok(reply)
        }
    }
    let server = OauthServer::start(vec![OauthReply::token("at-2", "rt-2", 3600)]);
    let (_setup, files, bearer) = fixture(&server.url(), Some(0));
    fakes::within(
        "refresh stopped after reply",
        Deadline::after(WITHIN),
        move || {
            let wait = StoppedAfterPost(AtomicBool::new(false));
            let result = bearer.authorization(&wait);
            assert!(wait.stopped());
            assert_eq!(record(&files)["generation"], 2);
            assert_eq!(record(&files)["refresh_token"], "rt-2");
            assert_eq!(result.unwrap(), Authorization::Send("Bearer at-2".into()));
        },
    );
}
#[test]
fn optional_refresh_fields_and_saturating_expiry() {
    for (body, expiry) in [
        (r#"{"access_token":"at-2"}"#.to_owned(), None),
        (
            json!({"access_token":"at-2", "expires_in":u64::MAX}).to_string(),
            Some(u64::MAX),
        ),
    ] {
        let server = OauthServer::start(vec![OauthReply::raw(200, &body)]);
        let (_setup, files, bearer) = fixture(&server.url(), Some(0));
        assert_eq!(
            auth(bearer).unwrap(),
            Authorization::Send("Bearer at-2".into())
        );
        let stored = record(&files);
        assert_eq!(stored["refresh_token"], "rt-1");
        assert_eq!(stored.get("expires_at").and_then(Value::as_u64), expiry);
    }
}
#[test]
fn rejected_or_unusable_replies_leave_the_record_unchanged() {
    for reply in [
        OauthReply::raw(400, "secret"),
        OauthReply::raw(401, "secret"),
        OauthReply::raw(200, "not json secret"),
        OauthReply::raw(200, "{}"),
        OauthReply::raw(200, r#"{"access_token":""}"#),
        OauthReply::raw(200, r#"{"access_token":"secret\r\n"}"#),
    ] {
        let server = OauthServer::start(vec![reply]);
        let (_setup, files, bearer) = fixture(&server.url(), Some(0));
        let before = record(&files);
        let error = auth(bearer).unwrap_err();
        assert!(matches!(error, Outcome::Login(_)));
        assert!(!format!("{error:?}").contains("secret"));
        assert_eq!(record(&files), before);
    }
}
#[test]
fn refresh_accepts_only_the_2xx_status_interval() {
    for (status, accepted) in [
        (199, false),
        (200, true),
        (201, true),
        (299, true),
        (300, false),
        (301, false),
    ] {
        let server = OauthServer::start(vec![OauthReply::raw(status, r#"{"access_token":"a"}"#)]);
        let (_setup, files, bearer) = fixture(&server.url(), Some(0));
        let before = record(&files);
        let result = auth(bearer);
        if accepted {
            assert_eq!(result.unwrap(), Authorization::Send("Bearer a".into()));
        } else {
            assert!(matches!(result, Err(Outcome::Login(_))));
            assert_eq!(record(&files), before);
        }
    }
}

#[test]
fn unusable_records_are_login_failures_without_a_request() {
    for field in [
        "refresh_token",
        "client_id",
        "generation",
        "access_token",
        "token_endpoint",
        "malformed",
        "userinfo",
        "header",
    ] {
        let server = OauthServer::start(vec![]);
        let (_setup, files, bearer) = fixture(&server.url(), Some(0));
        let mut value = record(&files);
        match field {
            "malformed" => {
                files.state.lock().unwrap().unreadable = true;
            }
            "userinfo" => {
                value["token_endpoint"] = json!("https://u:secret@auth.example/token");
            }
            "header" => {
                value["access_token"] = json!("secret\r\n");
            }
            field => {
                value.as_object_mut().unwrap().remove(field);
            }
        }
        files
            .state
            .lock()
            .unwrap()
            .records
            .insert(key("https://h/x"), value.clone());
        let error = auth(bearer).unwrap_err();
        assert!(matches!(error, Outcome::Login(_)), "{field}");
        assert!(!format!("{error:?}").contains("secret"));
        assert_eq!(server.request_count(), 0);
        assert_eq!(
            files.state.lock().unwrap().records[&key("https://h/x")],
            value
        );
    }
}
#[test]
fn an_unreachable_endpoint_has_its_own_sentence() {
    let (_setup, files, bearer) = fixture("http://127.0.0.1:1/token", Some(0));
    let before = record(&files);
    assert_eq!(
        auth(bearer).unwrap_err(),
        Outcome::Login("Fiber could not reach its token endpoint to renew the login.".into())
    );
    assert_eq!(record(&files), before);
}
#[test]
fn a_wrong_url_is_absent_and_never_refreshed_or_overwritten() {
    let server = OauthServer::start(vec![]);
    let (_setup, files, bearer) = fixture(&server.url(), Some(0));
    let mut value = record(&files);
    value["url"] = json!("https://elsewhere/x");
    files
        .state
        .lock()
        .unwrap()
        .records
        .insert(key("https://h/x"), value.clone());
    assert_eq!(auth(bearer.clone()).unwrap(), Authorization::None);
    assert_eq!(bearer.stored().unwrap(), None);
    assert_eq!(record(&files), value);
    assert_eq!(files.state.lock().unwrap().writes, 0);
    assert_eq!(server.request_count(), 0);
}
#[test]
fn write_failures_keep_the_correct_side_of_the_rename() {
    for after in [false, true] {
        let server = OauthServer::start(vec![OauthReply::token("at-2", "rt-2", 3600)]);
        let (_setup, files, bearer) = fixture(&server.url(), Some(0));
        let before = record(&files);
        files.state.lock().unwrap().failure = Some(after);
        assert!(matches!(auth(bearer.clone()), Err(Outcome::Login(_))));
        if after {
            assert_eq!(record(&files)["access_token"], "at-2");
            assert_eq!(
                auth(bearer).unwrap(),
                Authorization::Send("Bearer at-2".into())
            );
        } else {
            assert_eq!(record(&files), before);
        }
        assert_eq!(server.request_count(), 1);
    }
}
#[test]
fn the_reply_cap_includes_the_exact_limit() {
    for size in [
        crate::pipes::MAX_LINE - 1,
        crate::pipes::MAX_LINE,
        crate::pipes::MAX_LINE + 1,
    ] {
        let mut body = r#"{"access_token":"at-2"}"#.to_owned();
        body.extend(std::iter::repeat_n(' ', size - body.len()));
        let server = OauthServer::start(vec![OauthReply::raw(200, &body)]);
        let (_setup, files, bearer) = fixture(&server.url(), Some(0));
        let before = record(&files);
        let result = auth(bearer);
        if size > crate::pipes::MAX_LINE {
            assert!(matches!(result, Err(Outcome::Login(_))));
            assert_eq!(record(&files), before);
        } else {
            assert_eq!(result.unwrap(), Authorization::Send("Bearer at-2".into()));
        }
    }
}
#[test]
fn storing_increments_generations_refuses_userinfo_and_busy_files() {
    let (_setup, files, _bearer) = fixture("https://auth/token", None);
    let token = Token {
        access_token: contract::Secret::new("at-new".into()),
        refresh_token: None,
        expires_at: None,
        token_endpoint: "https://auth/token".into(),
        client_id: "fiber".into(),
    };
    assert_eq!(record(&files)["generation"], 1);
    store_token(files.as_ref(), "HTTPS://H:443/x#f", &token).unwrap();
    assert_eq!(record(&files)["generation"], 2);
    let before = record(&files);
    assert!(matches!(
        store_token(files.as_ref(), "https://u@h/x", &token),
        Err(TokenError::Url)
    ));
    let bad = Token {
        token_endpoint: "https://u:secret@auth/token".into(),
        ..token.clone()
    };
    assert!(matches!(
        store_token(files.as_ref(), "https://h/x", &bad),
        Err(TokenError::Url)
    ));
    let held = files.try_lock(&key("https://h/x")).unwrap().unwrap();
    assert!(matches!(
        store_token(files.as_ref(), "https://h/x", &token),
        Err(TokenError::Busy)
    ));
    assert_eq!(record(&files), before);
    drop(held);
    files.state.lock().unwrap().failure = Some(true);
    assert!(matches!(
        store_token(files.as_ref(), "https://h/x", &token),
        Err(TokenError::Files(TokenFileError::Unsynced(_)))
    ));
    assert_eq!(record(&files)["generation"], 3);
}
struct DoneRefresh(mpsc::Sender<()>);
impl Refresh for DoneRefresh {
    fn stopped(&self) -> bool {
        false
    }
    fn pause(&self) {
        TestRefresh.pause();
    }
    fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), ()> {
        TestRefresh.post(url, form)
    }
}
impl Drop for DoneRefresh {
    fn drop(&mut self) {
        self.0.send(()).unwrap();
    }
}
#[test]
fn background_refresh_clears_its_flag_after_each_completion() {
    let server = OauthServer::start(vec![
        OauthReply::token("at-2", "rt-2", 60),
        OauthReply::token("at-3", "rt-3", 60),
    ]);
    let setup = Setup::new();
    let now = contract::clock::wall_ms(setup.clock().wall()) / 1000;
    let (_setup, files, bearer) = fixture(&server.url(), Some(now + 60));
    assert_eq!(
        auth(bearer.clone()).unwrap(),
        Authorization::SendAndRefresh("Bearer at-1".into())
    );
    server.hold();
    let (finished, receiver) = mpsc::channel();
    bearer.refresh_in_background(Box::new(DoneRefresh(finished)));
    assert!(server.await_requests(1, Deadline::after(WITHIN)));
    assert_eq!(
        auth(bearer.clone()).unwrap(),
        Authorization::Send("Bearer at-1".into())
    );
    server.release();
    Deadline::after(WITHIN).recv_or_fail(&receiver, "background refresh ended");
    assert_eq!(record(&files)["generation"], 2);
    assert_eq!(
        auth(bearer.clone()).unwrap(),
        Authorization::SendAndRefresh("Bearer at-2".into())
    );
    let (finished, receiver) = mpsc::channel();
    bearer.refresh_in_background(Box::new(DoneRefresh(finished)));
    Deadline::after(WITHIN).recv_or_fail(&receiver, "second background refresh ended");
    assert_eq!(record(&files)["generation"], 3);
}
#[test]
fn background_refresh_stops_when_a_new_login_is_outside_the_window() {
    for expiry in [None, Some(301)] {
        let server = OauthServer::start(vec![]);
        let setup = Setup::new();
        let now = contract::clock::wall_ms(setup.clock().wall()) / 1000;
        let (_setup, files, bearer) = fixture(&server.url(), Some(now + 60));
        assert_eq!(
            auth(bearer.clone()).unwrap(),
            Authorization::SendAndRefresh("Bearer at-1".into())
        );
        store_token(
            files.as_ref(),
            "https://h/x",
            &Token {
                access_token: contract::Secret::new("at-new".into()),
                refresh_token: Some(contract::Secret::new("rt-new".into())),
                expires_at: expiry.map(|seconds| now + seconds),
                token_endpoint: server.url(),
                client_id: "fiber-abc".into(),
            },
        )
        .unwrap();
        let before = record(&files);
        let (finished, receiver) = mpsc::channel();
        bearer.refresh_in_background(Box::new(DoneRefresh(finished)));
        Deadline::after(WITHIN)
            .recv_or_fail(&receiver, "background refresh uses the new fresh login");
        assert_eq!(server.request_count(), 0);
        assert_eq!(record(&files), before);
        assert_eq!(
            auth(bearer).unwrap(),
            Authorization::Send("Bearer at-new".into())
        );
    }
}

#[test]
fn competing_refreshers_reread_the_generation_including_wrap_and_short_expiry() {
    for (generation, access, lifetime, waited_token, requests, final_lifetime) in [
        (1, "at-2", 3600, "at-2", 1, 3600),
        (1, "at-1", 3600, "at-1", 1, 3600),
        (1, "at-2", 60, "at-2", 1, 60),
        (u64::MAX, "at-1", 3600, "at-1", 1, 3600),
        (1, "at-2", 0, "at-3", 2, 3600),
        (1, "at-2", 1, "at-2", 1, 1),
    ] {
        let server = OauthServer::start(vec![
            OauthReply::token(access, "rt-2", lifetime),
            OauthReply::token("at-3", "rt-3", 3600),
        ]);
        server.hold();
        let (setup, files, first) = fixture(&server.url(), Some(0));
        files
            .state
            .lock()
            .unwrap()
            .records
            .get_mut(&key("https://h/x"))
            .unwrap()["generation"] = json!(generation);
        let second = Arc::new(Bearer::new("https://h/x", files.clone(), setup.clock()));
        let (finished_a, receive_a) = mpsc::channel();
        let a = std::thread::spawn(move || {
            finished_a.send(first.authorization(&TestRefresh)).unwrap();
        });
        assert!(server.await_requests(1, Deadline::after(WITHIN)));
        let (busy, receive_busy) = mpsc::channel();
        files.state.lock().unwrap().busy = Some(busy);
        let (finished_b, receive_b) = mpsc::channel();
        let b = std::thread::spawn(move || {
            finished_b.send(second.authorization(&TestRefresh)).unwrap();
        });
        Deadline::after(WITHIN).recv_or_fail(&receive_busy, "second refresher polls the held lock");
        server.release();
        let deadline = Deadline::after(WITHIN);
        assert_eq!(
            deadline
                .recv_or_fail(&receive_a, "first refresher ends")
                .unwrap(),
            Authorization::Send(format!("Bearer {access}"))
        );
        assert_eq!(
            deadline
                .recv_or_fail(&receive_b, "second refresher ends")
                .unwrap(),
            Authorization::Send(format!("Bearer {waited_token}"))
        );
        a.join().unwrap();
        b.join().unwrap();
        assert_eq!(server.request_count(), requests);
        assert_eq!(files.state.lock().unwrap().writes, requests);
        assert_eq!(
            record(&files)["generation"],
            generation.wrapping_add(u64::try_from(requests).unwrap())
        );
        let now = contract::clock::wall_ms(setup.clock().wall()) / 1000;
        assert_eq!(record(&files)["expires_at"], now + final_lifetime);
    }
}

#[test]
fn a_new_login_starts_at_generation_one_when_the_old_record_cannot_parse() {
    let (_setup, files, _bearer) = fixture("https://auth/token", None);
    files.state.lock().unwrap().unreadable = true;
    let token = Token {
        access_token: contract::Secret::new("new-login".into()),
        refresh_token: None,
        expires_at: None,
        token_endpoint: "https://auth/token".into(),
        client_id: "fiber".into(),
    };
    store_token(files.as_ref(), "https://h/x", &token).unwrap();
    files.state.lock().unwrap().unreadable = false;
    assert_eq!(record(&files)["generation"], 1);
    assert_eq!(record(&files)["access_token"], "new-login");
}

#[test]
fn token_error_codes_are_stable() {
    assert_eq!(
        TokenError::Url.code(),
        contract::ErrorCode::InvalidArguments
    );
    assert_eq!(TokenError::Busy.code(), contract::ErrorCode::IoFailed);
    assert_eq!(
        TokenError::Files(TokenFileError::Unsynced("path".into())).code(),
        contract::ErrorCode::IoFailed
    );
}
#[test]
fn authorization_debug_redacts_both_sending_variants() {
    for authorization in [
        Authorization::Send("Bearer secret-header".into()),
        Authorization::SendAndRefresh("Bearer secret-header".into()),
    ] {
        let debug = format!("{authorization:?}");
        assert!(!debug.contains("secret-header"));
        assert!(debug.contains("<redacted>"));
    }
    assert_eq!(format!("{:?}", Authorization::None), "None");
}

#[test]
fn token_debug_redacts_tokens_and_endpoint_credentials() {
    let token = Token {
        access_token: contract::Secret::new("access-secret".into()),
        refresh_token: Some(contract::Secret::new("refresh-secret".into())),
        expires_at: None,
        token_endpoint: "https://u:pw@auth.example.com/token?k=s3cr3t".into(),
        client_id: "fiber".into(),
    };
    let debug = format!("{token:?}");
    for secret in ["access-secret", "refresh-secret", "pw", "s3cr3t"] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn canonical_urls_keep_paths_and_queries_byte_for_byte() {
    let rows = [
        (
            "HTTPS://MCP.Example.COM:443/A/b/?Q=1#frag",
            Some("https://mcp.example.com/A/b/?Q=1"),
        ),
        ("http://h:80/x", Some("http://h/x")),
        ("http://h:443/x", Some("http://h:443/x")),
        ("https://h:80", Some("https://h:80")),
        ("https://h:8443", Some("https://h:8443")),
        ("https://h", Some("https://h")),
        ("https://h/", Some("https://h/")),
        ("https://h/x?", Some("https://h/x?")),
        ("https://[::1]:443/x", Some("https://[::1]/x")),
        ("ws://h/x", None),
        ("h/x", None),
        ("https://", None),
        ("", None),
        ("https://EX%4Aample.com/x", Some("https://ex%4aample.com/x")),
        ("https://:443/x", None),
        ("https://h:/x", Some("https://h/x")),
        ("https://h:0443/x", Some("https://h/x")),
        ("https://h:08443/x", Some("https://h:8443/x")),
        ("https://h:65534/x", Some("https://h:65534/x")),
        ("https://h:65535/x", Some("https://h:65535/x")),
        ("https://h:65536/x", None),
        ("https://h:xyz/x", None),
        ("https://u@h/x", None),
        ("https://u:p@h/x", None),
        ("https://@h/x", None),
        ("https://a@b@H/x", None),
        ("https://a?b@H/x", Some("https://a?b@H/x")),
        ("https://h/x?u@v", Some("https://h/x?u@v")),
        ("https://h/p@q", Some("https://h/p@q")),
        ("https://h/x#u@v", Some("https://h/x")),
        ("https://h/x#", Some("https://h/x")),
    ];
    for (url, expected) in rows {
        match expected {
            Some(expected) => assert_eq!(canonical_url(url).unwrap(), expected, "{url}"),
            None => assert!(matches!(canonical_url(url), Err(TokenError::Url)), "{url}"),
        }
    }
}

#[test]
fn userinfo_is_only_in_the_authority() {
    for url in [
        "https://u@h/x",
        "https://u:p@h/x",
        "https://@h/x",
        "https://a@b@H/x",
    ] {
        assert!(url_has_userinfo(url), "{url}");
    }
    for url in [
        "https://a?b@H/x",
        "https://h/x?u@v",
        "https://h/p@q",
        "https://h/x#u@v",
        "https://h/x",
    ] {
        assert!(!url_has_userinfo(url), "{url}");
    }
}

#[test]
fn a_key_is_the_sha256_of_the_canonical_url_bytes() {
    assert_eq!(
        key("https://h/x"),
        "29c7358107b5adaa7556089816b5ca38ac6875c87d0e7bbbb0f487ea09ee736d"
    );
    assert_eq!(
        key(&canonical_url("HTTPS://H:443/x#f").unwrap()),
        key(&canonical_url("https://h/x").unwrap())
    );
}

#[test]
fn token_key_hashes_the_canonical_url() {
    assert_eq!(
        token_key("https://h/x").unwrap(),
        "29c7358107b5adaa7556089816b5ca38ac6875c87d0e7bbbb0f487ea09ee736d"
    );
    assert_eq!(
        token_key("HTTPS://H:443/x#f").unwrap(),
        token_key("https://h/x").unwrap()
    );
    assert!(matches!(token_key("https://u@h/x"), Err(TokenError::Url)));
    assert!(matches!(token_key("ws://h/x"), Err(TokenError::Url)));
}
