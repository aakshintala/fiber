use super::*;

fn lua() -> Lua {
    let lua = Lua::new();
    install(&lua, PathBuf::from("/nonexistent-fiber-home")).unwrap();
    lua
}

fn eval(lua: &Lua, code: &str) -> Value {
    to_json(&lua.load(code).eval::<LuaValue>().unwrap()).unwrap()
}

#[test]
fn a_table_is_an_array_only_when_its_keys_are_one_to_its_size() {
    let lua = lua();
    assert_eq!(eval(&lua, "return {}"), serde_json::json!([]));
    assert_eq!(
        eval(&lua, "return {'a', 'b'}"),
        serde_json::json!(["a", "b"])
    );
    assert_eq!(
        eval(&lua, "return {[1] = 'a', [3] = 'c', x = 1}"),
        serde_json::json!({ "1": "a", "3": "c", "x": 1 })
    );
    assert_eq!(
        eval(&lua, "return {[2] = 'b', [1] = 'a'}"),
        serde_json::json!(["a", "b"])
    );
    assert_eq!(
        eval(&lua, "return {[0] = 'z'}"),
        serde_json::json!({ "0": "z" })
    );
    assert_eq!(
        eval(&lua, "return {n = 1.5, i = 2, t = true}"),
        serde_json::json!({ "n": 1.5, "i": 2, "t": true })
    );
}

#[test]
fn what_json_cannot_hold_is_an_error() {
    let lua = lua();
    for code in [
        "local t = {} t.t = t return t",
        "return { f = print }",
        "return 0/0",
        "return { [true] = 1 }",
    ] {
        let value = lua.load(code).eval::<LuaValue>();
        let value = value.unwrap_or(LuaValue::Function(
            lua.create_function(|_, ()| Ok(())).unwrap(),
        ));
        assert!(to_json(&value).is_err(), "{code}");
    }
    // A lightuserdata other than the null sentinel is not JSON null.
    assert!(
        to_json(&LuaValue::LightUserData(mlua::LightUserData(
            std::ptr::dangling_mut::<std::ffi::c_void>()
        )))
        .is_err()
    );
}

#[test]
fn json_keeps_nulls_and_the_shape_of_arrays_and_objects() {
    let lua = lua();
    for (text, encoded) in [
        ("[null]", "[null]"),
        (r#"{"a":null}"#, r#"{"a":null}"#),
        ("[]", "[]"),
        ("{}", "{}"),
        (
            r#"{"a":[null,{"b":null}],"c":[]}"#,
            r#"{"a":[null,{"b":null}],"c":[]}"#,
        ),
    ] {
        lua.globals().set("text", text).unwrap();
        let got: String = lua
            .load("return json.encode(json.decode(text))")
            .eval()
            .unwrap();
        assert_eq!(got, encoded, "{text}");
    }
}

#[test]
fn json_round_trips_through_lua() {
    let lua = lua();
    let value = serde_json::json!({ "a": [1, 2.5, "x", true], "b": { "c": "d" } });
    lua.globals()
        .set("v", to_lua(&lua, &value).unwrap())
        .unwrap();
    assert_eq!(eval(&lua, "return json.decode(json.encode(v))"), value);
    assert_eq!(eval(&lua, "return json.decode('null')"), Value::Null);
    assert!(lua.load("return json.decode('{')").exec().is_err());
}

#[test]
fn the_hashes_match_their_published_test_vectors() {
    let lua = lua();
    // FIPS 180-2's "abc".
    assert_eq!(
        eval(&lua, "return host.sha256('abc')"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // RFC 4231, test case 2.
    let hex = "return (host.hmac_sha256('Jefe', 'what do ya want for nothing?'):gsub('.', \
               function(c) return string.format('%02x', c:byte()) end))";
    assert_eq!(
        eval(&lua, hex),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn a_secret_that_is_not_stored_is_nil() {
    assert_eq!(eval(&lua(), "return host.secret('nope')"), Value::Null);
}

/// Present in a re-executed child, absent in the parent.
const PROXY_CHILD: &str = "FIBER_TEST_HOST_HTTP_PROXY_CHILD";

/// The server URL, passed to the child on its environment.
const PROXY_CHILD_SERVER: &str = "FIBER_TEST_HOST_HTTP_SERVER";

/// How long the parent waits for the re-executed child to exit.
const CHILD_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the parent waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// Runs a GET in a child process whose environment names the proxy, and
/// checks the proxy recorded `target`. The child re-runs this same test,
/// which performs the request and fails the child when it does not arrive.
fn through_proxy_env(test: &str, server_path: &str) {
    if std::env::var_os(PROXY_CHILD).is_some() {
        let url = std::env::var(PROXY_CHILD_SERVER).unwrap();
        let request = HttpRequest {
            method: "GET".to_owned(),
            url: format!("{url}{server_path}"),
            headers: Vec::new(),
            body: None,
            timeout: None,
        };
        let (status, body) = perform(&request).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"{}");
        return;
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let target = format!("127.0.0.1:{port}");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(PROXY_CHILD, "1")
        .env("HTTPS_PROXY", proxy.url())
        .env(PROXY_CHILD_SERVER, server.url())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output().unwrap();
        let _ = done.send(output);
    });
    let output = match finished.recv_timeout(CHILD_WITHIN) {
        Ok(output) => output,
        Err(_) => panic!("waited {CHILD_WITHIN:?} for the proxy-env child"),
    };
    assert!(
        output.status.success(),
        "the proxy-env child called through the proxy:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn host_http_tunnels_through_the_proxy_environment() {
    through_proxy_env(
        "host::tests::host_http_tunnels_through_the_proxy_environment",
        "/thing",
    );
}

#[test]
fn host_http_bypasses_the_proxy_for_no_proxy_hosts() {
    if std::env::var_os(PROXY_CHILD).is_some() {
        let url = std::env::var(PROXY_CHILD_SERVER).unwrap();
        let request = HttpRequest {
            method: "GET".to_owned(),
            url: format!("{url}/thing"),
            headers: Vec::new(),
            body: None,
            timeout: None,
        };
        let (status, body) = perform(&request).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"{}");
        return;
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host::tests::host_http_bypasses_the_proxy_for_no_proxy_hosts",
            "--nocapture",
        ])
        .env(PROXY_CHILD, "1")
        .env("HTTPS_PROXY", proxy.url())
        .env("NO_PROXY", "127.0.0.1")
        .env(PROXY_CHILD_SERVER, server.url())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output().unwrap();
        let _ = done.send(output);
    });
    let output = match finished.recv_timeout(CHILD_WITHIN) {
        Ok(output) => output,
        Err(_) => panic!("waited {CHILD_WITHIN:?} for the proxy-env child"),
    };
    assert!(
        output.status.success(),
        "the proxy-env child bypassed the proxy:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        proxy.connects().is_empty(),
        "nothing went through the proxy"
    );
    assert_eq!(server.requests().len(), 1);
}
