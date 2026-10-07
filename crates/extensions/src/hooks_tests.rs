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
use crate::host::FakeLock;
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
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new(), locks)
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

/// Writes `text` to `file` in the installed `fiber.test/<short>`'s
/// directory.
fn installed_file(home: &Home, short: &str, file: &str, text: &str) {
    let path = home
        .home()
        .join("extensions")
        .join(format!("fiber.test-{short}"))
        .join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
}

#[test]
fn a_prompt_file_is_loaded_with_its_text() {
    let home = Home::new();
    home.install("noted", None);
    home.edit_manifest("noted", |m| m["prompt"] = json!("prompt.md"));
    installed_file(&home, "noted", "prompt.md", "Prefer read first.\n");
    let session = home.load(&[]);
    assert_eq!(
        session.prompts(),
        [(
            "fiber.test/noted".to_owned(),
            "Prefer read first.\n".to_owned()
        )]
    );
    assert!(
        session
            .loaded()
            .iter()
            .any(|e| e.name == "fiber.test/noted")
    );
}

#[test]
fn an_extension_without_a_prompt_has_no_prompt_text() {
    let home = Home::new();
    home.install("plain", None);
    let session = home.load(&[]);
    assert!(session.prompts().is_empty());
    assert!(
        session
            .loaded()
            .iter()
            .any(|e| e.name == "fiber.test/plain")
    );
}

