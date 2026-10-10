//! `fiber.search_backend`'s registration checks (`docs/extensions.md`,
//! "Registering") and the backend calls Fiber's own `web_search` makes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::json;

use super::super::hub::Progress;
use super::*;

/// Wall-clock bound on every wait for the extension.
const WAIT: Duration = Duration::from_secs(5);

/// An extension named `fiber.test/t` whose entry script is `init`, in a
/// fresh temporary directory kept beside it.
fn extension(init: &str) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("fiber-lua-search");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let ext = LuaExtension::new(
        "fiber.test/t",
        dir.path(),
        "/nonexistent-fiber-home",
        FakeClock::new(),
    );
    (dir, Arc::new(ext))
}

/// Runs `read` on the extension's registrations on a thread, under `WAIT`.
#[track_caller]
fn registered<T: Send + 'static>(
    ext: &Arc<LuaExtension>,
    read: impl Fn(&super::super::CallbackTimeouts) -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.registered(read)));
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the entry script")
        .expect("the entry script ran")
}

#[track_caller]
fn backends(ext: &Arc<LuaExtension>) -> BTreeMap<String, Duration> {
    registered(ext, |timeouts| timeouts.search.clone())
}

#[track_caller]
fn problems(ext: &Arc<LuaExtension>) -> Vec<String> {
    registered(ext, |timeouts| timeouts.hooks.problems.clone())
}

/// A whole spec as Lua source, with `field` set to `value`, or left out
/// when `value` is empty.
fn spec_with(field: &str, value: &str) -> String {
    let mut fields = vec![("timeout", "100"), ("run", "function() return {} end")];
    for slot in &mut fields {
        if slot.0 == field {
            slot.1 = value;
        }
    }
    let body: Vec<String> = fields
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key} = {value}"))
        .collect();
    format!("{{ {} }}", body.join(", "))
}

#[test]
fn a_backend_registers_its_name_and_timeout() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"brave\", { timeout = 2000, run = function() return {} end })\n",
    );
    assert_eq!(
        backends(&ext),
        BTreeMap::from([("brave".to_owned(), Duration::from_millis(2000))])
    );
    assert_eq!(
        ext.search_backends().unwrap(),
        vec!["brave".to_owned()],
        "the backends list the registered names, sorted"
    );
    assert!(problems(&ext).is_empty());
}

#[test]
fn backend_names_list_sorted() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"zebra\", { timeout = 100, run = function() return {} end })\n\
         fiber.search_backend(\"apple\", { timeout = 100, run = function() return {} end })\n",
    );
    assert_eq!(
        ext.search_backends().unwrap(),
        vec!["apple".to_owned(), "zebra".to_owned()]
    );
}

#[test]
fn each_bad_spec_leaves_one_problem_and_the_rest_stand() {
    let timeout = "`timeout` must be a whole number of milliseconds above 0";
    let cases = [
        ("timeout", "", "missing `timeout`"),
        ("timeout", "0", timeout),
        ("timeout", "-1", timeout),
        ("timeout", "1.5", timeout),
        ("timeout", "\"100\"", timeout),
        ("run", "", "`run` must be a function"),
        ("run", "\"go\"", "`run` must be a function"),
    ];
    for (field, value, why) in cases {
        let (_dir, ext) = extension(&format!(
            "fiber.search_backend(\"bad\", {})\n\
             fiber.search_backend(\"good\", {})\n",
            spec_with(field, value),
            spec_with("", ""),
        ));
        assert_eq!(
            problems(&ext),
            vec![format!("`bad` search backend not registered: {why}")],
            "{field} = {value:?}"
        );
        assert_eq!(
            backends(&ext).into_keys().collect::<Vec<_>>(),
            ["good"],
            "{field} = {value:?}"
        );
    }
}

#[test]
fn a_name_must_be_a_non_empty_string_and_a_spec_a_table() {
    let (_dir, ext) = extension(&format!(
        "fiber.search_backend(42, {})\n\
         fiber.search_backend(\"\", {})\n\
         fiber.search_backend(\"x\", \"run\")\n\
         fiber.search_backend(\"good\", {})\n",
        spec_with("", ""),
        spec_with("", ""),
        spec_with("", ""),
    ));
    assert_eq!(
        problems(&ext),
        [
            "`42` search backend not registered: the name must be a string",
            "`` search backend not registered: the name must not be empty",
            "`x` search backend not registered: it takes a table of `timeout` and `run`",
        ]
    );
    assert_eq!(backends(&ext).into_keys().collect::<Vec<_>>(), ["good"]);
}

#[test]
fn a_timeout_of_one_millisecond_registers() {
    let (_dir, ext) = extension(&format!(
        "fiber.search_backend(\"x\", {})\n",
        spec_with("timeout", "1")
    ));
    assert_eq!(
        backends(&ext).get("x").copied(),
        Some(Duration::from_millis(1))
    );
}

