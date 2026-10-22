use super::*;
use fakes::Deadline;

fn lua() -> Lua {
    lua_in(PathBuf::from("/nonexistent-fiber-home"), &[])
}

/// Lua whose `host.secret` reads `secrets` from `home`.
fn lua_in(home: PathBuf, secrets: &[&str]) -> Lua {
    let lua = Lua::new();
    let hub = crate::lua::Hub::new(fakes::clock::FakeClock::new());
    let failures = failure::install(&lua).unwrap();
    install(
        &lua,
        HostContext {
            home,
            workspace: PathBuf::from("/nonexistent-workspace"),
            extension: "fiber.test/x".to_owned(),
            session: None,
            memory_cap: crate::MEMORY_CAP,
            secrets: secrets.iter().map(|name| (*name).to_owned()).collect(),
        },
        Arc::new(crate::SystemBrowser::default()),
        Rc::default(),
        &hub,
        failures.failure,
        failures.note_failure,
    )
    .unwrap();
    lua
}

/// Lua with the prelude installed, so `pcall` catches a failure as its table.
fn lua_prelude(home: PathBuf, secrets: &[&str]) -> Lua {
    let lua = lua_in(home, secrets);
    let clock = fakes::clock::FakeClock::new();
    let deadline = crate::lua::Deadline::new(clock);
    let dir = fakes::TempDir::new("fiber-host-prelude");
    crate::lua::install_prelude(&lua, &deadline, dir.path().to_path_buf(), crate::MEMORY_CAP)
        .unwrap();
    lua
}

#[track_caller]
fn eval(lua: &Lua, code: &str) -> Value {
    to_json(&lua.load(code).eval::<LuaValue>().unwrap()).unwrap()
}

#[test]
fn host_emit_rejects_values_past_the_shared_json_depth() {
    let lua = lua();
    let script = format!(
        "local t={{}}; for _=2,{} do t={{t}} end; local ok,e=pcall(host.emit,t); return {{ok=ok, message=tostring(e)}}",
        MAX_DEPTH + 1
    );
    let result = eval(&lua, &script);
    assert_eq!(result["ok"], false);
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains(&format!("nests deeper than {MAX_DEPTH}"))
    );
}

#[test]
fn a_table_nested_to_the_shared_json_depth_converts() {
    // 125 is written out, not MAX_DEPTH: the limit's own arithmetic must not
    // be able to move this bound with it.
    let lua = lua();
    let value = eval(&lua, "local t={}; for _=2,125 do t={t} end; return t");
    assert!(value.is_array());
}

#[test]
fn an_empty_table_encodes_as_an_object_but_returns_as_a_list() {
    let lua = lua();
    // `json.encode` keeps the empty table's object shape, while a callback
    // returning the same table through the host boundary reads it as a list.
    let encoded: String = lua.load("return json.encode({})").eval().unwrap();
    assert_eq!(encoded, "{}");
    assert_eq!(eval(&lua, "return {}"), serde_json::json!([]));
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
    let lua = lua_in(PathBuf::from("/nonexistent-fiber-home"), &["nope"]);
    assert_eq!(eval(&lua, "return host.secret('nope')"), Value::Null);
}

#[test]
fn a_declared_secret_that_is_stored_reads_trimmed() {
    let home = fakes::TempDir::new("fiber-host-secret");
    let home = home.path().to_path_buf();
    config::store_secret(&home, "token", &config::Secret::new("  s3cr3t \n".into())).unwrap();
    let lua = lua_in(home, &["token"]);
    assert_eq!(eval(&lua, "return host.secret('token')"), "s3cr3t");
}

/// The string `host.secret` raises for a name the manifest does not list.
const UNDECLARED: &str = "host.secret: `other.key` is not in the manifest's `secrets`";