#[test]
fn a_missing_prompt_file_fails_the_load_before_its_vm_starts() {
    let home = Home::new();
    home.install("broken", Some(&tagging("broken", "transform")));
    home.install("fine", Some(&tagging("fine", "transform")));
    home.edit_manifest("broken", |m| m["prompt"] = json!("gone.md"));
    let session = home.load(&[]);
    let loaded: Vec<String> = session.loaded().into_iter().map(|e| e.name).collect();
    assert_eq!(loaded, names(&["fine"]));
    assert!(
        session
            .dirs()
            .iter()
            .all(|(name, _)| name != "fiber.test/broken")
    );
    assert!(session.prompts().is_empty());
    // Its entry script never ran: only the fine extension's VM started.
    assert_eq!(session.lua.len(), 1);
    let notices = session.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/broken"));
    assert!(
        notices[0].message.contains("gone.md"),
        "{}",
        notices[0].message
    );
    assert_eq!(changed_content(&after_tool(&session, "x")), Some("x|fine"));
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
fn sections_omit_a_disabled_extension_and_one_without_opening() {
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
    // An `opening` with no paths yields no paths; the opening-message
    // build drops a section with no files, so none reaches the model.
    assert_eq!(
        session.sections(&project),
        [("fiber.test/empty".to_owned(), Vec::new(), None)]
    );
}

#[test]
fn an_extension_whose_entry_script_fails_has_no_section() {
    let home = Home::new();
    home.install("broken", Some("error(\"bad start\")\n"));
    home.edit_manifest("broken", |m| {
        m["opening"] = opening(&["index.md"], &[], None);
    });
    let session = home.load(&[]);
    assert!(session.loaded().is_empty());
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
    // The data-only extension starts no VM, and neither does the Lua one
    // without hooks: it is held only by its providers, if any
    // (`docs/model-routing.md`, "Model discovery").
    assert!(session.lua.is_empty());
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
fn a_damaged_extension_leaves_a_notice_and_the_rest_load() {
    let home = Home::new();
    home.install("fine", Some(&tagging("fine", "transform")));
    home.install("broken", Some(&tagging("broken", "transform")));
    fs::remove_file(home.home().join("extensions/fiber.test-broken/.fiber.json")).unwrap();
    let session = home.load(&[]);
    let loaded: Vec<String> = session.loaded().into_iter().map(|e| e.name).collect();
    assert_eq!(loaded, names(&["fine"]));
    let notices = session.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/broken"));
    assert_eq!(
        notices[0].message,
        "`fiber.test/broken` is damaged; run `fiber extension remove fiber.test/broken`, then install it again."
    );
    assert_eq!(changed_content(&after_tool(&session, "x")), Some("x|fine"));
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

#[test]
fn a_hook_reads_the_extension_s_project_settings_through_host_config() {
    let home = Home::new();
    home.install(
        "notes",
        Some(
            "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
               run = function(call) return { content = \"greeting:\" .. tostring(host.config.get(\"greeting\")) } end })\n",
        ),
    );
    // The per-project settings file, as `host.config.set` would write it.
    let file = home.home().join("projects/p/config/fiber.test-notes.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, r#"{"greeting": "hi"}"#).unwrap();
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    assert_eq!(
        changed_content(&after_tool(&session, "x")),
        Some("greeting:hi")
    );
}

#[test]
fn a_repository_key_the_manifest_does_not_list_is_one_notice() {
    let home = Home::new();
    home.install("notes", Some(&tagging("notes", "transform")));
    home.edit_manifest("notes", |m| m["repo_settings"] = json!(["listed"]));
    let file = home
        .root
        .path()
        .join("workspace/.fiber/config/fiber.test-notes.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, r#"{"listed": "yes", "nope": "evil"}"#).unwrap();
    let session = home.load(&[]);
    let all = session.notices();
    let ignored: Vec<_> = all
        .iter()
        .filter(|notice| notice.code == ErrorCode::ConfigKeyIgnored)
        .collect();
    assert_eq!(ignored.len(), 1, "{:?}", session.notices());
    assert_eq!(ignored[0].extension.as_deref(), Some("fiber.test/notes"));
    assert!(
        ignored[0].message.contains("`nope`"),
        "{}",
        ignored[0].message
    );
    // Collected once at load, not once per `get`.
    assert_eq!(session.notices().len(), 1);
}

/// One `fiber.provider` registration of `provider` with a `models` function.
fn registering(provider: &str) -> String {
    format!(
        "fiber.provider(\"{provider}\", {{ models = {{ timeout = 1000,\n\
           run = function() return {{}} end }} }})\n"
    )
}

#[test]
fn lua_providers_lists_each_registered_provider_in_provider_name_order() {
    let home = Home::new();
    home.install(
        "two",
        Some(&format!("{}{}", registering("b"), registering("a"))),
    );
    home.install(
        "none",
        Some("fiber.command(\"x\", { timeout = 1000, run = function(text) return text end })\n"),
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let listed: Vec<(String, String)> = session
        .lua_providers()
        .iter()
        .map(|(extension, provider)| (extension.clone(), provider.name().to_owned()))
        .collect();
    assert_eq!(
        listed,
        [
            ("fiber.test/two".to_owned(), "a".to_owned()),
            ("fiber.test/two".to_owned(), "b".to_owned()),
        ]
    );
    let sessioned = Arc::clone(&session);
    let functions = bounded(move || sessioned.lua_providers()[0].1.functions().unwrap());
    assert_eq!(functions, ["models"]);
    // A command-only extension stays loaded: its command is listed with its
    // description and the extension's name as the tag.
    let commands = session.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "x");
    assert_eq!(commands[0].tag, "fiber.test/none");
}

#[test]
fn a_refreshed_provider_the_session_does_not_use_is_unloaded() {
    let home = Home::new();
    home.install("used", Some(&registering("used")));
    home.install("other", Some(&registering("other")));
    home.install(
        "hooked",
        Some(&format!(
            "{}{}",
            tagging("h", "transform"),
            registering("hp")
        )),
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let other = session
        .lua_providers()
        .iter()
        .find(|(_, provider)| provider.name() == "other")
        .map(|(_, provider)| Arc::clone(provider))
        .expect("the other provider loads");
    let weak = Arc::downgrade(&other);
    // No cached copy, so the refresh runs; the thread holds the last Arc
    // beside this test's.
    let refresh = bounded(move || other.refresh(None)).expect("the refresh starts");
    let mut session = Arc::try_unwrap(session)
        .ok()
        .expect("the test holds the only Arc");
    session.retain_lua_providers(&["used"]);
    let kept: Vec<&str> = session
        .lua_providers()
        .iter()
        .map(|(_, provider)| provider.name())
        .collect();
    assert_eq!(kept, ["used"]);
    // The extension with hooks stays for them, even though its provider
    // went with the rest.
    assert_eq!(session.lua.len(), 1);
    bounded(move || {
        refresh
            .join()
            .expect("the refresh ends")
            .expect("models() runs")
    });
    assert!(
        weak.upgrade().is_none(),
        "the provider and its VM are gone once the refresh is written"
    );
}
