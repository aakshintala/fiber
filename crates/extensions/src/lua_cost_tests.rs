#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::Secret;
use contract::GenerationId;
use contract::clock::Clock;
use contract::events::CacheLifetime;
use contract::provider::ToolDefinition;
use contract::provider::{
    CallError, CallUsage, Delta, InputSize, ModelCall, ModelRequest, Provider, Reply,
};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};

use crate::{Error, LuaExtension, LuaProvider};

/// How long a test waits for one call.
const WAIT: Duration = Duration::from_secs(5);

/// The hub's wait past a callback's own timeout before it gives up on the
/// callback's thread.
const GRACE: Duration = Duration::from_secs(1);

#[track_caller]
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    match Deadline::after(WAIT).recv(&rx) {
        Ok(answer) => answer,
        Err(_) => panic!("the call did not return within {WAIT:?}"),
    }
}

/// A test-local provider `p` whose package's `init.lua` is `script`, in a
/// fresh temporary directory, on `clock`.
fn provider_on(
    root: &fakes::TempDir,
    script: &str,
    clock: Arc<FakeClock>,
) -> (Arc<LuaProvider>, std::path::PathBuf) {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("init.lua"), script).unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir.clone(), home, clock));
    (LuaProvider::new(extension, "p"), dir)
}

/// A provider `p` whose `cost.run` body is `body`, with a 1 s timeout.
fn cost_provider(root: &fakes::TempDir, body: &str) -> Arc<LuaProvider> {
    let script = format!(
        "fiber.provider(\"p\", {{ cost = {{ timeout = 1000, run = function(a) {body} end }} }})\n"
    );
    provider_on(root, &script, FakeClock::new()).0
}

#[track_caller]
fn cost(provider: &Arc<LuaProvider>, key: Option<&str>) -> Result<Option<f64>, Error> {
    let provider = Arc::clone(provider);
    let key = key.map(|k| Secret::new(k.to_owned()));
    within(move || {
        provider.cost(
            &GenerationId("gen-1".into()),
            "http://127.0.0.1:1/api/v1",
            key.as_ref(),
        )
    })
}

#[test]
fn nothing_and_finite_numbers_at_or_above_zero_are_costs_and_the_rest_are_errors() {
    for (returned, want) in [
        ("nil", Some(None)),
        ("0", Some(Some(0.0))),
        ("0.0000072", Some(Some(0.0000072))),
        ("2", Some(Some(2.0))),
        ("-1", None),
        ("-0.000001", None),
        ("math.huge", None),
        ("0/0", None),
        ("\"0.1\"", None),
        ("{}", None),
    ] {
        let root = fakes::TempDir::new("fiber-lua-cost");
        let provider = cost_provider(&root, &format!("return {returned}"));
        let got = cost(&provider, None);
        match want {
            Some(want) => assert_eq!(got.unwrap(), want, "{returned}"),
            None => assert!(got.is_err(), "{returned}: {got:?}"),
        }
    }
}

#[test]
fn a_negative_number_is_a_bad_return_naming_the_callback() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let provider = cost_provider(&root, "return -1");
    let err = cost(&provider, None).unwrap_err();
    let Error::BadReturn { callback, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(callback, "p.cost");
}

#[test]
fn a_raised_error_is_an_error() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let provider = cost_provider(&root, "error(\"lookup broke\")");
    let err = cost(&provider, None).unwrap_err();
    assert!(matches!(err, Error::Lua { .. }), "{err:?}");
}

/// A fifo at `<dir>/go_<name>.lua`: a `require` of it blocks until the
/// returned receiver's sender has opened it, which signals once the
/// callback reached the `require`.
fn go_module(dir: &Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        drop(held);
    });
    rx
}

#[test]
fn a_run_that_spins_past_its_timeout_is_a_timeout() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let clock = FakeClock::new();
    let (provider, dir) = provider_on(
        &root,
        "fiber.provider(\"p\", { cost = { timeout = 50, run = function()\n\
         require(\"go_spin\")\n\
         while true do end end } })\n",
        clock.clone(),
    );
    let went = go_module(&dir, "spin");
    let asked = clock.now();
    let (tx, result) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(provider.cost(&GenerationId("gen-1".into()), "http://h/v1", None))
    });
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the callback to pass its clock check");
    assert!(
        clock.await_parked(asked + Duration::from_millis(50) + GRACE, WAIT),
        "waited for the caller to park at the grace"
    );
    clock.advance(Duration::from_millis(50));
    let err = Deadline::after(WAIT)
        .recv(&result)
        .expect("waited for the spinning callback")
        .unwrap_err();
    let Error::Timeout {
        callback,
        timeout_ms,
        ..
    } = &err
    else {
        panic!("{err:?}")
    };
    assert_eq!((callback.as_str(), *timeout_ms), ("p.cost", 50));
}

