//! A session's extensions: what loads, what registers, and how the
//! `after_tool` chain runs (`docs/extensions.md`, "Hooks").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::events::CallStatus;
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use contract::shapes::Process;
use fakes::clock::FakeClock;
use serde_json::{Map, json};

use super::SessionExtensions;
use crate::lua::HookPhase;
use crate::{Origin, Request, plan};

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-session-extensions");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`, whose entry script is `init`, or a
    /// data-only extension when `init` is `None`.
    fn install(&self, short: &str, init: Option<&str>) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("extension.json"),
            json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1})
                .to_string(),
        )
        .unwrap();
        if let Some(init) = init {
            fs::write(src.join("init.lua"), init).unwrap();
        }
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// The session's extensions, loaded on a thread under [`WAIT`]: the
    /// fake clock never ends a wait the runtime does not end itself.
    /// Rewrites the installed `fiber.test/<short>`'s manifest with `change`,
    /// as an upgrade of Fiber or a hand edit would leave it.
    fn edit_manifest(&self, short: &str, change: impl FnOnce(&mut serde_json::Value)) {
        let path = self
            .home()
            .join("extensions")
            .join(format!("fiber.test-{short}"))
            .join("extension.json");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        change(&mut manifest);
        fs::write(&path, manifest.to_string()).unwrap();
    }

    fn load(&self, overrides: &[&str]) -> Arc<SessionExtensions> {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap();
        let home = self.home();
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new())
        }))
    }
}

/// An `after_tool` hook that appends `|<tag>` to the content.
fn tagging(tag: &str, phase: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ phase = \"{phase}\", on_failure = \"non-blocking\", timeout = 1000,\n\
           run = function(call) return {{ content = call.content .. \"|{tag}\" }} end }})\n"
    )
}

/// An `after_tool` hook whose `run` is `body`.
fn hook(on_failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// How long a test waits for a load or a chain before failing.
const WAIT: Duration = Duration::from_secs(5);

/// Runs `f` on its own thread and waits for it under [`WAIT`], so a runtime
/// that never answers fails the test instead of hanging it.
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    rx.recv_timeout(WAIT).expect("waited for the extensions")
}

fn after_tool(session: &Arc<SessionExtensions>, content: &str) -> AfterToolAnswer {
    let (session, content) = (Arc::clone(session), content.to_owned());
    bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: &content,
            details: None,
            process: None,
        })
    })
}

fn changed_content(answer: &AfterToolAnswer) -> Option<&str> {
    match &answer.outcome {
        AfterToolOutcome::Changed { content, .. } => content.as_deref(),
        AfterToolOutcome::Unchanged | AfterToolOutcome::Withheld { .. } => None,
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| format!("fiber.test/{n}")).collect()
}

fn opening(machine: &[&str], project: &[&str], budget_bytes: Option<u64>) -> serde_json::Value {
    let strings = |names: &[&str]| {
        names
            .iter()
            .map(|name| serde_json::Value::String((*name).into()))
            .collect::<Vec<_>>()
    };
    let mut opening = serde_json::Map::new();
    opening.insert("machine".into(), strings(machine).into());
    opening.insert("project".into(), strings(project).into());
    if let Some(budget) = budget_bytes {
        opening.insert("budget_bytes".into(), budget.into());
    }
    serde_json::Value::Object(opening)
}

#[test]
fn sections_list_machine_then_project_paths_in_extension_name_order() {
    let home = Home::new();
    home.install("zeta", None);
    home.install("alpha", None);
    home.edit_manifest("zeta", |m| {
        m["opening"] = opening(&["z.md"], &["p.md"], Some(10));
    });
    home.edit_manifest("alpha", |m| {
        m["opening"] = opening(&["a.md", "b/c.md"], &[], None);
    });
    let session = home.load(&[]);
    let project = ProjectKey::new("p").unwrap();
    let sections = session.sections(&project);
    let root = home.home();
    assert_eq!(
        sections,
        [
            (
                "fiber.test/alpha".to_owned(),
                vec![
                    root.join("data/fiber.test-alpha/a.md"),
                    root.join("data/fiber.test-alpha/b/c.md"),
                ],
                None,
            ),
            (
                "fiber.test/zeta".to_owned(),
                vec![
                    root.join("data/fiber.test-zeta/z.md"),
                    root.join("projects/p/data/fiber.test-zeta/p.md"),
                ],
                Some(10),
            ),
        ]
    );
}