#[test]
fn an_undeclared_secret_is_a_string_error_even_when_stored() {
    let home = fakes::TempDir::new("fiber-host-secret");
    let home = home.path().to_path_buf();
    config::store_secret(&home, "other.key", &config::Secret::new("s3cr3t".into())).unwrap();
    config::store_secret(&home, "token", &config::Secret::new("t0k3n".into())).unwrap();
    let lua = lua_prelude(home.clone(), &["a", "token"]);
    assert_eq!(eval(&lua, "return host.secret('token')"), "t0k3n");
    let message = pcall_string_of(&lua, "return host.secret('other.key')");
    assert!(message.ends_with(UNDECLARED), "{message}");
    assert!(!message.contains("s3cr3t"), "{message}");
    // A raw `coroutine.resume`, which the prelude's `pcall` never sees,
    // catches the same string.
    let lua = lua_in(home, &["a", "token"]);
    let (ok, err): (bool, LuaValue) = lua
        .load(
            "return coroutine.resume(coroutine.create(function() \
             return host.secret('other.key') end))",
        )
        .eval()
        .unwrap();
    assert!(!ok);
    let LuaValue::String(message) = err else {
        panic!("host.secret raised no string error: {err:?}");
    };
    let message = message.to_str().unwrap().to_owned();
    assert!(message.ends_with(UNDECLARED), "{message}");
}

/// Present in a re-executed child, absent in the parent.
const PROXY_CHILD: &str = "FIBER_TEST_HOST_HTTP_PROXY_CHILD";

/// The server URL, passed to the child on its environment.
const PROXY_CHILD_SERVER: &str = "FIBER_TEST_HOST_HTTP_SERVER";

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
    let proxy_url = proxy.url();
    let server_url = server.url();
    let output = fakes::rerun(
        test,
        &[
            (PROXY_CHILD, "1"),
            ("HTTPS_PROXY", proxy_url.as_str()),
            (PROXY_CHILD_SERVER, server_url.as_str()),
        ],
    );
    assert!(
        output.status.success(),
        "the proxy-env child called through the proxy:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
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
    let proxy_url = proxy.url();
    let server_url = server.url();
    let output = fakes::rerun(
        "host::tests::host_http_bypasses_the_proxy_for_no_proxy_hosts",
        &[
            (PROXY_CHILD, "1"),
            ("HTTPS_PROXY", proxy_url.as_str()),
            ("NO_PROXY", "127.0.0.1"),
            (PROXY_CHILD_SERVER, server_url.as_str()),
        ],
    );
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

fn get(
    url: &str,
    timeout: Option<Duration>,
) -> Result<(u16, Vec<u8>), (contract::ErrorCode, String)> {
    perform(&HttpRequest {
        method: "GET".to_owned(),
        url: url.to_owned(),
        headers: Vec::new(),
        body: None,
        timeout,
    })
}

#[test]
fn a_refused_connection_is_connection_failed() {
    let (code, message) = get("http://127.0.0.1:1/", None).unwrap_err();
    assert_eq!(code, contract::ErrorCode::ConnectionFailed);
    assert!(message.starts_with("host.http: "), "{message}");
}

/// The bound of `await_requests` on the silent server's recorded request.
const ACCEPT_WITHIN: Duration = Duration::from_secs(10);
/// How long the test waits for the blocking `get` before failing it.
const GET_WITHIN: Duration = Duration::from_secs(10);

#[test]
fn a_silent_server_past_the_backstop_is_timeout() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    // Hold the reply so the server records the request and never answers:
    // the read, not the connect, passes the backstop.
    server.hold();
    let (tx, rx) = std::sync::mpsc::channel();
    let url = format!("{}/", server.url());
    std::thread::spawn(move || {
        tx.send(get(&url, Some(Duration::from_millis(300))))
            .unwrap_or(());
    });
    let (code, message) = Deadline::after(GET_WITHIN)
        .recv(&rx)
        .expect("waited {GET_WITHIN:?} for the silent-server get")
        .unwrap_err();
    assert!(
        server.await_requests(1, Deadline::after(ACCEPT_WITHIN)),
        "the silent server never received the request"
    );
    assert_eq!(code, contract::ErrorCode::Timeout, "{message}");
    assert!(message.starts_with("host.http: "), "{message}");
}