#[test]
fn fiber_search_backend_after_the_entry_script_raises_and_registers_nothing() {
    let (_dir, ext) = extension(&format!(
        "fiber.search_backend(\"first\", {})\n\
         fiber.command(\"late\", {{ timeout = 1000, run = function()\n\
           local ok, e = pcall(function() fiber.search_backend(\"late\", {}) end)\n\
           return tostring(ok) .. \" \" .. tostring(e)\n\
         end }})\n",
        spec_with("", ""),
        spec_with("", ""),
    ));
    let before = ext.search_backends().unwrap();
    let (tx, rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    std::thread::spawn(move || tx.send(caller.command("late", "")));
    let said = Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the command")
        .unwrap();
    assert_eq!(
        said,
        "false init.lua:3: fiber.search_backend: a backend registers only while `init.lua` runs"
    );
    assert_eq!(before, ["first"]);
    assert_eq!(ext.search_backends().unwrap(), before);
    assert!(problems(&ext).is_empty());
}

#[test]
fn a_backend_and_a_tool_of_the_same_name_both_register() {
    let (_dir, ext) = extension(&format!(
        "fiber.tool(\"x\", {{ description = \"d\", input_schema = {{ type = \"object\" }}, \
           effects = {{ effects = {{}}, reversible = true }}, timeout = 100, run = function() return \"\" end }})\n\
         fiber.search_backend(\"x\", {})\n",
        spec_with("", ""),
    ));
    assert!(registered(&ext, |t| t.tools.contains_key("x")));
    assert!(backends(&ext).contains_key("x"));
}

/// Runs `call` on its own thread and hands back its result.
fn on_thread<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || match tx.send(call()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    rx
}

/// `require("go_<name>")` in `dir` signals that the callback has started.
fn go_module(dir: &std::path::Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    on_thread(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        drop(held);
    })
}

#[test]
fn search_run_returns_the_table_converted_to_json() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"s\", { timeout = 1000, run = function(arg) return { { title = \"t\", url = \"u\", snippet = \"s\" } } end })\n",
    );
    let returned = ext
        .search_run("s", json!({"query": "rust"}), &fakes::CancelToken::new())
        .unwrap();
    assert_eq!(
        returned,
        Some(json!([{"title": "t", "url": "u", "snippet": "s"}]))
    );
}

#[test]
fn a_spinning_run_past_its_timeout_returns_a_timeout() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-lua-search");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.search_backend(\"spin\", { timeout = 100, run = function() require(\"go_spin\") while true do end end })\n",
    )
    .unwrap();
    let went = go_module(dir.path(), "spin");
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/t",
        dir.path(),
        "/nonexistent-fiber-home",
        clock.clone(),
    ));
    let spinning = Arc::clone(&ext);
    let spun =
        on_thread(move || spinning.search_run("spin", json!({}), &fakes::CancelToken::new()));
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the backend to spin");
    clock.advance(Duration::from_millis(100));
    let Err(Error::Timeout { callback, .. }) =
        Deadline::after(WAIT).recv(&spun).expect("the run returned")
    else {
        panic!("the spinning run did not time out");
    };
    assert_eq!(callback, "spin");
}

#[test]
fn a_cancel_before_start_ends_the_call_and_keeps_the_extension() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"s\", { timeout = 5000, run = function() return { { title = \"t\", url = \"u\", snippet = \"s\" } } end })\n",
    );
    let cancel = fakes::CancelToken::new();
    cancel.cancel();
    let cancelled = ext.search_run("s", json!({}), &cancel).unwrap();
    assert_eq!(cancelled, None, "the call ends cancelled");
    let again = ext
        .search_run("s", json!({}), &fakes::CancelToken::new())
        .unwrap();
    assert_eq!(
        again,
        Some(json!([{"title": "t", "url": "u", "snippet": "s"}])),
        "the extension stayed ready"
    );
}

#[test]
fn an_unregistered_backend_gives_unknown_callback() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"s\", { timeout = 100, run = function() return {} end })\n",
    );
    let Err(Error::UnknownCallback { callback, .. }) =
        ext.search_run("missing", json!({}), &fakes::CancelToken::new())
    else {
        panic!("an unregistered backend did not fail");
    };
    assert_eq!(callback, "missing");
}

#[test]
fn a_refresh_from_a_backend_has_no_credential_to_refresh() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"s\", { timeout = 5000, run = function() return host.oauth.refresh(function() end) end })\n",
    );
    let Err(Error::Lua { message, .. }) =
        ext.search_run("s", json!({}), &fakes::CancelToken::new())
    else {
        panic!("a refresh from a backend did not fail");
    };
    assert!(
        message.contains("search backend"),
        "the error names the search backend: {message}"
    );
}