#[test]
fn sections_omit_a_disabled_extension_one_without_opening_and_an_empty_opening() {
    let home = Home::new();
    home.install("off", None);
    home.install("plain", None);
    home.install("empty", None);
    home.edit_manifest("off", |m| {
        m["opening"] = opening(&["o.md"], &[], None);
    });
    home.edit_manifest("empty", |m| {
        m["opening"] = opening(&[], &[], None);
    });
    let session = home.load(&["extensions.\"fiber.test/off\".enabled=false"]);
    assert!(
        session
            .loaded()
            .iter()
            .any(|e| e.name == "fiber.test/plain")
    );
    let project = ProjectKey::new("p").unwrap();
    assert!(session.sections(&project).is_empty());
}

#[test]
fn an_empty_home_loads_nothing_and_starts_no_vm() {
    let home = Home::new();
    let session = home.load(&[]);
    assert!(session.loaded().is_empty());
    assert!(session.notices().is_empty());
    assert!(!session.has_hooks());
    assert!(session.lua.is_empty());
    let answer = after_tool(&session, "x");
    assert_eq!(answer.outcome, AfterToolOutcome::Unchanged);
    assert!(answer.changed_by.is_empty());
}

#[test]
fn every_enabled_extension_is_loaded_with_its_version_and_a_disabled_one_is_not() {
    let home = Home::new();
    home.install("data", None);
    home.install("quiet", Some("-- registers nothing\n"));
    home.install("off", Some(&tagging("off", "transform")));
    let session = home.load(&["extensions.\"fiber.test/off\".enabled=false"]);
    let loaded: Vec<(String, String)> = session
        .loaded()
        .into_iter()
        .map(|e| (e.name, e.version))
        .collect();
    assert_eq!(
        loaded,
        [
            ("fiber.test/data".to_owned(), "v1.2.3".to_owned()),
            ("fiber.test/quiet".to_owned(), "v1.2.3".to_owned()),
        ]
    );
    assert!(session.notices().is_empty());
    assert!(!session.has_hooks());
    // The data-only extension has no VM; the Lua one without hooks does.
    assert_eq!(session.lua.len(), 1);
    assert!(session.lua[0].is_running());
    assert_eq!(
        after_tool(&session, "x").outcome,
        AfterToolOutcome::Unchanged
    );
}

#[test]
fn a_loaded_extensions_directory_is_listed_and_a_disabled_ones_is_not() {
    let home = Home::new();
    home.install("data", None);
    home.install("off", Some(&tagging("off", "transform")));
    let session = home.load(&["extensions.\"fiber.test/off\".enabled=false"]);
    assert_eq!(
        session.dirs(),
        [(
            "fiber.test/data".to_owned(),
            home.home().join("extensions").join("fiber.test-data")
        )]
    );
}

#[test]
fn an_entry_script_that_fails_leaves_a_notice_and_is_not_loaded() {
    let home = Home::new();
    home.install("broken", Some("error(\"bad start\")\n"));
    home.install("fine", Some(&tagging("fine", "transform")));
    let session = home.load(&[]);
    let loaded: Vec<String> = session.loaded().into_iter().map(|e| e.name).collect();
    assert_eq!(loaded, names(&["fine"]));
    let notices = session.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/broken"));
    assert!(
        notices[0].message.contains("bad start"),
        "{}",
        notices[0].message
    );
    assert_eq!(changed_content(&after_tool(&session, "x")), Some("x|fine"));
}