/// The failure a prelude `pcall` of `code` catches: its code and message.
#[track_caller]
fn pcall_of(lua: &Lua, code: &str) -> (String, String) {
    let (ok, err): (bool, LuaValue) = lua
        .load(format!("return pcall(function() {code} end)"))
        .eval()
        .unwrap();
    assert!(!ok, "{code} unexpectedly succeeded");
    let LuaValue::Table(failed) = err else {
        panic!("{code} raised no failure table");
    };
    (failed.get("code").unwrap(), failed.get("message").unwrap())
}

/// The string a prelude `pcall` of `code` catches: a wrong argument stays
/// a string error (ruling 17).
#[track_caller]
fn pcall_string_of(lua: &Lua, code: &str) -> String {
    let (ok, err): (bool, LuaValue) = lua
        .load(format!("return pcall(function() {code} end)"))
        .eval()
        .unwrap();
    assert!(!ok, "{code} unexpectedly succeeded");
    let LuaValue::String(err) = err else {
        panic!("{code} raised no string error: {err:?}");
    };
    err.to_str().unwrap().to_owned()
}

#[test]
fn a_secret_with_a_bad_name_is_a_string_at_its_source() {
    // Even a raw `coroutine.resume`, which the prelude's `pcall` never
    // sees, catches the string: the Lua half raises it at the call.
    let lua = lua_in(PathBuf::from("/nonexistent-fiber-home"), &["a/b"]);
    let (ok, err): (bool, LuaValue) = lua
        .load("return coroutine.resume(coroutine.create(function() return host.secret('a/b') end))")
        .eval()
        .unwrap();
    assert!(!ok);
    let LuaValue::String(message) = err else {
        panic!("host.secret raised no string error: {err:?}");
    };
    let message = message.to_str().unwrap().to_owned();
    assert!(message.contains("not a secret's name"), "{message}");
    let lua = lua_prelude(PathBuf::from("/nonexistent-fiber-home"), &["a/b"]);
    let kind: String = lua
        .load("local ok, err = pcall(function() return host.secret('a/b') end); return type(err)")
        .eval()
        .unwrap();
    assert_eq!(kind, "string");
}

#[test]
fn pkce_resolves_through_its_wrapper() {
    let lua = lua();
    let (ok, pair): (bool, LuaValue) = lua
        .load("return coroutine.resume(coroutine.create(host.oauth.pkce))")
        .eval()
        .unwrap();
    assert!(ok);
    let LuaValue::Table(pair) = pair else {
        panic!("host.oauth.pkce returned no table");
    };
    for field in ["challenge", "verifier"] {
        let value: String = pair.get(field).unwrap();
        assert_eq!(value.len(), 43, "{field}");
    }
}

#[test]
fn a_secret_with_a_bad_name_is_a_string_for_pcall() {
    let lua = lua_prelude(PathBuf::from("/nonexistent-fiber-home"), &["a/b"]);
    let message = pcall_string_of(&lua, "return host.secret('a/b')");
    assert!(message.contains("not a secret's name"), "{message}");
    // Undeclared, the same name is refused before its shape is looked at.
    let lua = lua_prelude(PathBuf::from("/nonexistent-fiber-home"), &[]);
    let message = pcall_string_of(&lua, "return host.secret('a/b')");
    assert!(
        message.ends_with("host.secret: `a/b` is not in the manifest's `secrets`"),
        "{message}"
    );
}