#[test]
fn a_cancel_after_the_call_finished_keeps_its_result() {
    // A finished call keeps how it ended when the cancel lands, exactly as
    // a tool call does: `cancel_call` leaves a finished call as it ended,
    // so the waiter judges its value, not a cancel.
    let mut shared = super::super::Shared::default();
    let id = 7;
    shared.calls.insert(
        id,
        Progress::Done(Ok(json!([{"title": "t", "url": "u", "snippet": "s"}]))),
    );
    shared.cancel_call(id);
    assert!(
        matches!(
            shared.calls.get(&id),
            Some(Progress::Done(Ok(value))) if *value == json!([{"title": "t", "url": "u", "snippet": "s"}])
        ),
        "a finished search keeps its value past a cancel, as a tool call does"
    );
}

/// One accepted return through the full path, as JSON.
fn accepted(run: &str) -> Value {
    let (_dir, ext) = extension(&format!(
        "fiber.search_backend(\"s\", {{ timeout = 1000, run = {run} }})\n",
    ));
    ext.search_run("s", json!({}), &fakes::CancelToken::new())
        .expect("the run returned")
        .expect("the run was not cancelled")
}

#[test]
fn accepted_returns_read_as_lists() {
    assert_eq!(accepted("function() return {} end"), json!([]));
    assert_eq!(
        accepted("function() return { { title = \"a\", url = \"u\", snippet = \"s\" } } end"),
        json!([{"title": "a", "url": "u", "snippet": "s"}])
    );
    assert_eq!(
        accepted(
            "function() return { { title = \"a\", url = \"1\", snippet = \"x\" }, \
              { title = \"b\", url = \"2\", snippet = \"y\" }, \
              { title = \"c\", url = \"3\", snippet = \"z\" } } end"
        ),
        json!([
            {"title": "a", "url": "1", "snippet": "x"},
            {"title": "b", "url": "2", "snippet": "y"},
            {"title": "c", "url": "3", "snippet": "z"},
        ])
    );
}

/// One refused return through the full path: the backend, its extension
/// and the position.
fn refused(run: &str) -> String {
    let (_dir, ext) = extension(&format!(
        "fiber.search_backend(\"brave\", {{ timeout = 1000, run = {run} }})\n",
    ));
    let Err(Error::BadReturn {
        extension,
        callback,
        why,
    }) = ext.search_run("brave", json!({}), &fakes::CancelToken::new())
    else {
        panic!("a malformed return did not fail: {run}");
    };
    assert_eq!(extension, "fiber.test/t");
    assert_eq!(callback, "brave");
    why
}

#[test]
fn refused_returns_name_the_backend_and_the_position() {
    assert!(
        refused("function() return \"nope\" end").contains("list"),
        "a string is not a list"
    );
    assert!(
        refused("function() return { title = \"t\", url = \"u\", snippet = \"s\" } end")
            .contains("list"),
        "a non-list table is refused"
    );
    let sparse = refused(
        "function() local e = { title = \"t\", url = \"u\", snippet = \"s\" } \
         return { [1] = e, [3] = e } end",
    );
    assert!(
        sparse.contains("list"),
        "a sparse list is refused: {sparse}"
    );
    let second = refused(
        "function() return { { title = \"a\", url = \"u\", snippet = \"s\" }, \
         \"nope\", { title = \"b\", url = \"u\", snippet = \"s\" } } end",
    );
    assert!(
        second.contains("result 2") && second.contains("not a table"),
        "entry 2 names its position: {second}"
    );
    for field in ["title", "url", "snippet"] {
        let missing = refused(&format!(
            "function() local e = {{ title = \"t\", url = \"u\", snippet = \"s\" }} \
             e.{field} = nil \
             return {{ e }} end",
        ));
        assert!(
            missing.contains("result 1") && missing.contains(field),
            "a missing `{field}` names it: {missing}"
        );
        let number = refused(&format!(
            "function() return {{ {{ title = \"t\", url = \"u\", snippet = \"s\", {field} = 7 }} }} end",
        ));
        assert!(
            number.contains("result 1") && number.contains(field),
            "a number for `{field}` names it: {number}"
        );
    }
    let function = refused(
        "function() return { { title = function() end, url = \"u\", snippet = \"s\" } } end",
    );
    assert!(
        function.contains("result 1") && function.contains("title"),
        "a function field is checked on the Lua value, not lost in conversion: {function}"
    );
    let rank = refused(
        "function() return { { title = \"t\", url = \"u\", snippet = \"s\", rank = 1 } } end",
    );
    assert!(
        rank.contains("result 1") && rank.contains("rank"),
        "an extra key is refused: {rank}"
    );
}