#[test]
fn a_hook_that_leaves_out_a_required_field_is_not_registered_and_says_why() {
    let home = Home::new();
    home.install(
        "picky",
        Some(
            r#"
local run = function(call) return { content = "ran" } end
fiber.hook("after_tool", { on_failure = "blocking", run = run })
fiber.hook("after_tool", { timeout = 100, run = run })
fiber.hook("after_tool", { timeout = 1.5, on_failure = "blocking", run = run })
fiber.hook("after_tool", { timeout = 0, on_failure = "blocking", run = run })
fiber.hook("after_tool", { timeout = 100, on_failure = 1, run = run })
fiber.hook("after_tool", { timeout = 100, on_failure = "maybe", run = run })
fiber.hook("after_tool", { phase = "late", timeout = 100, on_failure = "blocking", run = run })
fiber.hook("after_tool", { phase = "check", timeout = 100, on_failure = "blocking", run = run })
fiber.hook("after_tool", { timeout = 100, on_failure = "blocking", run = "no" })
fiber.hook("after_tool", "no")
fiber.hook("after_call", { timeout = 100, on_failure = "blocking", run = run })
fiber.hook(7, { timeout = 100, on_failure = "blocking", run = run })
fiber.hook("before_tool", { phase = "check", timeout = 1, on_failure = "blocking", run = run })
"#,
        ),
    );
    let session = home.load(&[]);
    let notices = session.notices();
    let messages: Vec<&str> = notices.iter().map(|n| n.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "`after_tool` hook not registered: missing `timeout`",
            "`after_tool` hook not registered: missing `on_failure`",
            "`after_tool` hook not registered: `timeout` must be a whole number of milliseconds above 0",
            "`after_tool` hook not registered: `timeout` must be a whole number of milliseconds above 0",
            "`after_tool` hook not registered: `on_failure` must be `blocking` or `non-blocking`",
            "`after_tool` hook not registered: `on_failure` must be `blocking` or `non-blocking`",
            "`after_tool` hook not registered: `phase` must be `sanitize`, `transform` or `check`",
            "`after_tool` hook not registered: a `check` hook exists only at `before_message`, `before_tool` and `before_model_call`",
            "`after_tool` hook not registered: `run` must be a function",
            "`after_tool` hook not registered: it takes a table of `timeout`, `on_failure` and `run`",
            "`after_call` hook not registered: `after_call` is not a hook point",
            "`7` hook not registered: `7` is not a hook point",
        ]
    );
    for notice in &notices {
        assert_eq!(notice.code, ErrorCode::ExtensionFailed);
        assert_eq!(notice.extension.as_deref(), Some("fiber.test/picky"));
    }
    // The extension still loads; its one hook at another point counts, and
    // no `after_tool` hook runs.
    assert_eq!(session.loaded().len(), 1);
    assert!(session.has_hooks());
    assert!(session.after_tool.is_empty());
    assert_eq!(
        after_tool(&session, "x").outcome,
        AfterToolOutcome::Unchanged
    );
}