#[test]
fn an_unreadable_secret_is_io_failed() {
    use std::os::unix::fs::PermissionsExt;
    let home = fakes::TempDir::new("fiber-host-secret");
    let home = home.path().to_path_buf();
    config::store_secret(&home, "token", &config::Secret::new("s3cr3t".to_owned())).unwrap();
    let path = home.join("credentials").join("token");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let lua = lua_prelude(home, &["token"]);
    let (code, message) = pcall_of(&lua, "return host.secret('token')");
    assert_eq!(code, "io_failed");
    assert!(message.contains("token"), "{message}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// The `env` the yielded `host.exec` spec carries: resume the coroutine
/// that calls it and read the spec it yields.
fn yielded_env(lua: &Lua, call: &str) -> Value {
    let yielded: (LuaValue, LuaValue, LuaValue) = lua
        .load(format!(
            "local inner = coroutine.create(function() {call} end)\n\
             local _, tag, kind, spec = coroutine.resume(inner)\n\
             assert(kind == \"exec\"); return tag, kind, spec"
        ))
        .eval()
        .unwrap();
    assert!(matches!(yielded.0, LuaValue::Table(_)));
    assert_eq!(
        yielded.1,
        LuaValue::String(lua.create_string("exec").unwrap())
    );
    to_json(&yielded.2).unwrap()
}

#[test]
fn exec_env_is_forwarded_in_the_yielded_spec() {
    let lua = lua();
    let spec = yielded_env(
        &lua,
        "host.exec('sh', {'-c', 'x'}, {env = {FIBER_X = '1'}})",
    );
    assert_eq!(spec["program"], "sh");
    assert_eq!(spec["env"], serde_json::json!({"FIBER_X": "1"}));
}

#[test]
fn exec_env_nil_is_omitted_from_the_yielded_spec() {
    let lua = lua();
    let spec = yielded_env(&lua, "host.exec('sh', {})");
    assert!(spec.get("env").is_none(), "{spec}");
}

#[test]
fn exec_env_rejects_a_non_table_or_a_non_string_entry() {
    let lua = lua_prelude(PathBuf::from("/nonexistent-fiber-home"), &[]);
    for code in [
        "return host.exec('sh', {}, {env = 1})",
        "return host.exec('sh', {}, {env = 'x'})",
        "return host.exec('sh', {}, {env = {A = 1}})",
        "return host.exec('sh', {}, {env = {[1] = 'x'}})",
    ] {
        let message = pcall_string_of(&lua, code);
        assert_eq!(
            message, "host.exec: `env` must be a table of strings",
            "{code}"
        );
    }
}

#[test]
fn exec_input_is_forwarded_in_the_yielded_spec() {
    let lua = lua();
    let spec = yielded_env(&lua, "host.exec('sh', {}, {input = 'abc'})");
    assert_eq!(spec["input"], "abc");
}

#[test]
fn exec_input_nil_is_omitted_from_the_yielded_spec() {
    let lua = lua();
    let spec = yielded_env(&lua, "host.exec('sh', {})");
    assert!(spec.get("input").is_none(), "{spec}");
}

#[test]
fn exec_input_rejects_a_non_string() {
    let lua = lua_prelude(PathBuf::from("/nonexistent-fiber-home"), &[]);
    for code in [
        "return host.exec('sh', {}, {input = 1})",
        "return host.exec('sh', {}, {input = {}})",
        "return host.exec('sh', {}, {input = true})",
    ] {
        let message = pcall_string_of(&lua, code);
        assert_eq!(message, "host.exec: `input` must be a string", "{code}");
    }
}

#[test]
fn exec_request_reads_input_as_bytes_and_absent_as_none() {
    let lua = Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("program", "sh").unwrap();
    spec.set("args", lua.create_table().unwrap()).unwrap();
    let read = exec_request_of(&spec).unwrap();
    assert_eq!(read.input, None, "absent input is null stdin");
    spec.set("input", "").unwrap();
    let read = exec_request_of(&spec).unwrap();
    assert_eq!(
        read.input,
        Some(Vec::new()),
        "empty input streams empty, then EOF"
    );
    spec.set("input", "abc").unwrap();
    let read = exec_request_of(&spec).unwrap();
    assert_eq!(read.input, Some(b"abc".to_vec()));
}

#[test]
fn exec_request_rejects_a_non_string_input() {
    let lua = Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("program", "sh").unwrap();
    spec.set("args", lua.create_table().unwrap()).unwrap();
    spec.set("input", 1).unwrap();
    let message = match exec_request_of(&spec) {
        Ok(_) => panic!("a non-string input never reads"),
        Err(message) => message,
    };
    assert_eq!(message, "host.exec: `input` must be a string");
}

/// An `exec` spec table with `env` built from `pairs`.
fn exec_spec(lua: &Lua, pairs: &[(&[u8], &[u8])]) -> Table {
    let spec = lua.create_table().unwrap();
    spec.set("program", "sh").unwrap();
    spec.set("args", lua.create_table().unwrap()).unwrap();
    let env = lua.create_table().unwrap();
    for (name, value) in pairs {
        env.set(
            lua.create_string(*name).unwrap(),
            lua.create_string(*value).unwrap(),
        )
        .unwrap();
    }
    spec.set("env", env).unwrap();
    spec
}

#[track_caller]
fn exec_request_of(spec: &Table) -> Result<exec::ExecRequest, String> {
    let text = super::request_from(
        "exec",
        Some(&LuaValue::Table(spec.clone())),
        None,
        std::path::Path::new("/w"),
        CAP,
    )
    .map_err(|error| {
        let text = error.to_string();
        text.strip_prefix("runtime error: ")
            .unwrap_or(&text)
            .to_owned()
    })?;
    match text {
        Some(super::Request::Exec(read)) => Ok(read),
        _ => panic!("an exec spec reads as an exec request"),
    }
}

const CAP: usize = 1 << 20;

#[test]
fn exec_request_reads_env_into_the_map() {
    let lua = Lua::new();
    let spec = exec_spec(&lua, &[(b"B", b"2"), (b"A", b"1")]);
    let read = exec_request_of(&spec).unwrap();
    assert_eq!(
        read.env.into_iter().collect::<Vec<_>>(),
        [
            ("A".to_owned(), "1".to_owned()),
            ("B".to_owned(), "2".to_owned())
        ]
    );
}

#[test]
fn exec_request_without_env_is_empty() {
    let lua = Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("program", "sh").unwrap();
    spec.set("args", lua.create_table().unwrap()).unwrap();
    let read = exec_request_of(&spec).unwrap();
    assert!(read.env.is_empty());
}

/// An `env` holding one `name` to `value` entry never reads: its message
/// is `expected`, which never echoes the value.
#[track_caller]
fn rejects_env(lua: &Lua, name: &[u8], value: &[u8], expected: &str) {
    let spec = exec_spec(lua, &[(name, value)]);
    let message = match exec_request_of(&spec) {
        Ok(_) => panic!("a bad env never reads"),
        Err(message) => message,
    };
    assert_eq!(message, expected);
    assert!(!message.contains("s3cret"), "{message}");
}

#[test]
fn exec_request_rejects_bad_names_and_nul_values_without_echoing_them() {
    let lua = Lua::new();
    let names = "host.exec: `env` names must not be empty or contain `=` or NUL";
    rejects_env(&lua, b"", b"s3cret-value", names);
    rejects_env(&lua, b"A=B", b"s3cret-value", names);
    rejects_env(&lua, b"A\0B", b"s3cret-value", names);
    rejects_env(
        &lua,
        b"A",
        b"x\0y-s3cret",
        "host.exec: `env` values must not contain NUL",
    );
}

#[test]
fn exec_request_rejects_a_non_table_env() {
    let lua = Lua::new();
    let spec = lua.create_table().unwrap();
    spec.set("program", "sh").unwrap();
    spec.set("args", lua.create_table().unwrap()).unwrap();
    spec.set("env", 1).unwrap();
    let message = match exec_request_of(&spec) {
        Ok(_) => panic!("a non-table env never reads"),
        Err(message) => message,
    };
    assert_eq!(message, "host.exec: `env` must be a table of strings");
}
