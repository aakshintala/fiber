//! Choosing the search backend (`docs/tools.md`, "Fiber's own, over a
//! backend") and running it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::CommandId;
use contract::ErrorCode;
use contract::clock::Clock as _;
use contract::search::{Domains, SearchBackend};
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::*;
use crate::lua::LuaExtension;

/// How long past a deadline the caller waits before it abandons the VM.
const GRACE: Duration = Duration::from_secs(1);

/// An extension named `fiber.test/t` whose entry script is `init`, in a
/// fresh temporary directory kept beside it.
fn extension(init: &str) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("fiber-search");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let ext = LuaExtension::new(
        "fiber.test/t",
        dir.path(),
        "/nonexistent-fiber-home",
        FakeClock::new(),
    );
    (dir, Arc::new(ext))
}

/// Runs `call` on its own thread and hands back its result.
fn on_thread<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || match tx.send(call()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    rx
}

/// One `select` row: the `(extension, backend)` pairs in load order, the
/// setting, the chosen pair's index and the notices as
/// `(code, message)`.
fn row(
    backends: &[(&str, &str)],
    setting: Option<&str>,
    chosen: Option<usize>,
    notices: &[(&str, &str)],
) {
    let owned: Vec<(String, String)> = backends
        .iter()
        .map(|(extension, name)| ((*extension).to_owned(), (*name).to_owned()))
        .collect();
    let (index, raised) = select(&owned, setting);
    assert_eq!(index, chosen, "backends {backends:?} setting {setting:?}");
    let raised: Vec<(String, String)> = raised
        .iter()
        .map(|notice| (code_name(&notice.code), notice.message.clone()))
        .collect();
    let wanted: Vec<(String, String)> = notices
        .iter()
        .map(|(code, message)| ((*code).to_owned(), (*message).to_owned()))
        .collect();
    assert_eq!(raised, wanted, "backends {backends:?} setting {setting:?}");
}

fn code_name(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .unwrap()
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn choose_table() {
    row(&[], None, None, &[]);
    row(
        &[],
        Some("x"),
        None,
        &[(
            "web_search_unavailable",
            "`web_search.backend` names `x`, which is not installed. Installed: none.",
        )],
    );
    row(&[("a", "brave")], None, Some(0), &[]);
    row(&[("a", "brave")], Some("brave"), Some(0), &[]);
    row(
        &[("a", "brave")],
        Some("kagi"),
        None,
        &[(
            "web_search_unavailable",
            "`web_search.backend` names `kagi`, which is not installed. Installed: `brave`.",
        )],
    );
    row(
        &[("a", "brave"), ("b", "kagi")],
        None,
        None,
        &[(
            "web_search_unavailable",
            "Several search backends are installed (`brave`, `kagi`): set `web_search.backend` to the one `web_search` uses.",
        )],
    );
    row(&[("a", "brave"), ("b", "kagi")], Some("kagi"), Some(1), &[]);
    row(
        &[("a", "brave"), ("b", "kagi")],
        Some("missing"),
        None,
        &[(
            "web_search_unavailable",
            "`web_search.backend` names `missing`, which is not installed. Installed: `brave`, `kagi`.",
        )],
    );
    row(
        &[("a", "brave"), ("b", "brave")],
        None,
        None,
        &[(
            "extension_failed",
            "`brave` search backend not registered: `a` and `b` both register it.",
        )],
    );
    row(
        &[("b", "brave"), ("a", "brave")],
        None,
        None,
        &[(
            "extension_failed",
            "`brave` search backend not registered: `a` and `b` both register it.",
        )],
    );
    row(
        &[("b", "kagi"), ("a", "kagi"), ("d", "brave"), ("c", "brave")],
        None,
        None,
        &[
            (
                "extension_failed",
                "`brave` search backend not registered: `c` and `d` both register it.",
            ),
            (
                "extension_failed",
                "`kagi` search backend not registered: `a` and `b` both register it.",
            ),
        ],
    );
    row(
        &[("a", "brave"), ("b", "brave"), ("c", "c")],
        None,
        Some(2),
        &[(
            "extension_failed",
            "`brave` search backend not registered: `a` and `b` both register it.",
        )],
    );
    row(
        &[("a", "brave"), ("b", "brave")],
        Some("brave"),
        None,
        &[
            (
                "extension_failed",
                "`brave` search backend not registered: `a` and `b` both register it.",
            ),
            (
                "web_search_unavailable",
                "`web_search.backend` names `brave`, which is not installed. Installed: none.",
            ),
        ],
    );
}

/// `choose` wraps the chosen handle in a backend over it.
#[test]
fn choose_returns_the_chosen_backend() {
    let (_dir, ext) = extension("return nil\n");
    let backends = vec![
        SearchSource {
            extension: "fiber.test/a".to_owned(),
            name: "brave".to_owned(),
            backend: SearchBackendKind::Lua(Arc::clone(&ext)),
        },
        SearchSource {
            extension: "fiber.test/b".to_owned(),
            name: "kagi".to_owned(),
            backend: SearchBackendKind::Lua(Arc::clone(&ext)),
        },
    ];
    let (chosen, notices) = choose(backends, Some("kagi"));
    assert!(chosen.is_some());
    assert!(notices.is_empty());
    let backends = vec![
        SearchSource {
            extension: "fiber.test/a".to_owned(),
            name: "brave".to_owned(),
            backend: SearchBackendKind::Lua(Arc::clone(&ext)),
        },
        SearchSource {
            extension: "fiber.test/b".to_owned(),
            name: "kagi".to_owned(),
            backend: SearchBackendKind::Lua(Arc::clone(&ext)),
        },
    ];
    let (chosen, notices) = choose(backends, None);
    assert!(chosen.is_none());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::WebSearchUnavailable);
}