#[test]
fn a_timeout_of_one_and_a_named_phase_register() {
    let home = Home::new();
    home.install(
        "edge",
        Some(
            "fiber.hook(\"after_tool\", { phase = \"sanitize\", timeout = 1, on_failure = \"non-blocking\",\n\
               run = function(call) return { content = \"ok\" } end })\n",
        ),
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    assert_eq!(session.after_tool.len(), 1);
    assert!(session.has_hooks());
}

#[test]
fn the_hook_is_shown_the_call_and_its_output() {
    let home = Home::new();
    home.install(
        "look",
        Some(&hook(
            "blocking",
            "return { content = table.concat({ call.tool, call.status, call.arguments.path, \
             call.content, tostring(call.details.n), tostring(call.process.exit_code), \
             tostring(call.process.timed_out) }, \" \") }",
        )),
    );
    let session = home.load(&[]);
    let answer = bounded(move || {
        let mut arguments = Map::new();
        arguments.insert("path".into(), json!("a.txt"));
        let details = json!({"n": 3});
        let process = Process {
            exit_code: Some(2),
            signal: None,
            timed_out: false,
        };
        session.after_tool(&AfterToolCall {
            tool: "shell",
            arguments: &arguments,
            status: CallStatus::Cancelled,
            content: "out",
            details: Some(&details),
            process: Some(&process),
        })
    });
    assert_eq!(
        changed_content(&answer),
        Some("shell cancelled a.txt out 3 2 false")
    );
}

#[test]
fn hooks_run_by_phase_then_by_name_and_each_sees_the_one_before() {
    let home = Home::new();
    home.install(
        "a",
        Some(&(tagging("a1", "transform") + &tagging("a2", "transform"))),
    );
    home.install(
        "b",
        Some(&(tagging("b-t", "transform") + &tagging("b-s", "sanitize"))),
    );
    home.install("c", Some(&tagging("c", "transform")));
    let answer = after_tool(&home.load(&[]), "x");
    assert_eq!(changed_content(&answer), Some("x|b-s|a1|a2|b-t|c"));
    assert_eq!(answer.changed_by, names(&["b", "a", "c"]));
}

#[test]
fn a_hook_that_names_no_phase_is_a_transform_hook() {
    let home = Home::new();
    home.install(
        "a",
        Some(&hook(
            "blocking",
            "return { content = call.content .. \"|a\" }",
        )),
    );
    home.install("b", Some(&tagging("b-s", "sanitize")));
    let answer = after_tool(&home.load(&[]), "x");
    assert_eq!(changed_content(&answer), Some("x|b-s|a"));
}

#[test]
fn hooks_order_puts_the_named_extensions_first_in_its_order() {
    let home = Home::new();
    home.install("a", Some(&tagging("a", "transform")));
    home.install(
        "b",
        Some(&(tagging("b-t", "transform") + &tagging("b-s", "sanitize"))),
    );
    home.install("c", Some(&tagging("c", "transform")));
    let answer = after_tool(
        &home.load(&[r#"hooks.order.after_tool=["fiber.test/c","fiber.test/b"]"#]),
        "x",
    );
    // The sanitize phase still runs first.
    assert_eq!(changed_content(&answer), Some("x|b-s|c|b-t|a"));
    assert_eq!(answer.changed_by, names(&["b", "c", "a"]));
    let answer = after_tool(
        &home.load(&[r#"hooks.order.after_tool=["fiber.test/b"]"#]),
        "x",
    );
    assert_eq!(changed_content(&answer), Some("x|b-s|b-t|a|c"));
    // An order for another point changes nothing here.
    let answer = after_tool(
        &home.load(&[r#"hooks.order.before_tool=["fiber.test/c"]"#]),
        "x",
    );
    assert_eq!(changed_content(&answer), Some("x|b-s|a|b-t|c"));
}

#[test]
fn a_non_blocking_failure_drops_its_change_with_a_notice_and_the_chain_goes_on() {
    let home = Home::new();
    home.install("a", Some(&hook("non-blocking", "error(\"kaput\")")));
    home.install("b", Some(&tagging("b", "transform")));
    let answer = after_tool(&home.load(&[]), "x");
    assert_eq!(changed_content(&answer), Some("x|b"));
    assert_eq!(answer.changed_by, names(&["b"]));
    assert_eq!(answer.notices.len(), 1);
    let notice = &answer.notices[0];
    assert_eq!(notice.code, ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some("fiber.test/a"));
    assert!(
        notice.message.contains("`after_tool`"),
        "{}",
        notice.message
    );
    assert!(notice.message.contains("kaput"), "{}", notice.message);
}

#[test]
fn a_blocking_failure_withholds_the_output_and_ends_the_chain() {
    let home = Home::new();
    home.install("a", Some(&hook("blocking", "error(\"kaput\")")));
    home.install("b", Some(&tagging("b", "sanitize")));
    // A later hook that would leave a notice: the chain ends before it.
    home.install("c", Some(&hook("non-blocking", "error(\"never\")")));
    let answer = after_tool(&home.load(&[]), "x");
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Withheld {
            extension: "fiber.test/a".into()
        }
    );
    assert_eq!(answer.changed_by, names(&["b", "a"]));
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
}

#[test]
fn a_return_that_is_not_a_change_is_the_hooks_failure() {
    for (body, why) in [
        ("return { status = \"failed\" }", "`status`"),
        ("return \"text\"", "a string, not a table"),
        ("return { content = 5 }", "`content` as a number"),
        ("return { artifact = true }", "`artifact` as a boolean"),
        ("return { colour = \"red\" }", "`colour`"),
        ("return { content = function() end }", "cannot be JSON"),
        ("return { \"a\", \"b\" }", "a list, not a table"),
    ] {
        let home = Home::new();
        home.install("odd", Some(&hook("non-blocking", body)));
        let answer = after_tool(&home.load(&[]), "x");
        assert_eq!(answer.outcome, AfterToolOutcome::Unchanged, "{body}");
        assert!(answer.changed_by.is_empty(), "{body}");
        assert_eq!(answer.notices.len(), 1, "{body}");
        assert!(
            answer.notices[0].message.contains(why),
            "{body}: {}",
            answer.notices[0].message
        );
    }
}

#[test]
fn nil_or_an_empty_table_changes_nothing_and_names_nobody() {
    for body in ["return nil", "return {}", ""] {
        let home = Home::new();
        home.install("idle", Some(&hook("blocking", body)));
        let answer = after_tool(&home.load(&[]), "x");
        assert_eq!(answer.outcome, AfterToolOutcome::Unchanged, "{body}");
        assert!(answer.changed_by.is_empty(), "{body}");
        assert!(answer.notices.is_empty(), "{body}");
    }
}

#[test]
fn details_and_the_last_artifact_carry_through_the_chain() {
    let home = Home::new();
    home.install(
        "a",
        Some(&hook(
            "blocking",
            "return { details = { n = 1 }, artifact = \"from a\" }",
        )),
    );
    home.install(
        "b",
        Some(&hook(
            "blocking",
            "return { artifact = \"from b \" .. call.details.n }",
        )),
    );
    let answer = after_tool(&home.load(&[]), "x");
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Changed {
            content: None,
            details: Some(json!({"n": 1})),
            artifact: Some("from b 1".into()),
        }
    );
    assert_eq!(answer.changed_by, names(&["a", "b"]));
}

fn declared_timeout(session: &SessionExtensions) -> Duration {
    session.lua[0].hooks().unwrap().by_point["after_tool"][0].timeout
}

#[test]
fn hook_timeout_ms_overrides_the_declared_timeout() {
    let home = Home::new();
    home.install("a", Some(&tagging("a", "transform")));
    assert_eq!(
        declared_timeout(&home.load(&[])),
        Duration::from_millis(1000)
    );
    assert_eq!(
        declared_timeout(&home.load(&["extensions.\"fiber.test/a\".hook_timeout_ms=5"])),
        Duration::from_millis(5)
    );
    assert_eq!(
        declared_timeout(&home.load(&["extensions.\"fiber.test/b\".hook_timeout_ms=5"])),
        Duration::from_millis(1000)
    );
}

#[test]
fn an_extension_for_another_api_or_a_process_extension_starts_no_vm() {
    let home = Home::new();
    home.install("old", Some(&tagging("old", "transform")));
    home.install("proc", Some(&tagging("proc", "transform")));
    home.install("lua", Some(&tagging("lua", "transform")));
    home.edit_manifest("old", |m| m["api"] = json!(2));
    home.edit_manifest("proc", |m| m["process"] = json!({"program": "true"}));
    let session = home.load(&[]);
    let loaded: Vec<String> = session.loaded().into_iter().map(|e| e.name).collect();
    assert_eq!(loaded, names(&["lua"]));
    assert_eq!(session.lua.len(), 1);
    assert_eq!(changed_content(&after_tool(&session, "x")), Some("x|lua"));
}

#[test]
fn memory_mib_raises_the_cap_a_hook_runs_under() {
    let grow = hook(
        "non-blocking",
        "local s = string.rep(\"x\", 3 * 1024 * 1024) return { content = tostring(#s) }",
    );
    let home = Home::new();
    home.install("small", Some(&grow));
    home.install("big", Some(&grow));
    home.edit_manifest("big", |m| m["memory_mib"] = json!(8));
    let session = home.load(&[r#"hooks.order.after_tool=["fiber.test/big"]"#]);
    let answer = after_tool(&session, "x");
    // The extension with 8 MiB makes its 3 MiB string; the one at the
    // default 1 MiB cap fails.
    assert_eq!(changed_content(&answer), Some("3145728"));
    assert_eq!(answer.changed_by, names(&["big"]));
    assert_eq!(answer.notices.len(), 1, "{:?}", answer.notices);
    assert_eq!(
        answer.notices[0].extension.as_deref(),
        Some("fiber.test/small")
    );
}

#[test]
fn a_hook_timeout_ms_of_zero_overrides_nothing() {
    let home = Home::new();
    home.install("a", Some(&tagging("a", "transform")));
    assert_eq!(
        declared_timeout(&home.load(&["extensions.\"fiber.test/a\".hook_timeout_ms=0"])),
        Duration::from_millis(1000)
    );
    assert_eq!(
        declared_timeout(&home.load(&["extensions.\"fiber.test/a\".hook_timeout_ms=1"])),
        Duration::from_millis(1)
    );
}

#[test]
fn each_hooks_phase_is_read_as_registered() {
    let home = Home::new();
    home.install(
        "phases",
        Some(
            "local run = function(call) end\n\
             fiber.hook(\"before_tool\", { phase = \"check\", timeout = 1, on_failure = \"blocking\", run = run })\n\
             fiber.hook(\"before_tool\", { phase = \"sanitize\", timeout = 1, on_failure = \"blocking\", run = run })\n\
             fiber.hook(\"before_tool\", { timeout = 1, on_failure = \"non-blocking\", run = run })\n",
        ),
    );
    let session = home.load(&[]);
    let declared = session.lua[0].hooks().unwrap();
    let read: Vec<(HookPhase, bool)> = declared.by_point["before_tool"]
        .iter()
        .map(|h| (h.phase, h.blocking))
        .collect();
    assert_eq!(
        read,
        [
            (HookPhase::Check, true),
            (HookPhase::Sanitize, true),
            (HookPhase::Transform, false),
        ]
    );
}