#[test]
fn run_receives_the_generation_its_base_url_and_its_key() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let provider = cost_provider(
        &root,
        "if a.generation_id == \"gen-1\" and a.base_url == \"http://127.0.0.1:1/api/v1\" \
         and a.key == \"sk-1\" then return 0.25 end error(\"unexpected argument\")",
    );
    assert_eq!(cost(&provider, Some("sk-1")).unwrap(), Some(0.25));
}

#[test]
fn without_a_key_run_receives_no_key() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let provider = cost_provider(
        &root,
        "if a.generation_id == \"gen-1\" and a.key == nil then return 0.5 end \
         error(\"unexpected argument\")",
    );
    assert_eq!(cost(&provider, None).unwrap(), Some(0.5));
}

/// A provider whose tools are its own shape, which counts its calls.
#[derive(Default)]
struct Inner {
    calls: AtomicUsize,
    warms: bool,
}

struct Idle;

impl ModelCall for Idle {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        Err(CallError::Cancelled {
            usage: Box::new(CallUsage::unnamed(InputSize::default())),
        })
    }

    fn cancel(&self) {}
}

impl Provider for Inner {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::new(Idle)
    }

    fn warms(&self, _request: &ModelRequest) -> bool {
        self.warms
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        tools
            .iter()
            .map(|tool| {
                let mut map = Map::new();
                map.insert("own".into(), json!(tool.name));
                map
            })
            .collect()
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: Vec::new(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: String::new(),
        input_schema: json!({"type": "object"}),
        deferred: false,
        hosted: None,
    }
}

#[test]
fn costed_without_cost_is_the_inner_provider_itself() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let (lua, _) = provider_on(
        &root,
        "fiber.provider(\"p\", { sign = { timeout = 1000, run = function() return {} end } })\n",
        FakeClock::new(),
    );
    let inner: Arc<dyn Provider> = Arc::new(Inner::default());
    let wrapped = {
        let inner = Arc::clone(&inner);
        within(move || lua.costed(inner, "http://h/v1", None)).unwrap()
    };
    assert!(std::ptr::addr_eq(
        Arc::as_ptr(&wrapped),
        Arc::as_ptr(&inner)
    ));
    assert!(wrapped.cost_lookup().is_none());
}

#[test]
fn costed_with_cost_delegates_calls_and_tools_and_looks_up_through_cost() {
    let root = fakes::TempDir::new("fiber-lua-cost");
    let (lua, _) = provider_on(
        &root,
        "fiber.provider(\"p\", { cost = { timeout = 1000, run = function(a) \
         if a.generation_id == \"gen-ok\" and a.key == \"sk-1\" then return 0.125 end \
         error(\"no such generation\") end } })\n",
        FakeClock::new(),
    );
    let inner = Arc::new(Inner::default());
    let wrapped = {
        let inner = Arc::clone(&inner) as Arc<dyn Provider>;
        within(move || lua.costed(inner, "http://h/v1", Some(Secret::new("sk-1".into())))).unwrap()
    };
    let tools = [tool("b"), tool("a")];
    assert_eq!(wrapped.wire_tools(&tools), inner.wire_tools(&tools));
    let _call = wrapped.call(&request());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);

    let lookup = wrapped.cost_lookup().unwrap();
    let found = {
        let lookup = Arc::clone(&lookup);
        within(move || lookup.cost(&GenerationId("gen-ok".into())))
    };
    assert_eq!(found, Some(0.125));
    // A raised error is nothing to the loop.
    let missing = within(move || lookup.cost(&GenerationId("gen-gone".into())));
    assert_eq!(missing, None);
}

#[test]
fn costed_forwards_whether_the_inner_provider_warms() {
    for warms in [false, true] {
        let root = fakes::TempDir::new("fiber-lua-cost");
        let (lua, _) = provider_on(
            &root,
            "fiber.provider(\"p\", { cost = { timeout = 1000, run = function() return 0 end } })\n",
            FakeClock::new(),
        );
        let inner: Arc<dyn Provider> = Arc::new(Inner {
            warms,
            ..Inner::default()
        });
        let wrapped = within(move || lua.costed(inner, "http://h/v1", None)).unwrap();
        assert!(wrapped.cost_lookup().is_some());
        assert_eq!(wrapped.warms(&request()), warms);
    }
}