/// A backend echoing its argument into the snippet: what `search` sends.
fn echo() -> (fakes::TempDir, Arc<LuaExtension>) {
    extension(
        "fiber.search_backend(\"s\", { timeout = 5000, run = function(arg) \
         return { { title = \"t\", url = \"u\", snippet = json.encode(arg) } } end })\n",
    )
}

fn search(
    ext: &Arc<LuaExtension>,
    domains: &Domains,
) -> Result<Option<Vec<SearchResult>>, Failure> {
    LuaSearch::new(Arc::clone(ext), "s".to_owned()).search(
        "rust",
        domains,
        &fakes::CancelToken::new(),
    )
}

#[test]
fn the_arguments_arrive_as_query_and_the_domain_filter() {
    let (_dir, ext) = echo();
    let any = search(&ext, &Domains::Any).unwrap().unwrap();
    assert_eq!(any[0].snippet, "{\"query\":\"rust\"}");
    let allowed = search(&ext, &Domains::Allowed(vec!["rust-lang.org".to_owned()]))
        .unwrap()
        .unwrap();
    assert_eq!(
        allowed[0].snippet,
        "{\"allowed_domains\":[\"rust-lang.org\"],\"query\":\"rust\"}"
    );
    let blocked = search(&ext, &Domains::Blocked(vec!["example.com".to_owned()]))
        .unwrap()
        .unwrap();
    assert_eq!(
        blocked[0].snippet,
        "{\"blocked_domains\":[\"example.com\"],\"query\":\"rust\"}"
    );
}

#[test]
fn process_search_receives_the_query_and_returns_its_results() {
    let runtime = crate::runtime::Runtime::new(FakeClock::new());
    runtime.register_owner("fiber.test/proc", None);
    let received = Arc::new(Mutex::new(None));
    let run = {
        let received = Arc::clone(&received);
        runtime.install_process_handler(
            "fiber.test/proc",
            CommandId("register-search".to_owned()),
            "run",
            Arc::new(move |args, _cancel| {
                *received.lock().unwrap() = Some(args);
                Ok(Some(serde_json::json!([
                    {"title": "Rust", "url": "https://rust-lang.org", "snippet": "Language"}
                ])))
            }),
        )
    };
    let backend = ProcessSearch::new(
        runtime,
        "fiber.test/proc".to_owned(),
        "search".to_owned(),
        run,
    );
    let results = backend
        .search(
            "rust ownership",
            &Domains::Allowed(vec!["rust-lang.org".to_owned()]),
            &fakes::CancelToken::new(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        *received.lock().unwrap(),
        Some(serde_json::json!({
            "query": "rust ownership",
            "allowed_domains": ["rust-lang.org"],
        }))
    );
    assert!(
        results.iter().any(|result| result.title == "Rust"
            && result.url == "https://rust-lang.org"
            && result.snippet == "Language"),
        "{results:?}"
    );
}

#[test]
fn a_malformed_return_is_a_tool_error_naming_the_backend() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"brave\", { timeout = 1000, run = function() \
         return { { title = \"a\", url = \"u\", snippet = \"s\" }, \
         { title = \"b\", snippet = \"s\" } } end })\n",
    );
    let Err(failed) = LuaSearch::new(ext, "brave".to_owned()).search(
        "rust",
        &Domains::Any,
        &fakes::CancelToken::new(),
    ) else {
        panic!("a malformed return did not fail");
    };
    assert_eq!(failed.code, ErrorCode::ToolError);
    assert!(
        failed.message.contains("brave")
            && failed.message.contains("fiber.test/t")
            && failed.message.contains("result 2"),
        "the message names the backend, its extension and the position: {}",
        failed.message
    );
}

/// `require("go_<name>")` in `dir` signals that the callback has started:
/// the loader runs the registered observer before it opens the empty file.
fn go_module(dir: &std::path::Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    std::fs::File::create(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    crate::lua::observe_load(
        path.canonicalize().unwrap(),
        Box::new(move || match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }),
    );
    rx
}

#[test]
fn a_timeout_maps_to_timeout() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-search");
    std::fs::write(
        dir.path().join("init.lua"),
        "fiber.search_backend(\"s\", { timeout = 50, run = function() require(\"go_spin\") while true do end end })\n",
    )
    .unwrap();
    let went = go_module(dir.path(), "spin");
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/t",
        dir.path(),
        "/nonexistent-fiber-home",
        clock.clone(),
    ));
    let searching = Arc::clone(&ext);
    let asked = clock.now();
    let ran = on_thread(move || {
        LuaSearch::new(searching, "s".to_owned()).search(
            "rust",
            &Domains::Any,
            &fakes::CancelToken::new(),
        )
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for the backend to start");
    assert!(
        clock.await_parked(
            asked + Duration::from_millis(50) + GRACE,
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the caller parked at the grace"
    );
    clock.advance_when_parked(
        Some(asked + Duration::from_millis(50) + GRACE),
        None,
        Duration::from_millis(50),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let Err(failed) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&ran)
        .expect("the search returned")
    else {
        panic!("a spinning backend did not fail");
    };
    assert_eq!(failed.code, ErrorCode::Timeout);
}

#[test]
fn a_raised_error_maps_to_tool_error_with_its_message() {
    let (_dir, ext) = extension(
        "fiber.search_backend(\"s\", { timeout = 1000, run = function() error(\"boom\", 0) end })\n",
    );
    let Err(failed) = LuaSearch::new(ext, "s".to_owned()).search(
        "rust",
        &Domains::Any,
        &fakes::CancelToken::new(),
    ) else {
        panic!("a raised error did not fail");
    };
    assert_eq!(failed.code, ErrorCode::ToolError);
    assert!(
        failed.message.contains("boom"),
        "the message keeps the error: {}",
        failed.message
    );
}

#[test]
fn a_cancel_maps_to_no_result() {
    let (_dir, ext) = echo();
    let cancel = fakes::CancelToken::new();
    cancel.cancel();
    let cancelled =
        LuaSearch::new(Arc::clone(&ext), "s".to_owned()).search("rust", &Domains::Any, &cancel);
    assert!(
        matches!(cancelled, Ok(None)),
        "a cancelled search returns no result"
    );
}
