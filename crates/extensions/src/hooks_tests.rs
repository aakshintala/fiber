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
use contract::clock::Clock as _;
use contract::events::{CallStatus, Notice};
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, BeforeToolCall, CheckOutcome, Hooks,
    JobDelivery, Refusal,
};
use contract::shapes::{DeclaredEffects, Process};
use contract::tool::{Bound, SpillFile};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, json};

use super::SessionExtensions;
use crate::lua::HookPhase;
use crate::{ModelsArg, Origin, Request, plan};
use fakes::FakeLock;

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
    /// data-only extension when `init` is `None`. The manifest declares
    /// every provider file, with no URLs. A test of an undeclared package
    /// rewrites the declaration with `edit_manifest`.
    fn install(&self, short: &str, init: Option<&str>, providers: &[(&str, &str)]) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        let mut manifest = json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        if !providers.is_empty() {
            fs::create_dir_all(src.join("providers")).unwrap();
            let declared: serde_json::Map<String, serde_json::Value> = providers
                .iter()
                .map(|(name, lua)| {
                    fs::write(src.join("providers").join(format!("{name}.lua")), lua).unwrap();
                    ((*name).to_owned(), json!([]))
                })
                .collect();
            manifest["providers"] = serde_json::Value::Object(declared);
        }
        fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
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

    /// The session's extensions, loaded on a thread under [`fakes::MUST_SUCCEED_WITHIN`]: the
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

    fn config(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap()
    }

    #[track_caller]
    fn load(&self, overrides: &[&str]) -> Arc<SessionExtensions> {
        self.load_with_host(overrides, None)
    }

    /// Loads with the caller's clock kept, so the test can advance fake
    /// time past a parked deadline after the parked signal.
    fn load_with_clock(&self, overrides: &[&str], clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
        let config = self.config(overrides);
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, clock, locks, None)
        }))
    }

    #[track_caller]
    fn load_with_host(
        &self,
        overrides: &[&str],
        host: Option<Arc<crate::host::script::HostScript>>,
    ) -> Arc<SessionExtensions> {
        let config = self.config(overrides);
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, host)
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

/// Runs `f` on its own thread and waits for it under [`fakes::MUST_SUCCEED_WITHIN`], so a runtime
/// that never answers fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the extensions")
}

#[track_caller]
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
            spilled: None,

            not_regular: false,
            delivery: None,
        })
    })
}

fn changed_content(answer: &AfterToolAnswer) -> Option<&str> {
    match &answer.outcome {
        AfterToolOutcome::Changed { content, .. } => content.as_deref(),
        AfterToolOutcome::Unchanged
        | AfterToolOutcome::Withheld { .. }
        | AfterToolOutcome::Failed(_) => None,
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
    home.install("noted", None, &[]);
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
    home.install("plain", None, &[]);
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
    home.install("broken", Some(&tagging("broken", "transform")), &[]);
    home.install("fine", Some(&tagging("fine", "transform")), &[]);
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
    home.install("zeta", None, &[]);
    home.install("alpha", None, &[]);
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
    home.install("off", None, &[]);
    home.install("plain", None, &[]);
    home.install("empty", None, &[]);
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
    home.install("broken", Some("error(\"bad start\")\n"), &[]);
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
    home.install("data", None, &[]);
    home.install("quiet", Some("-- registers nothing\n"), &[]);
    home.install("off", Some(&tagging("off", "transform")), &[]);
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
fn loaded_extensions_carry_their_manifest_replaces_in_order() {
    let home = Home::new();
    home.install("a", None, &[]);
    home.edit_manifest("a", |m| m["replaces"] = json!(["write", "read", "write"]));
    home.install("b", None, &[]);
    let session = home.load(&[]);
    let loaded: Vec<(String, Vec<String>)> = session
        .loaded()
        .into_iter()
        .map(|e| (e.name, e.replaces))
        .collect();
    assert_eq!(
        loaded,
        [
            (
                "fiber.test/a".to_owned(),
                vec!["write".to_owned(), "read".to_owned(), "write".to_owned()],
            ),
            ("fiber.test/b".to_owned(), Vec::new()),
        ]
    );
    // A manifest name nothing registers is a declaration, not an audit:
    // both data-only extensions load with no notice.
    assert!(session.notices().is_empty());
    home.edit_manifest("a", |m| m["replaces"] = json!(["edit"]));
    let rebuilt = home.load(&[]);
    let again: Vec<(String, Vec<String>)> = rebuilt
        .loaded()
        .into_iter()
        .map(|e| (e.name, e.replaces))
        .collect();
    assert_eq!(
        again,
        [
            ("fiber.test/a".to_owned(), vec!["edit".to_owned()]),
            ("fiber.test/b".to_owned(), Vec::new()),
        ]
    );
    assert!(rebuilt.notices().is_empty());
}

#[test]
fn a_loaded_extensions_directory_is_listed_and_a_disabled_ones_is_not() {
    let home = Home::new();
    home.install("data", None, &[]);
    home.install("off", Some(&tagging("off", "transform")), &[]);
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
fn a_damaged_extension_is_skipped_without_a_notice_and_the_rest_load() {
    let home = Home::new();
    home.install("fine", Some(&tagging("fine", "transform")), &[]);
    home.install("broken", Some(&tagging("broken", "transform")), &[]);
    fs::remove_file(home.home().join("extensions/fiber.test-broken/.fiber.json")).unwrap();
    let session = home.load(&[]);
    let loaded: Vec<String> = session.loaded().into_iter().map(|e| e.name).collect();
    assert_eq!(loaded, names(&["fine"]));
    // `Providers::load` already reported it, so no notice here.
    assert!(session.notices().is_empty());
    assert_eq!(changed_content(&after_tool(&session, "x")), Some("x|fine"));
}

#[test]
fn an_unrelated_init_error_keeps_the_extensions_provider_data() {
    let home = Home::new();
    home.install("broken-data", Some("error('ordinary init failure')"), &[]);
    home.edit_manifest("broken-data", |manifest| {
        manifest["providers"] = json!({"p":[]})
    });
    let dir = home
        .home()
        .join("extensions/fiber.test-broken-data/providers");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("p.json"),
        json!({"name":"p", "models":[]}).to_string(),
    )
    .unwrap();
    let session = home.load(&[]);
    assert_eq!(session.notices().len(), 1);
    assert!(
        session.notices()[0]
            .message
            .contains("ordinary init failure")
    );
    assert!(session.rejected().is_empty());
    let (mut providers, notices) = crate::Providers::load(&home.home()).unwrap();
    assert!(notices.is_empty());
    providers.add_session(
        &session,
        &home.config(&[]),
        &|_, _| unreachable!("no credential command runs"),
        &|_| None,
    );
    assert_eq!(providers.names().collect::<Vec<_>>(), ["p"]);
}

#[test]
fn a_provider_start_reports_an_unreadable_provider_directory() {
    let home = Home::new();
    home.install("bad-directory", None, &[("p", "fiber.provider('p', {})")]);
    let session = home.load(&[]);
    assert!(session.notices().is_empty());
    assert_eq!(session.lua_providers().len(), 1);
    let path = home
        .home()
        .join("extensions/fiber.test-bad-directory/providers");
    fs::remove_dir_all(&path).unwrap();
    fs::write(&path, "not a directory").unwrap();
    let error = start(&home, &session, "fiber.test/bad-directory", "p")
        .err()
        .expect("provider directory must be readable");
    let crate::Error::Io {
        path: failed,
        source,
    } = error
    else {
        panic!("expected the directory read failure, got {error:?}");
    };
    assert_eq!(failed, path);
    assert_eq!(source.kind(), std::io::ErrorKind::NotADirectory);
}

#[test]
fn an_entry_script_that_fails_leaves_a_notice_and_is_not_loaded() {
    let home = Home::new();
    home.install("broken", Some("error(\"bad start\")\n"), &[]);
    home.install("fine", Some(&tagging("fine", "transform")), &[]);
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
        &[],
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
        ), &[]);
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
        &[],
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
            spilled: None,

            not_regular: false,
            delivery: None,
        })
    });
    assert_eq!(
        changed_content(&answer),
        Some("shell cancelled a.txt out 3 2 false")
    );
}

#[test]
fn the_hook_is_shown_each_delivery_word_with_the_starting_tool() {
    let home = Home::new();
    home.install(
        "seen",
        Some(&hook(
            "blocking",
            "return { content = tostring(call.delivery) .. \"|\" .. tostring(call.tool) }",
        )),
        &[],
    );
    let session = home.load(&[]);
    for (delivery, word) in [
        (JobDelivery::Completion, "completion"),
        (JobDelivery::Monitor, "monitor"),
        (JobDelivery::Delegate, "delegate"),
        (JobDelivery::OutputFile, "output_file"),
    ] {
        let answer = bounded({
            let session = Arc::clone(&session);
            move || {
                let arguments = Map::new();
                session.after_tool(&AfterToolCall {
                    tool: "shell",
                    arguments: &arguments,
                    status: CallStatus::Completed,
                    content: "out",
                    details: None,
                    process: None,
                    spilled: None,
                    not_regular: false,
                    delivery: Some(delivery),
                })
            }
        });
        assert_eq!(
            changed_content(&answer),
            Some(format!("{word}|shell").as_str())
        );
    }
    // An ordinary call has no delivery: the tool is shown as called.
    let answer = after_tool(&session, "x");
    assert_eq!(changed_content(&answer), Some("nil|read"));
    // A delivery from no tool leaves the tool out, so the hook sees nil.
    let answer = bounded({
        let session = Arc::clone(&session);
        move || {
            let arguments = Map::new();
            session.after_tool(&AfterToolCall {
                tool: "",
                arguments: &arguments,
                status: CallStatus::Completed,
                content: "out",
                details: None,
                process: None,
                spilled: None,
                not_regular: false,
                delivery: Some(JobDelivery::Completion),
            })
        }
    });
    assert_eq!(changed_content(&answer), Some("completion|nil"));
}

#[test]
fn the_hook_is_shown_a_spilled_output_file_as_path_and_size_without_content() {
    let home = Home::new();
    home.install(
        "spill",
        Some(&hook(
            "blocking",
            "return { content = tostring(call.content) .. \"|\" .. tostring(call.path) .. \
             \"|\" .. tostring(call.size) }",
        )),
        &[],
    );
    let session = home.load(&[]);
    let file = SpillFile {
        path: "/s/s_1/artifacts/s_x.txt".into(),
        size: 1_048_591,
    };
    let answer = bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "shell",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "",
            details: None,
            process: None,
            spilled: Some(&file),
            not_regular: false,
            delivery: Some(JobDelivery::OutputFile),
        })
    });
    assert_eq!(
        changed_content(&answer),
        Some("nil|/s/s_1/artifacts/s_x.txt|1048591")
    );
}

/// An `output_file` pass that found no regular file, as the pass asks it:
/// the pass failed before any hook ran, and no content is handed over.
#[track_caller]
fn not_regular(session: &Arc<SessionExtensions>) -> AfterToolAnswer {
    let session = Arc::clone(session);
    bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "shell",
            arguments: &arguments,
            status: CallStatus::Failed,
            content: "",
            details: None,
            process: None,
            spilled: None,
            delivery: Some(JobDelivery::OutputFile),
            not_regular: true,
        })
    })
}

#[test]
fn a_not_regular_probe_withholds_on_a_blocking_hook_without_running_it() {
    let home = Home::new();
    home.install(
        "scan",
        Some(&hook(
            "blocking",
            "return { content = call.content .. \"changed\" }",
        )),
        &[],
    );
    let session = home.load(&[]);
    let answer = not_regular(&session);
    // Withheld, not Changed: no hook ran on the probe.
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Withheld {
            extension: "fiber.test/scan".into(),
        }
    );
    assert!(answer.notices.is_empty());
}

#[test]
fn a_not_regular_probe_with_only_non_blocking_hooks_keeps_and_names_each() {
    let home = Home::new();
    home.install(
        "fmt",
        Some(&hook(
            "non-blocking",
            "return { content = call.content .. \"changed\" }",
        )),
        &[],
    );
    let session = home.load(&[]);
    let answer = not_regular(&session);
    // Unchanged, not Changed: no hook ran on the probe.
    assert_eq!(answer.outcome, AfterToolOutcome::Unchanged);
    assert_eq!(answer.notices.len(), 1);
    assert_eq!(
        answer.notices[0].extension.as_deref(),
        Some("fiber.test/fmt")
    );
}

#[test]
fn a_not_regular_probe_without_hooks_is_no_pass_to_fail() {
    let home = Home::new();
    home.install("plain", None, &[]);
    let session = home.load(&[]);
    let answer = not_regular(&session);
    assert_eq!(answer.outcome, AfterToolOutcome::Unchanged);
    assert!(answer.notices.is_empty());
}

#[test]
fn the_hook_is_shown_a_small_output_file_as_content_without_path() {
    let home = Home::new();
    home.install(
        "spill",
        Some(&hook(
            "blocking",
            "return { content = tostring(call.content) .. \"|\" .. tostring(call.path) }",
        )),
        &[],
    );
    let session = home.load(&[]);
    let answer = bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "shell",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "small",
            details: None,
            process: None,
            spilled: None,
            not_regular: false,
            delivery: Some(JobDelivery::OutputFile),
        })
    });
    assert_eq!(changed_content(&answer), Some("small|nil"));
}

#[test]
fn a_hook_returning_artifact_on_an_output_file_changes_only_the_artifact() {
    let home = Home::new();
    home.install(
        "spill",
        Some(&hook("blocking", "return { artifact = \"replaced\" }")),
        &[],
    );
    let session = home.load(&[]);
    let answer = bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "shell",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "small",
            details: None,
            process: None,
            spilled: None,
            not_regular: false,
            delivery: Some(JobDelivery::OutputFile),
        })
    });
    let AfterToolOutcome::Changed {
        content, artifact, ..
    } = answer.outcome
    else {
        panic!("expected a change, got {:?}", answer.outcome);
    };
    assert_eq!(content, None);
    assert_eq!(artifact.as_deref(), Some("replaced"));
}

#[test]
fn hooks_run_by_phase_then_by_name_and_each_sees_the_one_before() {
    let home = Home::new();
    home.install(
        "a",
        Some(&(tagging("a1", "transform") + &tagging("a2", "transform"))),
        &[],
    );
    home.install(
        "b",
        Some(&(tagging("b-t", "transform") + &tagging("b-s", "sanitize"))),
        &[],
    );
    home.install("c", Some(&tagging("c", "transform")), &[]);
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
        &[],
    );
    home.install("b", Some(&tagging("b-s", "sanitize")), &[]);
    let answer = after_tool(&home.load(&[]), "x");
    assert_eq!(changed_content(&answer), Some("x|b-s|a"));
}

#[test]
fn hooks_order_puts_the_named_extensions_first_in_its_order() {
    let home = Home::new();
    home.install("a", Some(&tagging("a", "transform")), &[]);
    home.install(
        "b",
        Some(&(tagging("b-t", "transform") + &tagging("b-s", "sanitize"))),
        &[],
    );
    home.install("c", Some(&tagging("c", "transform")), &[]);
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
    home.install("a", Some(&hook("non-blocking", "error(\"kaput\")")), &[]);
    home.install("b", Some(&tagging("b", "transform")), &[]);
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
    home.install("a", Some(&hook("blocking", "error(\"kaput\")")), &[]);
    home.install("b", Some(&tagging("b", "sanitize")), &[]);
    // A later hook that would leave a notice: the chain ends before it.
    home.install("c", Some(&hook("non-blocking", "error(\"never\")")), &[]);
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
        home.install("odd", Some(&hook("non-blocking", body)), &[]);
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
        home.install("idle", Some(&hook("blocking", body)), &[]);
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
        &[],
    );
    home.install(
        "b",
        Some(&hook(
            "blocking",
            "return { artifact = \"from b \" .. call.details.n }",
        )),
        &[],
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
    home.install("a", Some(&tagging("a", "transform")), &[]);
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
fn an_extension_for_another_api_starts_no_vm_while_a_process_extension_loads_without_one() {
    let home = Home::new();
    home.install("old", Some(&tagging("old", "transform")), &[]);
    home.install("proc", Some(&tagging("proc", "transform")), &[]);
    home.install("lua", Some(&tagging("lua", "transform")), &[]);
    home.edit_manifest("old", |m| m["api"] = json!(2));
    home.edit_manifest("proc", |m| {
        m["process"] = json!({"program": "true", "exit_timeout_ms": 0})
    });
    let session = home.load(&[]);
    // The wrong-API extension stays out; the process extension loads
    // without a VM, admitting the package with no registrations yet. No
    // doc promises the load order, so the test pins membership, not order.
    let loaded: Vec<String> = session.loaded().into_iter().map(|e| e.name).collect();
    assert!(loaded.contains(&"fiber.test/proc".to_owned()), "{loaded:?}");
    assert!(!loaded.contains(&"fiber.test/old".to_owned()), "{loaded:?}");
    let dirs: Vec<String> = session.dirs().into_iter().map(|(name, _)| name).collect();
    assert!(dirs.contains(&"fiber.test/proc".to_owned()), "{dirs:?}");
    assert!(!dirs.contains(&"fiber.test/old".to_owned()), "{dirs:?}");
    assert!(session.commands().is_empty());
    assert_eq!(changed_content(&after_tool(&session, "x")), Some("x|lua"));
}

#[test]
fn memory_mib_raises_the_cap_a_hook_runs_under() {
    let grow = hook(
        "non-blocking",
        "local s = string.rep(\"x\", 3 * 1024 * 1024) return { content = tostring(#s) }",
    );
    let home = Home::new();
    home.install("small", Some(&grow), &[]);
    home.install("big", Some(&grow), &[]);
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
    home.install("a", Some(&tagging("a", "transform")), &[]);
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
        ), &[]);
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
        ), &[]);
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
    home.install("notes", Some(&tagging("notes", "transform")), &[]);
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
        None,
        &[("b", &registering("b")), ("a", &registering("a"))],
    );
    home.install(
        "none",
        Some("fiber.command(\"x\", { timeout = 1000, run = function(text) return text end })\n"),
        &[],
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
fn a_command_conflict_is_one_notice() {
    // The probe build settles notices from metadata and the real build
    // settles the same final names for admission; publishing both would
    // record the conflict twice.
    fn registers(name: &str) -> String {
        format!(
            "fiber.command(\"sync\", {{ timeout = 1000, description = \"{name}\", run = function(text) return text end }})\n"
        )
    }
    let home = Home::new();
    home.install("a", Some(&registers("A")), &[]);
    home.install("b", Some(&registers("B")), &[]);
    let session = home.load(&[]);
    assert!(session.commands().is_empty(), "neither gets the name");
    let conflicts: Vec<_> = session
        .notices()
        .into_iter()
        .filter(|notice| notice.code == ErrorCode::CommandConflict)
        .collect();
    assert_eq!(conflicts.len(), 1, "one conflict notice, not two");
    assert!(
        conflicts[0].message.contains("`sync`"),
        "{}",
        conflicts[0].message
    );
    assert_eq!(session.notices().len(), 1);
}

#[test]
fn a_refreshed_provider_the_session_does_not_use_is_unloaded() {
    let home = Home::new();
    home.install("used", None, &[("used", &registering("used"))]);
    home.install("other", None, &[("other", &registering("other"))]);
    home.install(
        "hooked",
        Some(&tagging("h", "transform")),
        &[("hp", &registering("hp"))],
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
    let refresh =
        bounded(move || other.refresh(None, ModelsArg::default())).expect("the refresh starts");
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

#[test]
fn session_wait_for_runs_queues_an_exec_before_returning() {
    use crate::lua::{Pause, pause_settle, unpause_settle};
    let home = Home::new();
    let ready = home.root.path().join("ready");
    home.install(
        "barrier",
        Some(&format!(
            r#"fiber.command("run", {{ timeout = 5000, run = function()
  host.exec("sh", {{"-c", "echo $$ > \"$0\"", {ready:?}}})
  return "finished"
end }})"#,
            ready = ready.display().to_string(),
        )),
        &[],
    );
    let clock = FakeClock::new();
    let session = {
        let config = home.config(&[]);
        let home_dir = home.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded({
            let clock = Arc::clone(&clock);
            move || SessionExtensions::load(&home_dir, &config, clock, locks, None)
        }))
    };
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (tx, rx) = mpsc::channel();
    session.deliver_to(tx);
    let (arrived, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = std::sync::Mutex::new(released);
    pause_settle(Arc::new(move |_, pause| {
        if pause == Pause::ExecEnding {
            let _sent = arrived.send(());
            let _released = released.lock().unwrap().recv();
        }
    }));
    let lua = Arc::clone(&session.lua[0]);
    let (done, completed) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(lua.command("run", ""));
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrival)
        .expect("the exec ended before its delivery");
    let waiter = Arc::clone(&session);
    let (started_tx, started) = mpsc::channel();
    let (done, returned) = mpsc::channel();
    thread::spawn(move || {
        started_tx.send(thread::current().id()).unwrap();
        waiter.wait_for_runs();
        let _sent = done.send(());
    });
    let waiter_id = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&started)
        .expect("the waiter started");
    assert!(
        clock.await_thread_parked(
            waiter_id,
            None,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the session barrier parks while its exec is held"
    );
    assert!(
        returned.try_recv().is_err(),
        "the session barrier waits for its Lua VM"
    );
    release.send(()).unwrap();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&returned)
        .expect("the session barrier returned");
    let contract::inbox::Delivery::ExtensionExec(line) = rx
        .try_recv()
        .expect("the line is queued before the barrier returns")
    else {
        panic!("expected extension_exec");
    };
    assert_eq!(line.program, "sh");
    assert_eq!(line.process.exit_code, Some(0));
    assert!(matches!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&completed)
            .unwrap(),
        Err(crate::Error::Stopped { .. })
    ));
    unpause_settle();
}

/// One `fiber.provider` registration whose `models` runs `host.exec` before
/// returning an empty list, so a provider callback's run is what the
/// session inbox must carry as `extension_exec`.
fn exec_models(provider: &str) -> String {
    format!(
        "fiber.provider(\"{provider}\", {{ models = {{ timeout = 5000,\n\
           run = function() host.exec(\"sh\", {{\"-c\", \"exit 0\"}}) return {{}} end }},\n\
           credential = {{ timeout = 1000,\n\
           run = function() return {{ token = \"t\", expires_at = 1893456000 }} end }} }})\n"
    )
}

/// Drops every loaded provider handle while the test holds the only session
/// reference, so the next `start_provider` starts a fresh VM.
fn drop_providers(session: &mut Arc<SessionExtensions>) {
    Arc::get_mut(session)
        .expect("the test holds the only Arc")
        .retain_lua_providers(&[]);
}

/// Holds each provider `host.exec` at its end: the arrival proves the run
/// was admitted and ended before the shutdown wait could close admission.
fn hold_provider_exec() -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    use crate::lua::{Pause, Target, pause_settle};
    let (arrived, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = std::sync::Mutex::new(released);
    pause_settle(Arc::new(move |target, pause| {
        if pause == Pause::ExecEnding && matches!(target, Target::Provider { .. }) {
            let _sent = arrived.send(());
            let _released = released.lock().unwrap().recv();
        }
    }));
    (arrival, release)
}

/// Runs `models()` off to the side: the call blocks while its exec is held,
/// so the test thread stays free to start the shutdown wait first.
fn spawn_models(
    provider: Arc<crate::LuaProvider>,
) -> mpsc::Receiver<Result<Vec<config::ModelData>, crate::Error>> {
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(provider.models(&ModelsArg::default()));
    });
    result
}

/// Starts the session's shutdown wait off to the side. The caller checks the
/// waiter is still running then releases the held exec and joins the waiter
/// under the hang guard: the wait cannot return while the exec is held, and
/// returns once the hold lifts and the line queues.
fn spawn_waiter(session: &Arc<SessionExtensions>) -> mpsc::Receiver<()> {
    let waiter = Arc::clone(session);
    let (started_tx, started) = mpsc::channel();
    let (done, returned) = mpsc::channel();
    thread::spawn(move || {
        started_tx.send(()).unwrap();
        waiter.wait_for_runs();
        let _sent = done.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&started)
        .expect("the waiter started");
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&returned)
            .is_err(),
        "the shutdown wait blocks on the held provider exec"
    );
    returned
}

/// The held provider exec's line, read without waiting: it was queued before
/// the shutdown wait returned, and the wait returning proves nothing unless
/// the line is already here.
fn provider_exec_line(rx: &mpsc::Receiver<contract::inbox::Delivery>) {
    let contract::inbox::Delivery::ExtensionExec(line) = rx
        .try_recv()
        .expect("the provider exec line is queued before the wait returns")
    else {
        panic!("expected extension_exec");
    };
    assert_eq!(line.program, "sh");
    assert_eq!(line.process.exit_code, Some(0));
}

/// Birth order (and hook fixture) for a provider-exec shutdown test: swept
/// means the VM is born before the inbox exists, retained means it is born
/// after, and shared means the provider lends the hook's VM.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderBirth {
    Swept,
    Retained,
    Shared,
}

/// Runs the held-exec shutdown wait for `birth`, covering what the three
/// provider-exec tests share: the wait stays pending while the exec is held,
/// then returns `Stopped` for `models()` once the hold lifts with the exec's
/// line already queued.
fn drive_provider_exec(init: Option<&str>, birth: ProviderBirth) {
    use crate::lua::unpause_settle;
    let home = Home::new();
    let script = exec_models("p");
    home.install("prov", init, &[("p", script.as_str())]);
    let mut session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (provider, rx) = match birth {
        ProviderBirth::Swept => {
            drop_providers(&mut session);
            // The fresh VM is born before any inbox exists; the sweep in
            // `deliver_to` must still reach it.
            let provider = start(&home, &session, "fiber.test/prov", "p").unwrap();
            let (tx, rx) = mpsc::channel();
            session.deliver_to(tx);
            (provider, rx)
        }
        ProviderBirth::Retained => {
            let (tx, rx) = mpsc::channel();
            session.deliver_to(tx);
            drop_providers(&mut session);
            // The fresh VM is born after the inbox exists; the birth attach
            // must hand it the retained sender.
            let provider = start(&home, &session, "fiber.test/prov", "p").unwrap();
            (provider, rx)
        }
        ProviderBirth::Shared => {
            // The extension stays for its hook, so `start_provider` lends its
            // VM; the shared VM must carry the line exactly once.
            let provider = start(&home, &session, "fiber.test/prov", "p").unwrap();
            let (tx, rx) = mpsc::channel();
            session.deliver_to(tx);
            (provider, rx)
        }
    };
    let (arrival, release) = hold_provider_exec();
    let models = spawn_models(provider);
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrival)
        .expect("the provider exec ended before its delivery");
    let waiter = spawn_waiter(&session);
    release.send(()).unwrap();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&waiter)
        .expect("the shutdown wait returned after the release");
    // The shutdown wait stops the VM first, so the suspended `models()`
    // answers `Stopped` while its exec's line is still queued.
    assert!(matches!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&models)
            .expect("models() returned"),
        Err(crate::Error::Stopped { .. })
    ));
    provider_exec_line(&rx);
    if birth == ProviderBirth::Shared {
        assert!(rx.try_recv().is_err(), "the shared VM hands its line once");
    }
    unpause_settle();
}

#[test]
fn provider_exec_born_before_deliver_to_is_swept_and_waited_on() {
    drive_provider_exec(None, ProviderBirth::Swept);
}

#[test]
fn provider_exec_born_after_deliver_to_uses_the_retained_inbox() {
    drive_provider_exec(None, ProviderBirth::Retained);
}

#[test]
fn provider_exec_sharing_a_hook_vm_is_logged_once() {
    let hook = tagging("prov", "transform");
    drive_provider_exec(Some(&hook), ProviderBirth::Shared);
}

fn hold_init(url: &str) -> String {
    format!(
        "fiber.command(\"hold\", {{ timeout = 8000, run = function() local r = host.http({{ url = \"{url}\" }}) host.status(\"late\") host.log(\"late\") host.emit({{x=1}}) return r.body end }})\n"
    )
}

#[test]
fn session_seal_drops_late_status_log_and_emit_from_a_parked_command() {
    // Through the session-level `ExtensionDoor::seal`: a command parked on
    // `host.http` resumes after the seal, and its later `host.status`,
    // `host.log` and `host.emit` reach neither the Emit nor the inbox.
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let home = Home::new();
    home.install("a", Some(&hold_init(&url)), &[]);
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let recorder = Arc::new(fakes::EventLog::default());
    session.emit_to(Arc::clone(&recorder) as Arc<dyn contract::emit::Emit>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    let lua = Arc::clone(&session.lua[0]);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(lua.command("hold", ""));
    });
    assert!(
        server.await_requests(1, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "waited for the command to park on host.http"
    );
    contract::extension::ExtensionDoor::seal(&*session);
    server.release();
    let result = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("waited for the parked command to return");
    assert_eq!(result.unwrap(), "ok");
    assert!(
        recorder.events().is_empty(),
        "nothing reaches the Emit after the session seal"
    );
    assert!(
        inbox_rx.try_recv().is_err(),
        "nothing reaches the inbox after the session seal"
    );
}

/// A fake door for `SessionExtensions::drive_to`: records each drive and
/// answers `Ok(None)`, so `host.drive` returns true.
struct FakeDrive {
    calls: std::sync::Mutex<Vec<(String, String)>>,
    called: mpsc::Sender<()>,
}

impl contract::extension::Drive for FakeDrive {
    fn drive(
        &self,
        extension: &str,
        command: &str,
        _args: Map<String, serde_json::Value>,
        answer: contract::inbox::Ack,
    ) {
        self.calls
            .lock()
            .unwrap()
            .push((extension.to_owned(), command.to_owned()));
        let _sent = self.called.send(());
        answer.0(Ok(None));
    }
}

/// `drive_to` hands the driver to every extension's `host.drive`: before
/// it the command's drive raises `closing`, after it the drive reaches
/// that driver. A `drive_to` replaced with `()` would still raise
/// `closing` after it.
#[test]
fn drive_to_hands_the_driver_to_each_extension() {
    let home = Home::new();
    home.install(
        "driver",
        Some(
            "fiber.command(\"go\", { timeout = 5000, run = function()\n\
             local ok, err = pcall(host.drive, \"tools\", {})\n\
             if ok then return \"drove\" else return err.code end\n\
             end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    // No driver bound yet: `host.drive` raises `closing`.
    let lua = Arc::clone(&session.lua[0]);
    assert_eq!(bounded(move || lua.command("go", "")).unwrap(), "closing");
    let (called_tx, called_rx) = mpsc::channel();
    let drive = Arc::new(FakeDrive {
        calls: std::sync::Mutex::new(Vec::new()),
        called: called_tx,
    });
    session.drive_to(Arc::clone(&drive) as Arc<dyn contract::extension::Drive>);
    let lua = Arc::clone(&session.lua[0]);
    assert_eq!(bounded(move || lua.command("go", "")).unwrap(), "drove");
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&called_rx)
        .expect("waited for the drive to reach the driver");
    assert_eq!(
        drive.calls.lock().unwrap().clone(),
        [("fiber.test/driver".to_owned(), "tools".to_owned())]
    );
}

/// An entry script line that appends `x` to `counter` each time it runs.
fn counting(counter: &std::path::Path) -> String {
    format!(
        "local ok, seen = pcall(host.fs.read, \"{0}\")\n\
         if not ok or seen == nil then seen = \"\" end\n\
         host.fs.write(\"{0}\", seen .. \"x\")\n",
        counter.display()
    )
}

/// One `fiber.provider` registration of `provider` with a `credential`
/// function.
fn crediting(provider: &str) -> String {
    format!(
        "fiber.provider(\"{provider}\", {{ credential = {{ timeout = 1000,\n\
           run = function() return {{ token = \"t\", expires_at = 1893456000 }} end }} }})\n"
    )
}

/// `start_provider` on its own thread under [`fakes::MUST_SUCCEED_WITHIN`].
#[track_caller]
fn start(
    home: &Home,
    session: &Arc<SessionExtensions>,
    extension: &str,
    provider: &str,
) -> Result<Arc<crate::LuaProvider>, crate::Error> {
    let (session, config) = (Arc::clone(session), home.config(&[]));
    let (extension, provider) = (extension.to_owned(), provider.to_owned());
    bounded(move || {
        let started = session.start_provider(
            &config,
            FakeClock::new(),
            Arc::new(FakeLock::new()),
            &extension,
            &provider,
        )?;
        started.registers("credential").map(|yes| {
            assert!(yes, "the started provider answers for its functions");
            started
        })
    })
}

#[test]
fn a_later_provider_start_keeps_the_case_host_script_from_load() {
    let home = Home::new();
    let marker = home.root.path().join("starts");
    let url = "http://127.0.0.1:1/case-only";
    home.install(
        "prov",
        Some(&format!(
            "local ok, seen = pcall(host.fs.read, {:?})\n\
             if not ok or seen == nil then seen = \"\" end\n\
             host.fs.write({:?}, seen .. \"x\")\n\
             if seen ~= \"\" then\n\
             local reply = host.http({{url = {:?}}})\n\
             assert(reply.status == 200 and reply.body == \"started later\")\n\
             end\n",
            marker.display().to_string(),
            marker.display().to_string(),
            url,
        )),
        &[("p", &crediting("p"))],
    );
    let host = crate::host::script::HostScript::new(
        vec![crate::host::script::HttpEntry {
            request: json!({"url": url}),
            reply: Ok((200, b"started later".to_vec())),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut session = home.load_with_host(&[], Some(Arc::clone(&host)));
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    Arc::get_mut(&mut session)
        .expect("the test holds the only Arc")
        .retain_lua_providers(&[]);

    let provider = start(&home, &session, "fiber.test/prov", "p").unwrap();

    assert_eq!(provider.name(), "p");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "xx");
    assert!(host.unmet().is_empty(), "{:?}", host.unmet());
}

#[test]
fn start_provider_starts_a_fresh_vm_for_a_provider_only_extension() {
    let home = Home::new();
    let counter = home.root.path().join("runs");
    home.install("prov", Some(&counting(&counter)), &[("p", &crediting("p"))]);
    let mut session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x");
    Arc::get_mut(&mut session)
        .expect("the test holds the only Arc")
        .retain_lua_providers(&[]);
    let started = start(&home, &session, "fiber.test/prov", "p").unwrap();
    assert_eq!(started.name(), "p");
    assert_eq!(fs::read_to_string(&counter).unwrap(), "xx", "one fresh run");
    // A fresh VM registers no hook or command into the session.
    assert!(!session.has_hooks());
    assert!(session.commands().is_empty());
}

#[test]
fn start_provider_lends_the_vm_an_extension_keeps_for_its_hooks() {
    let home = Home::new();
    let counter = home.root.path().join("runs");
    home.install(
        "hooked",
        Some(&format!(
            "{}{}",
            counting(&counter),
            tagging("h", "transform"),
        )),
        &[("hp", &crediting("hp"))],
    );
    let mut session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    Arc::get_mut(&mut session)
        .expect("the test holds the only Arc")
        .retain_lua_providers(&[]);
    start(&home, &session, "fiber.test/hooked", "hp").unwrap();
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x", "no second run");
}

#[test]
fn start_provider_for_a_provider_the_vm_does_not_register_is_extension_failed() {
    let home = Home::new();
    home.install("prov", None, &[("p", &crediting("p"))]);
    home.install(
        "hooked",
        Some(&tagging("h", "transform")),
        &[("hp", &crediting("hp"))],
    );
    let session = home.load(&[]);
    for extension in ["fiber.test/prov", "fiber.test/hooked"] {
        let err = start(&home, &session, extension, "absent").err().unwrap();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed, "{extension}");
        assert!(err.to_string().contains("`absent`"), "{err}");
    }
    let err = start(&home, &session, "fiber.test/missing", "p")
        .err()
        .unwrap();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
}

#[test]
fn start_provider_reports_an_entry_script_that_fails() {
    let home = Home::new();
    let flag = home.root.path().join("fail");
    home.install(
        "prov",
        None,
        &[(
            "p",
            &format!(
                "local ok = pcall(host.fs.read, \"{}\")\nif ok then error(\"broken\") end\n{}",
                flag.display(),
                crediting("p"),
            ),
        )],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    fs::write(&flag, "").unwrap();
    let err = start(&home, &session, "fiber.test/prov", "p")
        .err()
        .unwrap();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    assert!(err.to_string().contains("broken"), "{err}");
}

/// A search backend fixture: `fiber.test/<short>` registers `backend`,
/// whose one result carries `tag` as its title.
fn backend_init(short: &str, backend: &str, tag: &str) -> (String, Option<String>) {
    (
        short.to_owned(),
        Some(format!(
            "fiber.search_backend(\"{backend}\", {{ timeout = 1000, run = function() \
               return {{ {{ title = \"{tag}\", url = \"https://example.com/\", snippet = \"s\" }} }} end }})\n"
        )),
    )
}

fn install_backend(home: &Home, short: &str, backend: &str, tag: &str) {
    let (name, init) = backend_init(short, backend, tag);
    let _ = name;
    home.install(short, init.as_deref(), &[]);
}

/// Runs the session's chosen backend once and returns its result's title.
fn backend_title(session: &Arc<SessionExtensions>) -> String {
    use contract::search::Domains;
    let backend = session.search_backend().expect("a backend is chosen");
    let found = backend
        .search("rust", &Domains::Any, &fakes::CancelToken::new())
        .expect("the search ran")
        .expect("the search was not cancelled");
    assert_eq!(found.len(), 1);
    found[0].title.clone()
}

#[test]
fn one_installed_backend_is_used_with_no_notice() {
    let home = Home::new();
    install_backend(&home, "one", "brave", "brave-hit");
    let session = home.load(&[]);
    assert!(session.search_backend().is_some());
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| notice.code != ErrorCode::WebSearchUnavailable),
        "{:?}",
        session.notices()
    );
    assert_eq!(backend_title(&session), "brave-hit");
}

#[test]
fn two_backends_with_no_setting_are_not_declared_and_name_the_setting() {
    let home = Home::new();
    install_backend(&home, "one", "brave", "brave-hit");
    install_backend(&home, "two", "kagi", "kagi-hit");
    let session = home.load(&[]);
    assert!(session.search_backend().is_none());
    let notices: Vec<_> = session
        .notices()
        .into_iter()
        .filter(|notice| notice.code == ErrorCode::WebSearchUnavailable)
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].message.contains("web_search.backend"),
        "{}",
        notices[0].message
    );
}

#[test]
fn two_backends_with_the_setting_name_the_second() {
    let home = Home::new();
    install_backend(&home, "one", "brave", "brave-hit");
    install_backend(&home, "two", "kagi", "kagi-hit");
    let session = home.load(&["web_search.backend=kagi"]);
    assert_eq!(backend_title(&session), "kagi-hit");
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| notice.code != ErrorCode::WebSearchUnavailable),
        "{:?}",
        session.notices()
    );
}

#[test]
fn a_setting_naming_a_missing_backend_is_not_declared_and_names_it() {
    let home = Home::new();
    install_backend(&home, "one", "brave", "brave-hit");
    let session = home.load(&["web_search.backend=missing"]);
    assert!(session.search_backend().is_none());
    let notices: Vec<_> = session
        .notices()
        .into_iter()
        .filter(|notice| notice.code == ErrorCode::WebSearchUnavailable)
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].message.contains("missing"),
        "{}",
        notices[0].message
    );
}

#[test]
fn an_extension_with_only_a_backend_stays_for_it() {
    let home = Home::new();
    install_backend(&home, "one", "brave", "brave-hit");
    let session = home.load(&[]);
    assert_eq!(session.lua.len(), 1);
}

#[test]
fn a_disabled_extensions_backend_is_not_counted() {
    let home = Home::new();
    install_backend(&home, "one", "brave", "brave-hit");
    install_backend(&home, "two", "kagi", "kagi-hit");
    let session = home.load(&["extensions.\"fiber.test/two\".enabled=false"]);
    assert_eq!(backend_title(&session), "brave-hit");
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| notice.code != ErrorCode::WebSearchUnavailable),
        "{:?}",
        session.notices()
    );
}

#[test]
fn a_lua_provider_its_manifest_does_not_declare_fails_the_extension() {
    let home = Home::new();
    home.install(
        "rogue",
        Some(&tagging("h", "transform")),
        &[("rogue", &crediting("rogue"))],
    );
    home.edit_manifest("rogue", |m| {
        m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    });
    home.install("good", None, &[("acme", &crediting("acme"))]);
    home.edit_manifest("good", |m| {
        m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    });
    let session = home.load(&[]);
    let names: Vec<String> = session.loaded().iter().map(|e| e.name.clone()).collect();
    assert!(!names.iter().any(|n| n == "fiber.test/rogue"), "{names:?}");
    assert!(names.iter().any(|n| n == "fiber.test/good"), "{names:?}");
    assert!(
        session.lua_providers().is_empty()
            || session
                .lua_providers()
                .iter()
                .all(|(e, _)| e != "fiber.test/rogue")
    );
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/rogue"));
    assert!(
        notices[0].message.contains("rogue"),
        "{}",
        notices[0].message
    );
    assert_eq!(
        after_tool(&session, "c").outcome,
        AfterToolOutcome::Unchanged
    );
}

#[test]
fn provider_only_and_mixed_packages_leave_the_provider_file_lazy() {
    for mixed in [false, true] {
        let home = Home::new();
        let marker = home.root.path().join("provider-runs");
        let init_marker = home.root.path().join("init-runs");
        let init = format!("{}{}", counting(&init_marker), tagging("h", "transform"));
        let script = format!("{}{}", counting(&marker), crediting("acme"));
        home.install("lazy", mixed.then_some(init.as_str()), &[("acme", &script)]);
        let session = home.load(&[]);
        assert!(session.notices().is_empty(), "{:?}", session.notices());
        assert!(!marker.exists());
        assert_eq!(session.lua_providers().len(), 1);
        let provider = Arc::clone(&session.lua_providers()[0].1);
        assert_eq!(
            bounded(move || provider.functions().unwrap()),
            ["credential"]
        );
        assert_eq!(fs::read_to_string(&marker).unwrap(), "x");
        if mixed {
            assert_eq!(fs::read_to_string(init_marker).unwrap(), "x");
            assert!(session.has_hooks());
        } else {
            assert!(!init_marker.exists());
            assert!(!session.has_hooks());
        }
    }
}

#[test]
fn a_provider_in_init_refuses_the_whole_extension_and_its_data() {
    for caught in [false, true] {
        let home = Home::new();
        let script = crediting("acme");
        let init = if caught {
            format!("pcall(function() {script} end)")
        } else {
            script
        };
        home.install("bad", Some(&init), &[]);
        home.edit_manifest("bad", |m| m["providers"] = json!({"acme":[]}));
        let dir = home.home().join("extensions/fiber.test-bad/providers");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("acme.json"),
            json!({"name":"acme", "models":[]}).to_string(),
        )
        .unwrap();
        let session = home.load(&[]);
        assert!(session.loaded().is_empty());
        assert_eq!(session.rejected(), ["fiber.test/bad"]);
        let notices = session.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
        assert!(notices[0].message.contains("providers/acme.lua"));
        let (mut providers, _) = crate::Providers::load(&home.home()).unwrap();
        assert!(providers.names().any(|name| name == "acme"));
        providers.add_session(
            &session,
            &home.config(&[]),
            &|_, _| unreachable!("no credential command runs"),
            &|_| None,
        );
        assert!(!providers.names().any(|name| name == "acme"));
    }
}

#[test]
fn two_providers_share_one_vm_until_every_handle_is_dropped() {
    let home = Home::new();
    let counter = home.root.path().join("module-runs");
    let a = format!("require('counter')\n{}", crediting("a"));
    let b = format!("require('counter')\n{}", crediting("b"));
    home.install("pair", None, &[("a", &a), ("b", &b)]);
    fs::write(
        home.home().join("extensions/fiber.test-pair/counter.lua"),
        counting(&counter),
    )
    .unwrap();
    let mut session = home.load(&[]);
    assert!(session.notices().is_empty());
    assert!(!counter.exists());
    Arc::get_mut(&mut session)
        .unwrap()
        .retain_lua_providers(&["a"]);
    let a = start(&home, &session, "fiber.test/pair", "a").unwrap();
    let b = start(&home, &session, "fiber.test/pair", "b").unwrap();
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x");
    drop(a);
    drop(b);
    Arc::get_mut(&mut session)
        .unwrap()
        .retain_lua_providers(&[]);
    let fresh = start(&home, &session, "fiber.test/pair", "b").unwrap();
    assert_eq!(fresh.name(), "b");
    assert_eq!(fs::read_to_string(counter).unwrap(), "xx");
}

#[test]
fn a_declared_lua_provider_loads() {
    let home = Home::new();
    home.install("good", None, &[("acme", &crediting("acme"))]);
    home.edit_manifest("good", |m| {
        m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    });
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    assert_eq!(session.lua_providers().len(), 1);
    assert_eq!(session.lua_providers()[0].1.name(), "acme");
}

#[test]
fn start_provider_fails_when_a_fresh_vm_registers_an_undeclared_provider() {
    let home = Home::new();
    let flag = home.root.path().join("flag");
    let flag_str = flag.display().to_string();
    home.install(
        "prov",
        None,
        &[(
            "acme",
            &format!(
                "local ok = pcall(host.fs.read, \"{flag_str}\")\n\
             if ok then {rogue} end\n{acme}",
                rogue = crediting("rogue"),
                acme = crediting("acme"),
            ),
        )],
    );
    home.edit_manifest("prov", |m| {
        m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    });
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    std::fs::write(&flag, "").unwrap();
    let err = start(&home, &session, "fiber.test/prov", "acme")
        .err()
        .unwrap();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    assert!(err.to_string().contains("rogue"), "{err}");
}

#[test]
fn a_data_provider_its_manifest_does_not_declare_keeps_the_extension_out_without_a_second_notice() {
    let home = Home::new();
    let marker = home.root.path().join("data-marker");
    home.install(
        "rogue",
        Some(&format!(
            "host.fs.write({:?}, \"x\")\n",
            marker.display().to_string()
        )),
        &[],
    );
    home.edit_manifest("rogue", |m| {
        m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    });
    let dir = home.home().join("extensions").join("fiber.test-rogue");
    std::fs::create_dir_all(dir.join("providers")).unwrap();
    std::fs::write(
        dir.join("providers").join("rogue.json"),
        json!({"name": "rogue", "models": []}).to_string(),
    )
    .unwrap();
    let session = home.load(&[]);
    assert!(!marker.is_file(), "the entry script never ran");
    let names: Vec<String> = session.loaded().iter().map(|e| e.name.clone()).collect();
    assert!(!names.iter().any(|n| n == "fiber.test/rogue"), "{names:?}");
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| { notice.extension.as_deref() != Some("fiber.test/rogue") }),
        "{:?}",
        session.notices()
    );
}

#[test]
fn an_undeclared_scripted_provider_fails_as_undeclared() {
    let home = Home::new();
    home.install("evil", None, &[("scripted", &crediting("scripted"))]);
    home.edit_manifest("evil", |m| {
        m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    });
    let session = home.load(&[]);
    let names: Vec<String> = session.loaded().iter().map(|e| e.name.clone()).collect();
    assert!(!names.iter().any(|n| n == "fiber.test/evil"), "{names:?}");
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/evil"));
    assert!(
        notices[0].message.contains("scripted"),
        "{}",
        notices[0].message
    );
    assert!(
        notices[0].message.contains("does not declare"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_hook_given_a_spilled_call_sees_its_path_and_size() {
    let home = Home::new();
    home.install(
        "spill",
        Some(&hook(
            "blocking",
            "return { content = call.path .. \"|\" .. tostring(call.size) }",
        )),
        &[],
    );
    let session = home.load(&[]);
    let file = SpillFile {
        path: "/s/s_1/artifacts/s_x.txt".into(),
        size: 1_048_591,
    };
    let answer = bounded(move || {
        let arguments = Map::new();
        session.after_tool(&AfterToolCall {
            tool: "shell",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "cut",
            details: None,
            process: None,
            spilled: Some(&file),

            not_regular: false,
            delivery: None,
        })
    });
    assert_eq!(
        changed_content(&answer),
        Some("/s/s_1/artifacts/s_x.txt|1048591")
    );
}

#[test]
fn a_hook_given_a_call_without_a_file_sees_no_path_or_size() {
    let home = Home::new();
    home.install(
        "spill",
        Some(&hook(
            "blocking",
            "return { content = tostring(call.path) .. \"|\" .. tostring(call.size) }",
        )),
        &[],
    );
    let session = home.load(&[]);
    let answer = after_tool(&session, "x");
    assert_eq!(changed_content(&answer), Some("nil|nil"));
}

/// One delivered event's kind and payload, as a watcher test builds it.
fn completed(n: u64) -> contract::events::Event {
    contract::events::Event::ToolCallCompleted(contract::events::ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![],
        details: Some(json!({"n": n})),
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        protected_paths: None,
        hosts_surfaced: vec![],
        provider_item: None,
        loaded: None,
    })
}

fn noticed(message: &str) -> contract::events::Event {
    contract::events::Event::Notice(Notice {
        code: ErrorCode::ExtensionFailed,
        message: message.to_owned(),
        extension: None,
    })
}

fn stepped() -> contract::events::Event {
    contract::events::Event::StepStarted(contract::events::Empty {})
}

/// One `host.status` line per watcher run, sent on a channel in run
/// order: the tests wait on the channel through a deadline, never on a
/// sleep.
struct StatusSignal(mpsc::Sender<String>);

impl contract::emit::Emit for StatusSignal {
    fn emit(&self, event: &contract::events::Event) {
        if let contract::events::Event::ExtensionUi(ui) = event
            && let contract::events::Ui::Status { status } = &ui.ui
        {
            let _ignored = self.0.send(status.clone());
        }
    }
}

/// Installs the status emitter on `session`, returning the lines' channel.
#[track_caller]
fn status_channel(session: &Arc<SessionExtensions>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    session.emit_to(Arc::new(StatusSignal(tx)));
    rx
}

/// A watcher on `tool_call_completed` beside an `after_tool` hook: each
/// completion arrives in session order, interleaved with the hook.
#[test]
fn a_watcher_receives_each_completion_in_session_order_with_the_hooks() {
    let home = Home::new();
    home.install(
        "ordered",
        Some(
            "fiber.hook(\"after_tool\", { timeout = 2000, on_failure = \"non-blocking\", run = function(call) host.status('hook:' .. call.tool) return {} end })\n\
             fiber.watch({\"tool_call_completed\"}, { timeout = 2000, run = function(ev) host.status('watch:' .. ev.payload.details.n) end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    session.deliver(&completed(1), Some(contract::Seq(0)));
    let (hook_tx, hook_rx) = mpsc::channel();
    let hook_session = Arc::clone(&session);
    thread::spawn(move || {
        hook_tx.send(after_tool(&hook_session, "first")).unwrap();
    });
    // The completion queues first, so the hook waits behind it: the single
    // worker runs them in stream order, and the hook's return proves it ran.
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the first completion"),
        "watch:1"
    );
    let answer = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&hook_rx)
        .expect("waited for the hook to run");
    assert!(matches!(answer.outcome, AfterToolOutcome::Unchanged));
    session.deliver(&completed(2), Some(contract::Seq(1)));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the hook's line"),
        "hook:read"
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the second completion"),
        "watch:2"
    );
}

/// An idle watcher receives an ephemeral offer: the positive case a
/// drop-all-ephemeral mutant fails.
#[test]
fn an_idle_watcher_receives_an_ephemeral_offer() {
    let home = Home::new();
    home.install(
        "idle",
        Some(
            "fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev) host.status('idle:' .. ev.payload.message) end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    // An ephemeral offer carries no seq, as the log gives it none.
    session.deliver(&noticed("hello"), None);
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the idle watcher to run"),
        "idle:hello"
    );
}

/// A parked watcher drops an ephemeral offer but queues the durable one,
/// then receives it after release; the cursor only advances on seqs.
#[test]
fn a_parked_watcher_drops_the_ephemeral_offer_and_queues_the_durable_one() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let home = Home::new();
    home.install(
        "busy",
        Some(&format!(
            "fiber.watch({{\"notice\"}}, {{ timeout = 5000, run = function(ev) host.http({{ url = \"{url}\" }}) host.status('ephemeral-ran') end }})\n\
             fiber.watch({{\"step_started\"}}, {{ timeout = 5000, run = function(ev) host.status('durable-ran') end }})\n"
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    session.deliver(&noticed("park"), None);
    assert!(
        server.await_requests(1, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "waited for the watcher to park on host.http"
    );
    // While it is parked the ephemeral offer is dropped and the durable one
    // queues: the parked watcher is busy, and the durable one must be too,
    // since the single worker cannot run it while the first is parked.
    session.deliver(&noticed("dropped"), None);
    session.deliver(&stepped(), Some(contract::Seq(7)));
    {
        let lua = session
            .lua_extension("fiber.test/busy")
            .expect("the watchers-only extension stays loaded");
        assert!(lua.is_watcher_busy(0), "the parked watcher is busy");
        assert!(
            lua.is_watcher_busy(1),
            "the durable offer queues behind the parked watcher"
        );
    }
    server.release();
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the parked watcher"),
        "ephemeral-ran"
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the queued durable one"),
        "durable-ran"
    );
    // The ephemeral offers carried no seq, so only the durable one moves a cursor.
    let cursors = session
        .cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(cursors.get(&("fiber.test/busy".to_owned(), 0)), None);
    assert_eq!(cursors.get(&("fiber.test/busy".to_owned(), 1)), Some(&7));
}

/// A kind no watcher registered for fires nothing and moves no cursor: the
/// later matching offer proves the stream processed everything, so the
/// unmatched one left no trace.
#[test]
fn an_unmatched_kind_fires_no_watcher() {
    let home = Home::new();
    home.install(
        "picky",
        Some(
            "fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev) host.status('picky:' .. ev.payload.message) end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    session.deliver(&stepped(), Some(contract::Seq(3)));
    session.deliver(&noticed("after"), None);
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the later matching offer"),
        "picky:after"
    );
    // The unmatched offer queued nothing and moved no cursor: the later
    // offer carries no seq, so the cursor map stays empty.
    assert!(
        session
            .cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

/// A cursor advances to each offered seq, in order.
#[test]
fn a_cursor_advances_to_each_offered_seq() {
    let home = Home::new();
    home.install(
        "cursor",
        Some("fiber.watch({\"step_started\"}, { timeout = 2000, run = function(ev) end })\n"),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    session.deliver(&stepped(), Some(contract::Seq(3)));
    session.deliver(&stepped(), Some(contract::Seq(5)));
    assert_eq!(
        *session
            .cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [("fiber.test/cursor".to_owned(), 0, 5)]
            .into_iter()
            .map(|(name, index, seq)| ((name, index), seq))
            .collect()
    );
}

/// A watcher registered mid-session receives only events offered after its
/// registration: A reaches the early watcher alone, B reaches both, in
/// extension-name order.
#[test]
fn a_late_watcher_receives_only_events_after_its_registration() {
    let home = Home::new();
    home.install(
        "early",
        Some(
            "fiber.watch({\"step_started\"}, { timeout = 2000, run = function(ev) host.status('early-ran') end })\n",
        ),
        &[],
    );
    let first = home.load(&[]);
    assert!(first.notices().is_empty(), "{:?}", first.notices());
    let first_statuses = status_channel(&first);
    first.deliver(&stepped(), Some(contract::Seq(4)));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&first_statuses)
            .expect("waited for the early watcher"),
        "early-ran"
    );
    home.install(
        "late",
        Some(
            "fiber.watch({\"step_started\"}, { timeout = 2000, run = function(ev) host.status('late-ran') end })\n",
        ),
        &[],
    );
    // Loading again registers the late extension mid-session: A was never
    // offered to a session that holds it.
    let second = home.load(&[]);
    assert!(second.notices().is_empty(), "{:?}", second.notices());
    let second_statuses = status_channel(&second);
    second.deliver(&stepped(), Some(contract::Seq(9)));
    // Each extension runs on its own worker, so the two runs race: the set
    // is what the fan-out pins — the late watcher sees only B, never A.
    let mut ran = vec![
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&second_statuses)
            .expect("waited for the first watcher"),
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&second_statuses)
            .expect("waited for the second watcher"),
    ];
    ran.sort();
    assert_eq!(
        ran,
        vec!["early-ran".to_owned(), "late-ran".to_owned()],
        "both watchers receive only B"
    );
    // The first session never sees B: its stream ended at A, and the two
    // sessions share nothing. The offer is synchronous and nothing was
    // ever queued on the first session after A, so no worker can deliver
    // later: the channel stays empty without waiting.
    assert!(
        first_statuses.try_recv().is_err(),
        "nothing further runs on the first session"
    );
}

/// Watchers of one extension run in registration order: the same worker
/// serves the one stream, so the fan-out's push order is the run order.
#[test]
fn one_extensions_watchers_run_in_registration_order() {
    let home = Home::new();
    home.install(
        "pair",
        Some(
            "fiber.watch({\"step_started\"}, { timeout = 2000, run = function(ev) host.status('first') end })\n\
             fiber.watch({\"step_started\"}, { timeout = 2000, run = function(ev) host.status('second') end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    session.deliver(&stepped(), Some(contract::Seq(2)));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the first watcher"),
        "first"
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the second watcher"),
        "second"
    );
}

/// One `host.status` line and one notice per watcher run, each on its own
/// channel in arrival order: the failure tests wait on channels through a
/// deadline, never on a sleep.
struct Observed {
    status: mpsc::Sender<String>,
    notice: mpsc::Sender<String>,
}

impl contract::emit::Emit for Observed {
    fn emit(&self, event: &contract::events::Event) {
        if let contract::events::Event::ExtensionUi(ui) = event
            && let contract::events::Ui::Status { status } = &ui.ui
        {
            let _ignored = self.status.send(status.clone());
        }
        if let contract::events::Event::Notice(notice) = event {
            let _ignored = self.notice.send(notice.message.clone());
        }
    }
}

/// Installs the status-and-notice emitter on `session`.
fn observe(session: &Arc<SessionExtensions>) -> (mpsc::Receiver<String>, mpsc::Receiver<String>) {
    let (status_tx, status_rx) = mpsc::channel();
    let (notice_tx, notice_rx) = mpsc::channel();
    session.emit_to(Arc::new(Observed {
        status: status_tx,
        notice: notice_tx,
    }));
    (status_rx, notice_rx)
}

/// An erroring watcher emits exactly one notice naming the extension, and
/// the stream continues with the next watcher.
#[test]
fn an_erroring_watcher_emits_one_notice_and_the_stream_continues() {
    let home = Home::new();
    home.install(
        "broke",
        Some(
            "fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev) error('boom') end })\n\
             fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev) host.status('second-ran') end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (statuses, notices) = observe(&session);
    session.deliver(&noticed("x"), None);
    let notice = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&notices)
        .expect("waited for the failure notice");
    assert!(
        notice.contains("fiber.test/broke")
            && notice.contains("watcher:0")
            && notice.contains("boom"),
        "the notice names the extension, the watcher and the error, {notice:?}"
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the next watcher"),
        "second-ran"
    );
    // Exactly one notice: the error ends the one delivery, not the stream.
    // Both deliveries already ended (the notice proves the first, the
    // status proves the second), so nothing more can arrive: the channel
    // stays empty without waiting.
    assert!(notices.try_recv().is_err(), "no second notice follows");
}

/// A watcher parked past its timeout yields the timeout notice naming the
/// exact declared ms, and the stream continues with the next watcher. The
/// run sleeps with no reply coming, so only the expiry arm can end it; the
/// session runs on a frozen fake clock, so `asked` is exactly the instant
/// read before `deliver`, and the test advances onto the deadline, which
/// wakes the loop through the clock subscription.
#[test]
fn a_watcher_parked_past_its_timeout_notices_and_the_stream_continues() {
    let home = Home::new();
    home.install(
        "slow",
        Some(
            "fiber.watch({\"notice\"}, { timeout = 300, run = function(ev) host.status('sleeping') host.exec('sleep', {'30'}) end })\n\
             fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev) host.status('after-timeout') end })\n",
        ),
        &[],
    );
    let clock = FakeClock::new();
    let session = home.load_with_clock(&[], Arc::clone(&clock));
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (statuses, notices) = observe(&session);
    // Retire the loop's fresh boot park (unbounded, from loading): only
    // parks made after this move count toward the wait below.
    clock.advance_when_parked(
        None,
        None,
        Duration::ZERO,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let before = clock.now();
    session.deliver(&noticed("x"), None);
    // The `sleeping` line proves the run reached the exec call; the park
    // follows under the same lock hold, and `mark_parked` waits for it.
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the watcher to start"),
        "sleeping"
    );
    // The delivery parks the sleeping watcher at asked + 300 ms; fake time
    // is frozen, so asked is exactly `before`. The wall-clock deadline only
    // bounds the waits for the parked delivery to appear and the loop to
    // notice the move.
    let thread = clock
        .mark_parked(
            before + Duration::from_millis(300),
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        )
        .expect("waited for the sleeping watcher to park at its deadline")
        .threads()
        .pop()
        .expect("one watcher parked at its deadline");
    // Pin the exec runner's fresh re-park at before + 10 ms before
    // advancing: its poll park would otherwise arrive beside the
    // watcher's deadline park and fail the unnamed wait.
    let poll = clock
        .mark_parked(
            before + Duration::from_millis(10),
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        )
        .expect("waited for the exec runner to park");
    assert!(
        clock.await_any_parked_since(&poll, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "waited for the exec runner to re-park"
    );
    clock.advance_when_parked(
        Some(before + Duration::from_millis(300)),
        Some(thread),
        Duration::from_millis(300),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let notice = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&notices)
        .expect("waited for the timeout notice");
    assert!(
        notice.contains("fiber.test/slow")
            && notice.contains("watcher:0")
            && notice.contains("300 ms"),
        "the notice names the extension, the watcher and the exact declared ms, {notice:?}"
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the next watcher"),
        "after-timeout"
    );
}

/// `host.tool` from a watcher answers the no-runner error while
/// `host.exec` runs: the refusal names the call site, not a tool failure.
#[test]
fn a_watcher_refuses_host_tool_but_runs_host_exec() {
    let home = Home::new();
    home.install(
        "gated",
        Some(
            "fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev)\n\
             local took, err = pcall(host.tool, 'read', {})\n\
             host.status('tool:' .. tostring(took) .. ':' .. tostring(err))\n\
             local ran = pcall(host.exec, 'true', {})\n\
             host.status('exec:' .. tostring(ran))\n\
             end })\n",
        ),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    session.deliver(&noticed("x"), None);
    let refused = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&statuses)
        .expect("waited for the tool refusal");
    assert!(
        refused.starts_with("tool:false:")
            && refused.contains("called outside a running tool call"),
        "the refusal names the call site, {refused:?}"
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the exec result"),
        "exec:true"
    );
}

/// A watcher pushed after `seal()` still runs: the seal drops routing, not
/// the stream. Service calls answer `closing`, local calls succeed, and the
/// late-bound emitter is gone, so the run reports through the filesystem.
/// The push goes through the drain's synchronous wait, as production runs
/// it after `fiber_exited`: no file poll, the wait itself is the barrier.
#[test]
fn a_sealed_watchers_run_with_closing_services_and_closed_state() {
    use contract::extension::ExtensionDoor as _;
    let home = Home::new();
    let probe = home.root.path().join("seal-probe");
    home.install(
        "sealed",
        Some(&format!(
            "fiber.watch({{\"notice\"}}, {{ timeout = 2000, run = function(ev)\n\
             local out = {{}}\n\
             local function probe(name, fn)\n\
             local ok, err = pcall(fn)\n\
             local detail = ok and \"ok\" or tostring((type(err) == \"table\" and (err.message or err.code)) or err)\n\
             out[#out + 1] = name .. \":\" .. detail\n\
             end\n\
             probe(\"model\", function() host.model({{ model = \"fake/m\", messages = {{ {{ role = \"user\", content = \"hi\" }} }}, max_tokens = 16 }}) end)\n\
             probe(\"drive\", function() host.drive(\"tools\", {{}}) end)\n\
             probe(\"delegate\", function() host.delegate({{ description = \"s\", prompt = \"p\", model = \"m\" }}, {{ timeout = 5000, run = function() end }}) end)\n\
             probe(\"state\", function() state.set(\"k\", 1) end)\n\
             probe(\"fs\", function() host.fs.write({probe:?}, \"wrote\") end)\n\
             probe(\"data_dir\", function() host.fs.write(host.data_dir(\"machine\") .. \"/wrote\", \"wrote\") end)\n\
             host.fs.write({probe:?} .. \".out\", table.concat(out, \"\\n\"))\n\
             end }})\n",
            probe = probe.display().to_string()
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    // Production order at session end: the seal drops routing, then the
    // state close lands with the lines it holds. The drain's synchronous
    // push-and-wait runs the post-seal delivery, as `drain_watchers` does:
    // it returns only after the run finished writing.
    session.seal();
    session.close_state();
    let lua = session
        .lua_extension("fiber.test/sealed")
        .expect("the watchers-only extension stays loaded");
    lua.drain_watcher(0, super::watcher_arg(&noticed("sealed")))
        .expect("the sealed watcher runs");
    let out = fs::read_to_string(probe.with_extension("out"))
        .expect("the sealed watcher wrote its report");
    assert!(
        out.contains("model:host.model: the session is closing"),
        "{out:?}"
    );
    assert!(
        out.contains("drive:host.drive: the session is closing"),
        "{out:?}"
    );
    assert!(
        out.contains("delegate:host.delegate: not available here"),
        "{out:?}"
    );
    assert!(out.contains("state:extension state is closed"), "{out:?}");
    assert!(out.contains("fs:ok"), "{out:?}");
    assert!(out.contains("data_dir:ok"), "{out:?}");
    assert_eq!(fs::read_to_string(&probe).unwrap(), "wrote");
}

/// A watcher queued when the signal stops the hub never starts: the stop
/// settles the parked delivery first, so the queued one stays queued.
#[test]
fn a_watcher_queued_at_a_signal_never_starts() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let home = Home::new();
    let second = home.root.path().join("second");
    home.install(
        "stopped",
        Some(&format!(
            "fiber.watch({{\"notice\"}}, {{ timeout = 5000, run = function(ev) host.http({{ url = \"{url}\" }}) end }})\n\
             fiber.watch({{\"notice\"}}, {{ timeout = 5000, run = function(ev) host.fs.write({second:?}, \"second\") end }})\n"
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    session.deliver(&noticed("park"), None);
    assert!(
        server.await_requests(1, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "waited for the first watcher to park on host.http"
    );
    // The one worker is parked on the first delivery, so the second stays
    // queued behind it; the stop lands before it can ever start. The stop
    // is synchronous, so no settle-signal is needed: after it returns the
    // hub takes no more jobs. A parked watcher delivery has no waiter, so
    // its abandoned run settles nothing visible.
    session.deliver(&noticed("queued"), None);
    crate::lua::hub::stop_every_callback();
    server.release();
    assert!(!second.exists(), "the queued watcher never started");
}

/// One durable `text_completed` line: notices are ephemeral and never
/// reach the log file, so the drain's fixtures are text lines.
fn texted(text: &str) -> contract::events::Event {
    contract::events::Event::TextCompleted(contract::events::TextCompleted {
        text: text.to_owned(),
        provider_item: None,
    })
}

/// A session-dir log holding nothing yet: the test appends after recording
/// the watermark, so the drain's range is exactly what followed
/// registration. The root keeps the log alive.
fn empty_log() -> (fakes::TempDir, Arc<log::Log>, PathBuf) {
    let root = fakes::TempDir::new("fiber-watch-drain");
    let sessions = root.path().join("sessions");
    let log = Arc::new(
        log::Log::create(
            &sessions,
            contract::SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    (root, log, sessions.join("s_1"))
}

/// The drain replays each watcher's `(cursor, end]` range in log order on
/// the one worker, advancing cursors and collecting nothing on success.
#[test]
fn drain_watchers_replays_each_watchers_range_in_order() {
    let home = Home::new();
    let first = home.root.path().join("first");
    let second = home.root.path().join("second");
    home.install(
        "drained",
        Some(&format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) local ok, seen = pcall(host.fs.read, {first:?}) if not ok or seen == nil then seen = \"\" end host.fs.write({first:?}, seen .. ev.payload.text .. \"\\n\") end }}) fiber.watch({{\"step_started\"}}, {{ timeout = 2000, run = function(ev) host.fs.write({second:?}, \"second\") end }})"
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, dir) = empty_log();
    session.set_watermark(log.count());
    log.append(&texted("a"), None, None).unwrap();
    log.append(&texted("b"), None, None).unwrap();
    log.append(&stepped(), None, None).unwrap();
    let (errors, stopped) = session.drain_watchers(&log, &dir);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!stopped, "a drain that runs to the end was not stopped");
    assert_eq!(fs::read_to_string(&first).unwrap(), "a\nb\n");
    assert_eq!(fs::read_to_string(&second).unwrap(), "second");
    assert_eq!(
        *session
            .cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [
            (("fiber.test/drained".to_owned(), 0), 1),
            (("fiber.test/drained".to_owned(), 1), 2)
        ]
        .into_iter()
        .collect()
    );
}

/// A watcher with no live offers drains exactly its post-watermark range:
/// lines before registration never reach it.
#[test]
fn a_late_watcher_drains_only_its_post_watermark_range() {
    let home = Home::new();
    let seen = home.root.path().join("seen");
    home.install(
        "late",
        Some(&format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) local ok, old = pcall(host.fs.read, {seen:?}) if not ok or old == nil then old = \"\" end host.fs.write({seen:?}, old .. ev.payload.text .. \"\\n\") end }})"
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, dir) = empty_log();
    log.append(&texted("before"), None, None).unwrap();
    // Registered after the last live offer: nothing was ever offered live,
    // so the cursor stays unset and the drain starts at the watermark.
    session.set_watermark(log.count());
    log.append(&texted("after"), None, None).unwrap();
    let (errors, stopped) = session.drain_watchers(&log, &dir);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!stopped, "a drain that runs to the end was not stopped");
    assert_eq!(fs::read_to_string(&seen).unwrap(), "after\n");
}

/// A signal during the drain stops it at once: the parked delivery's wait
/// answers `Stopped`, pre-signal errors still come back, and later
/// watchers never start.
#[test]
fn a_signal_during_the_drain_stops_it_at_once() {
    let home = Home::new();
    let later = home.root.path().join("later");
    home.install(
        "draining",
        Some(&format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) error('pre-signal') end }}) fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) host.status('parked') host.exec('sleep', {{'30'}}) end }}) fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) host.fs.write({later:?}, \"later\") end }})"
        )),
        &[],
    );
    let clock = FakeClock::new();
    let session = home.load_with_clock(&[], Arc::clone(&clock));
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let statuses = status_channel(&session);
    let (_logs, log, dir) = empty_log();
    session.set_watermark(log.count());
    log.append(&texted("drain-me"), None, None).unwrap();
    let draining = Arc::clone(&session);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ignored = done_tx.send(draining.drain_watchers(&log, &dir));
    });
    // The fake clock is held, never advanced: only the stop can wake the
    // parked delivery's wait.
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&statuses)
            .expect("waited for the draining watcher to park"),
        "parked"
    );
    crate::lua::hub::stop_every_callback();
    // The fake clock never advances, so only `Stopped` wakes the wait; a
    // missing stop fails at this bound.
    let (errors, stopped) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("waited for the drain to answer the stop");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(stopped, "a signal that ends the drain reports it stopped");
    assert!(
        errors[0].extension.contains("fiber.test/draining"),
        "{errors:?}"
    );
    assert!(errors[0].message.contains("pre-signal"), "{errors:?}");
    assert!(!later.exists(), "no later watcher starts after the stop");
}

/// A durable delivery still queued when the drain starts runs before the
/// drain replays past it: the live offer advanced the cursor past the only
/// line, so the drain's range is empty and it returns at once unless the
/// pre-replay wait holds it for the queued delivery. Without the wait the
/// drain returns while the delivery is still queued, and the `waiting`
/// receive below expires naming it.
#[test]
fn drain_watchers_waits_for_a_queued_durable_delivery() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let home = Home::new();
    let ran = home.root.path().join("ran");
    home.install(
        "covered",
        Some(&format!(
            "fiber.watch({{\"step_started\"}}, {{ timeout = 5000, run = function(ev) host.http({{ url = \"{url}\" }}) host.fs.write({ran:?}, \"ran\") end }})\n",
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, dir) = empty_log();
    session.set_watermark(log.count());
    log.append(&stepped(), None, None).unwrap();
    // The live offer parks on the held server and advances the cursor past
    // the line, so the drain can replay nothing: only the pre-replay wait
    // keeps it from returning before the queued delivery runs.
    session.deliver(&stepped(), Some(contract::Seq(0)));
    assert!(
        server.await_requests(1, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "waited for the live delivery to park on host.http"
    );
    let lua = session
        .lua_extension("fiber.test/covered")
        .expect("the watchers-only extension stays loaded");
    let (wait_tx, wait_rx) = mpsc::channel();
    lua.pause_at_drain_wait(Arc::new(move || {
        let _ignored = wait_tx.send(());
    }));
    let draining = Arc::clone(&session);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ignored = done_tx.send(draining.drain_watchers(&log, &dir));
    });
    // The wait engaged before any release: the parked delivery is still
    // queued, so only the wait holds the drain here.
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&wait_rx)
        .expect("waited for the drain to wait for the queued delivery");
    server.release();
    let (errors, stopped) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("waited for the drain after the release");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!stopped, "a drain that runs to the end was not stopped");
    // The queued delivery ran under the wait, before the drain returned:
    // its seq was already covered, so the drain replayed nothing and the
    // cursor it advanced at queue time still stands.
    assert_eq!(fs::read_to_string(&ran).unwrap(), "ran");
    assert_eq!(
        *session
            .cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [(("fiber.test/covered".to_owned(), 0), 0)]
            .into_iter()
            .collect()
    );
}

/// A live-covered line never replays at the drain: the cursor names the
/// last covered `seq`, so the drain starts one past it. Without the `+ 1`
/// the covered line redelivers (`* 1` stays on it) or the bound moves the
/// wrong way (`- 1` underflows past the range in release, panics in debug).
#[test]
fn drain_watchers_replays_only_past_the_live_covered_seq() {
    let home = Home::new();
    let seen = home.root.path().join("seen");
    home.install(
        "covered",
        Some(&format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) local ok, old = pcall(host.fs.read, {seen:?}) if not ok or old == nil then old = \"\" end host.fs.write({seen:?}, old .. ev.payload.text .. \"\\n\") end }})\n",
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, dir) = empty_log();
    session.set_watermark(log.count());
    log.append(&texted("a"), None, None).unwrap();
    log.append(&texted("b"), None, None).unwrap();
    // The live offer covers seq 0: the cursor advances at queue time, and
    // the drain's pre-replay wait holds it until that delivery ran, so the
    // live run wrote "a" before the drain replays.
    session.deliver(&texted("a"), Some(contract::Seq(0)));
    let (errors, stopped) = session.drain_watchers(&log, &dir);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!stopped, "a drain that runs to the end was not stopped");
    // The drain replays only "b": "a" was already covered live.
    assert_eq!(fs::read_to_string(&seen).unwrap(), "a\nb\n");
}

/// A log directory the drain cannot open replays nothing and collects the
/// failure: the drain is the report, since the log is closed to new lines.
/// Without the collection the drain returns no error for the missing log.
#[test]
fn drain_watchers_collects_a_log_it_cannot_open() {
    let home = Home::new();
    let ran = home.root.path().join("ran");
    home.install(
        "unread",
        Some(&format!(
            "fiber.watch({{\"step_started\"}}, {{ timeout = 2000, run = function(ev) host.fs.write({ran:?}, \"ran\") end }})\n",
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, _dir) = empty_log();
    session.set_watermark(log.count());
    let (errors, stopped) = session.drain_watchers(&log, &home.root.path().join("missing"));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(!stopped, "a drain that never replays was not stopped");
    assert_eq!(errors[0].extension, "log", "{errors:?}");
    assert_eq!(errors[0].callback, "drain", "{errors:?}");
    assert!(
        errors[0].message.contains("could not read the log"),
        "{errors:?}"
    );
    assert!(
        !ran.exists(),
        "nothing replays from a log that cannot be read"
    );
}

/// A log line that does not parse is collected, and the lines before it
/// still replay: the failure names the line without losing its prefix.
/// Without the collection the drain drops the bad line silent.
#[test]
fn drain_watchers_collects_an_unreadable_line_and_replays_its_prefix() {
    let home = Home::new();
    let seen = home.root.path().join("seen");
    home.install(
        "prefix",
        Some(&format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) local ok, old = pcall(host.fs.read, {seen:?}) if not ok or old == nil then old = \"\" end host.fs.write({seen:?}, old .. ev.payload.text .. \"\\n\") end }})\n",
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, dir) = empty_log();
    session.set_watermark(log.count());
    log.append(&texted("a"), None, None).unwrap();
    // The garbage lands after the one good line in the log's file.
    let events = dir.join("events.jsonl");
    let mut bytes = fs::read(&events).unwrap();
    bytes.extend_from_slice(b"not json\n");
    fs::write(&events, bytes).unwrap();
    let (errors, stopped) = session.drain_watchers(&log, &dir);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(!stopped, "a drain that runs to the end was not stopped");
    assert_eq!(errors[0].extension, "log", "{errors:?}");
    assert!(
        errors[0].message.contains("unreadable log line"),
        "{errors:?}"
    );
    // The good prefix still replays: only the bad line is skipped.
    assert_eq!(fs::read_to_string(&seen).unwrap(), "a\n");
}

/// A log line that parses as JSON but not as its kind's event is
/// collected, and never delivered: the failure names the line without
/// offering a half-read event to any watcher. Without the collection the
/// drain drops the corrupt line silent.
#[test]
fn drain_watchers_collects_a_line_it_cannot_read_as_an_event() {
    let home = Home::new();
    let seen = home.root.path().join("seen");
    home.install(
        "corrupt",
        Some(&format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) local ok, old = pcall(host.fs.read, {seen:?}) if not ok or old == nil then old = \"\" end host.fs.write({seen:?}, old .. ev.payload.text .. \"\\n\") end }})\n",
        )),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let (_logs, log, dir) = empty_log();
    session.set_watermark(log.count());
    log.append(&texted("a"), None, None).unwrap();
    // The corrupt line copies the good one with a payload its kind cannot
    // read, at a seq the drain's range covers, in the log's file.
    let events = dir.join("events.jsonl");
    let mut bytes = fs::read(&events).unwrap();
    let first: Vec<u8> = bytes
        .split(|b| *b == b'\n')
        .next()
        .unwrap_or_default()
        .to_vec();
    let mut bad: serde_json::Value = serde_json::from_slice(&first).unwrap();
    bad["payload"]["text"] = json!(123);
    bytes.extend_from_slice(&serde_json::to_vec(&bad).unwrap());
    bytes.push(b'\n');
    fs::write(&events, bytes).unwrap();
    let (errors, stopped) = session.drain_watchers(&log, &dir);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(!stopped, "a drain that runs to the end was not stopped");
    assert_eq!(errors[0].extension, "log", "{errors:?}");
    assert_eq!(errors[0].callback, "drain", "{errors:?}");
    assert!(
        errors[0].message.contains("could not read as an event"),
        "{errors:?}"
    );
    // Only the good line replays: the corrupt one is never delivered.
    assert_eq!(fs::read_to_string(&seen).unwrap(), "a\n");
}

/// A watch refusal surfaces as the registration notice naming the
/// extension (Ruling 6): a `fiber.watch` without a `timeout` registers
/// nothing and leaves one notice. Without the surfacing, the session
/// loads silent.
#[test]
fn a_watch_refusal_surfaces_as_the_registration_notice() {
    let home = Home::new();
    home.install(
        "nowatch",
        Some("fiber.watch({\"notice\"}, { run = function(ev) end })\n"),
        &[],
    );
    let session = home.load(&[]);
    assert!(session.watchers.is_empty(), "nothing registers");
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0].extension.as_deref(),
        Some("fiber.test/nowatch"),
        "{notices:?}"
    );
    assert!(
        notices[0].message.contains("`watcher:0`")
            && notices[0].message.contains("missing `timeout`"),
        "{notices:?}"
    );
}

/// The load gate and the removed-extension check: these tests install a
/// stand-in under the first-party `hooks` name and pin the gate around it.
/// The stand-in keeps the tests about the gate, not the package's Lua: a
/// one-line `init` script is enough to prove loading, skipping and notices.
/// Installs the package named `github.com/aakshintala/fiber/extensions/hooks`
/// with `init` as its entry script.
fn install_hooks(home: &Home, init: &str) {
    let src = home.root.path().join("src").join("hooks-first-party");
    fs::create_dir_all(&src).unwrap();
    // The package's own manifest, so the tests load what ships.
    fs::write(
        src.join("extension.json"),
        include_str!("../../../extensions/hooks/extension.json"),
    )
    .unwrap();
    fs::write(src.join("init.lua"), init).unwrap();
    plan(
        &home.home(),
        &Request::Path(src),
        "0.1.0",
        &Origin::github(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// One `extensions."<hooks>".settings.hooks=<entries>` override.
fn hooks_override(entries: &str) -> String {
    format!(
        "extensions.\"{}\".settings.hooks={entries}",
        config::full_name("hooks")
    )
}

/// Loads the session with `entries` as the declared `hooks` settings.
fn load_with_hooks(home: &Home, entries: &str) -> Arc<SessionExtensions> {
    let setting = hooks_override(entries);
    home.load(&[setting.as_str()])
}

fn hooks_config(home: &Home, entries: &str) -> Config {
    let setting = hooks_override(entries);
    home.config(&[setting.as_str()])
}

const BLOCKING_ENTRY: &str =
    r#"{"audit":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"blocking"}}"#;
const NON_BLOCKING_ENTRY: &str = r#"{"audit":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"non-blocking"}}"#;
const NO_ON_FAILURE_ENTRY: &str =
    r#"{"audit":{"point":"after_tool","command":"true","timeout":1000}}"#;
const UNKNOWN_ON_FAILURE_ENTRY: &str =
    r#"{"audit":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"sometimes"}}"#;
const NON_STRING_ON_FAILURE_ENTRY: &str =
    r#"{"audit":{"point":"after_tool","command":"true","timeout":1000,"on_failure":5}}"#;
const WATCHER_ENTRY: &str =
    r#"{"ping":{"watch":["turn_completed"],"command":"true","timeout":1000}}"#;

#[test]
fn hooks_without_a_hooks_key_does_not_load() {
    let home = Home::new();
    install_hooks(&home, "-- no hooks registered\n");
    let session = home.load(&[]);
    assert!(
        !session.loaded_names().contains(&config::full_name("hooks")),
        "{:?}",
        session.loaded_names()
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn hooks_with_a_hooks_key_loads() {
    let home = Home::new();
    install_hooks(&home, "-- no hooks registered\n");
    let session = load_with_hooks(&home, BLOCKING_ENTRY);
    assert!(
        session.loaded_names().contains(&config::full_name("hooks")),
        "{:?}",
        session.loaded_names()
    );
}

#[test]
fn hooks_with_an_empty_hooks_key_loads_silently() {
    let home = Home::new();
    install_hooks(&home, "-- no hooks registered\n");
    let session = load_with_hooks(&home, "{}");
    assert!(
        session.loaded_names().contains(&config::full_name("hooks")),
        "{:?}",
        session.loaded_names()
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

/// `hooks_state` maps declarations to the gate's state: nothing declared
/// or already loaded is `Ready`; any `blocking` (or missing or invalid
/// `on_failure`) entry without the extension loaded is `MissingBlocking`;
/// watcher entries never gate, and an empty `hooks` key loads silently.
#[test]
fn hooks_state_maps_declarations_to_a_gate_state() {
    let home = Home::new();
    let hooks = config::full_name("hooks");
    // (entries JSON, `None` for no `hooks` key; extension loaded; state)
    // Every `MissingBlocking` below names the sorted blockers it gates on
    // (`docs/invocation.md:1160`).
    let cases: &[(Option<&str>, bool, super::HooksState)] = &[
        // No declarations: nothing to gate.
        (None, false, super::HooksState::Ready),
        // The extension loaded: declared entries can run.
        (Some(BLOCKING_ENTRY), true, super::HooksState::Ready),
        // A lone `on_failure`-less entry fails closed like `blocking`.
        (
            Some(NO_ON_FAILURE_ENTRY),
            false,
            super::HooksState::MissingBlocking {
                names: vec!["audit".to_owned()],
            },
        ),
        // An `on_failure`-less entry beside a `blocking` one still gates.
        (
            Some(
                r#"{"plain":{"point":"after_tool","command":"true","timeout":1000},"guard":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"blocking"}}"#,
            ),
            false,
            super::HooksState::MissingBlocking {
                // Blocker names arrive sorted (`docs/invocation.md:1160`).
                names: vec!["guard".to_owned(), "plain".to_owned()],
            },
        ),
        // An unknown `on_failure` is not consent to fail open.
        (
            Some(UNKNOWN_ON_FAILURE_ENTRY),
            false,
            super::HooksState::MissingBlocking {
                names: vec!["audit".to_owned()],
            },
        ),
        // A non-string `on_failure` is not consent either.
        (
            Some(NON_STRING_ON_FAILURE_ENTRY),
            false,
            super::HooksState::MissingBlocking {
                names: vec!["audit".to_owned()],
            },
        ),
        // Watcher entries never gate the start.
        (Some(WATCHER_ENTRY), false, super::HooksState::Ready),
        // An empty `hooks` key loads silently.
        (Some("{}"), false, super::HooksState::Ready),
    ];
    for (entries, is_loaded, expected) in cases {
        let config = match entries {
            Some(json) => hooks_config(&home, json),
            None => home.config(&[]),
        };
        let loaded = is_loaded
            .then(|| hooks.clone())
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(
            super::hooks_state(&config, &loaded).unwrap(),
            *expected,
            "entries: {entries:?}, loaded: {is_loaded}"
        );
    }
}

#[test]
fn hooks_state_names_every_blocker_but_no_watcher_or_non_blocking_entry() {
    let home = Home::new();
    let entries = r#"{"z":{"on_failure":"blocking"},"a":false,"missing":{},"invalid":{"on_failure":7},"notice":{"on_failure":"non-blocking"},"watch":{"watch":[],"on_failure":"blocking"}}"#;
    // A non-object entry fails before any installed/missing decision, naming
    // the first malformed entry in sorted key order (`docs/extensions.md`,
    // "Hooks declared in configuration").
    let error = super::hooks_state(&hooks_config(&home, entries), &[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{error:?}");
    let crate::Error::Config(config::ConfigError::WrongType { key, .. }) = &error else {
        panic!("expected WrongType, got {error:?}");
    };
    assert_eq!(key, "hooks.a");
    assert!(error.to_string().contains("hooks.a"), "{error}");
    let objects = r#"{"z":{"on_failure":"blocking"},"a":{"on_failure":"blocking"},"notice":{"on_failure":"non-blocking"},"watch":{"watch":[]}}"#;
    // Blocker names arrive sorted (`docs/invocation.md:1160`).
    assert_eq!(
        super::hooks_state(&hooks_config(&home, objects), &[]).unwrap(),
        super::HooksState::MissingBlocking {
            names: vec!["a".into(), "z".into()]
        }
    );
    assert_eq!(
        super::hooks_state(&hooks_config(&home, objects), &[config::full_name("hooks")]).unwrap(),
        super::HooksState::Ready
    );
}

/// A non-object entry fails `config_invalid` naming the entry, whatever the
/// key order and whether or not `hooks` is installed. The named entry is
/// the malformed one (`docs/extensions.md`, "Hooks declared in
/// configuration").
#[test]
fn hooks_state_rejects_a_non_object_entry_before_any_installed_decision() {
    let home = Home::new();
    let hooks = config::full_name("hooks");
    // Each acceptance map arrives in both file orders: either order names
    // the malformed entry, `z` here and `a` below.
    for (entries, expected) in [
        (r#"{"a":{"on_failure":"blocking"},"z":false}"#, "hooks.z"),
        (r#"{"z":false,"a":{"on_failure":"blocking"}}"#, "hooks.z"),
        (r#"{"a":false,"z":{"on_failure":"blocking"}}"#, "hooks.a"),
        (r#"{"z":{"on_failure":"blocking"},"a":false}"#, "hooks.a"),
    ] {
        for loaded in [Vec::new(), vec![hooks.clone()]] {
            let error = super::hooks_state(&hooks_config(&home, entries), &loaded).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ConfigInvalid, "entries: {entries}");
            let crate::Error::Config(config::ConfigError::WrongType { key, .. }) = &error else {
                panic!("expected WrongType, got {error:?}");
            };
            assert_eq!(key, expected, "entries: {entries}");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
    // Several malformed entries report the first in sorted key order, not the
    // first in the file order (`docs/extensions.md`, "Hooks declared in
    // configuration").
    let error = super::hooks_state(&hooks_config(&home, r#"{"b":false,"a":5}"#), &[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{error:?}");
    let crate::Error::Config(config::ConfigError::WrongType { key, .. }) = &error else {
        panic!("expected WrongType, got {error:?}");
    };
    assert_eq!(key, "hooks.a");
    assert!(error.to_string().contains("hooks.a"), "{error}");
}

/// A non-object `hooks` value fails `config_invalid` naming the `hooks` key,
/// whether or not `hooks` is installed.
#[test]
fn hooks_state_rejects_a_non_object_hooks_value_naming_hooks() {
    let home = Home::new();
    let hooks = config::full_name("hooks");
    for loaded in [Vec::new(), vec![hooks.clone()]] {
        let error = super::hooks_state(&hooks_config(&home, "false"), &loaded).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{error:?}");
        let crate::Error::Config(config::ConfigError::WrongType { key, .. }) = &error else {
            panic!("expected WrongType, got {error:?}");
        };
        assert_eq!(key, "hooks");
        assert!(error.to_string().contains("hooks"), "{error}");
    }
}

#[test]
fn hooks_state_is_missing_notice_for_unloaded_non_blocking_hooks() {
    let home = Home::new();
    match super::hooks_state(&hooks_config(&home, NON_BLOCKING_ENTRY), &[]).unwrap() {
        super::HooksState::MissingNotice { notice } => {
            assert_eq!(notice.code, ErrorCode::ExtensionMissing);
            assert_eq!(
                notice.extension.as_deref(),
                Some(config::full_name("hooks").as_str())
            );
            assert!(
                notice.message.contains("fiber extension install hooks"),
                "{}",
                notice.message
            );
        }
        state @ (super::HooksState::Ready | super::HooksState::MissingBlocking { .. }) => {
            panic!("expected MissingNotice, got {state:?}");
        }
    }
}

#[test]
fn declared_non_blocking_hooks_without_the_package_leave_one_notice() {
    let home = Home::new();
    let session = load_with_hooks(&home, NON_BLOCKING_ENTRY);
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionMissing);
    assert_eq!(
        notices[0].extension.as_deref(),
        Some(config::full_name("hooks").as_str())
    );
}

#[test]
fn declared_blocking_hooks_without_the_package_stay_silent_until_the_gate() {
    let home = Home::new();
    let session = load_with_hooks(&home, BLOCKING_ENTRY);
    assert!(
        !session.loaded_names().contains(&config::full_name("hooks")),
        "{:?}",
        session.loaded_names()
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

/// The first-party entry script registers a valid watcher entry beside
/// one missing `timeout`: the valid entry registers, and the notice for
/// the invalid one names the entry and the field (`docs/extensions.md`,
/// "Hooks declared in configuration").
#[test]
fn a_watcher_entry_missing_timeout_names_the_entry_and_field() {
    let home = Home::new();
    install_hooks(&home, include_str!("../../../extensions/hooks/init.lua"));
    let session = load_with_hooks(
        &home,
        r#"{"broken":{"watch":["turn_completed"],"command":"true"},"ok":{"watch":["turn_completed"],"command":"true","timeout":1000}}"#,
    );
    let watchers = session.lua[0].watchers().unwrap();
    assert_eq!(watchers.watchers.len(), 1, "{watchers:?}");
    assert_eq!(watchers.watchers[0].kinds, ["turn_completed"]);
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0].extension.as_deref(),
        Some(config::full_name("hooks").as_str())
    );
    assert!(
        notices[0]
            .message
            .contains("`timeout` hook not registered for entry `broken`"),
        "{notices:?}"
    );
}

/// The person's `hook_timeout_ms` override reaches watcher entries the
/// first-party script registers, as it does hook entries, while another
/// extension's override changes nothing (`docs/configuration.md`,
/// `extensions."<name>".hook_timeout_ms`).
#[test]
fn hook_timeout_ms_overrides_a_declared_watcher_timeout() {
    let home = Home::new();
    install_hooks(&home, include_str!("../../../extensions/hooks/init.lua"));
    let entries = r#"{"guard":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"blocking"},"note":{"watch":["turn_completed"],"command":"true","timeout":1000}}"#;
    let setting = hooks_override(entries);
    let hooks = config::full_name("hooks");
    let own = format!("extensions.\"{hooks}\".hook_timeout_ms=5");
    let session = home.load(&[setting.as_str(), own.as_str()]);
    assert_eq!(
        session.lua[0].hooks().unwrap().by_point["after_tool"][0].timeout,
        Duration::from_millis(5)
    );
    assert_eq!(
        session.lua[0].watchers().unwrap().watchers[0].timeout,
        Duration::from_millis(5)
    );
    let other = "extensions.\"fiber.test/b\".hook_timeout_ms=5";
    let plain = home.load(&[setting.as_str(), other]);
    assert_eq!(
        plain.lua[0].hooks().unwrap().by_point["after_tool"][0].timeout,
        Duration::from_millis(1000)
    );
    assert_eq!(
        plain.lua[0].watchers().unwrap().watchers[0].timeout,
        Duration::from_millis(1000)
    );
}

#[test]
fn unreadable_hooks_settings_skip_the_load_and_report_config_invalid() {
    let home = Home::new();
    install_hooks(&home, "-- no hooks registered\n");
    let file = home.home().join("config").join("hooks.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{not json").unwrap();
    let session = home.load(&[]);
    assert!(
        !session.loaded_names().contains(&config::full_name("hooks")),
        "{:?}",
        session.loaded_names()
    );
    // Malformed settings are not a missing extension: the read's error
    // propagates, and the gate fails the start with its code.
    let error = super::hooks_state(&home.config(&[]), &[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
}

// ---------- Process package admission ----------
//
// A process package's registrations fold into the session's registries
// through the same settle and notice paths Lua registrations take, with
// the process references behind the session's runtime.

use crate::package::{Package as ProcessPackage, PackageOptions as ProcessPackageOptions};
use crate::process::RegisterOutcome as ProcessOutcome;
use contract::CommandId as ProcessCommandId;
use serde_json::Value as ProcessValue;
use std::collections::BTreeSet as ProcessFoldSet;

/// A process package directory holding `extra` merged over the base
/// manifest for `fiber.test/proc`.
fn process_dir(extra: serde_json::Value) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-process-fold");
    let mut manifest = json!({
        "name": "fiber.test/proc",
        "version": "v1.2.3",
        "fiber": "0.1.0",
        "api": 1,
        "process": {"program": "proc", "exit_timeout_ms": 1000},
    });
    for (key, value) in extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    fs::write(dir.path().join("extension.json"), manifest.to_string()).unwrap();
    dir
}

/// Loads the process package in `dir`.
fn process_package(dir: &fakes::TempDir) -> Arc<ProcessPackage> {
    ProcessPackage::load(ProcessPackageOptions {
        dir: dir.path().to_path_buf(),
        clock: FakeClock::new(),
    })
    .unwrap()
}

/// Answers one registration call through the package.
fn process_register(
    package: &ProcessPackage,
    id: &str,
    call: &str,
    args: ProcessValue,
) -> ProcessOutcome {
    package.register(
        ProcessCommandId(id.to_owned()),
        call,
        args.as_array().unwrap(),
    )
}

/// A valid `fiber.tool` spec with `run`, as JSON.
fn process_tool_spec() -> ProcessValue {
    json!({
        "description": "Does the thing.",
        "input_schema": {"type": "object"},
        "effects": {"effects": [], "reversible": true},
        "timeout": 5000,
        "run": true,
    })
}

/// A valid `fiber.hook` spec with `run`, as JSON.
fn process_hook_spec() -> ProcessValue {
    json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "blocking",
        "run": true,
    })
}

/// A valid `fiber.watch` spec with `run`, as JSON.
fn process_watcher_spec() -> ProcessValue {
    json!({"timeout": 5000, "run": true})
}

/// A valid `fiber.command` spec with `run`, as JSON.
fn process_command_spec() -> ProcessValue {
    json!({"timeout": 5000, "run": true, "description": "Runs it."})
}

/// A valid `fiber.search_backend` spec with `run`, as JSON.
fn process_search_spec() -> ProcessValue {
    json!({"timeout": 5000, "run": true})
}

/// Registers one of every session-facing family through the package.
fn register_every_family(package: &ProcessPackage) {
    for (id, call, args) in [
        ("c1", "fiber.tool", json!(["t", process_tool_spec()])),
        (
            "c2",
            "fiber.hook",
            json!(["turn_start", process_hook_spec()]),
        ),
        (
            "c3",
            "fiber.hook",
            json!(["after_tool", process_hook_spec()]),
        ),
        (
            "c4",
            "fiber.watch",
            json!([["notice"], process_watcher_spec()]),
        ),
        (
            "c5",
            "fiber.command",
            json!(["cmd", process_command_spec()]),
        ),
        (
            "c6",
            "fiber.search_backend",
            json!(["web", process_search_spec()]),
        ),
    ] {
        let outcome = process_register(package, id, call, args);
        assert!(
            matches!(outcome, ProcessOutcome::Accepted),
            "{call}: {outcome:?}"
        );
    }
}

#[test]
fn process_watcher_uses_the_session_clock() {
    let home = Home::new();
    let clock = Arc::new(FakeClock::new());
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(7));
    let expected = clock.now();
    let session = home.load_with_clock(&[], Arc::clone(&clock));
    let runtime = &session.runtime;
    runtime.register_owner("fiber.test/proc", None);
    let watcher = runtime.install_process_handler(
        "fiber.test/proc",
        ProcessCommandId("register-watch".to_owned()),
        "run",
        Arc::new(|_args, _scope, _cancel| Ok(Some(json!(null)))),
    );
    let offered = runtime
        .push_watcher(&watcher, json!({"kind": "notice"}), None)
        .expect("the registered process watcher answers");
    assert_eq!(offered.1, expected);
}

#[test]
fn process_check_hook_refuses_the_tool() {
    let dir = process_dir(json!({}));
    let mut session = SessionExtensions::default();
    let package = ProcessPackage::load_with_runtime(
        ProcessPackageOptions {
            dir: dir.path().to_path_buf(),
            clock: FakeClock::new(),
        },
        session.runtime.clone(),
    )
    .unwrap();
    let mut spec = process_hook_spec();
    spec["phase"] = json!("check");
    let outcome = process_register(&package, "c1", "fiber.hook", json!(["before_tool", spec]));
    assert!(matches!(outcome, ProcessOutcome::Accepted), "{outcome:?}");
    package.registry().unwrap().set_handler(
        &ProcessCommandId("c1".to_owned()),
        "run",
        Arc::new(|_args, _scope, _cancel| Ok(Some(json!({"refuse": "process check refused"})))),
    );
    let mut chain = super::chain::Builder::default();
    assert!(session.admit_process(
        "fiber.test/proc",
        &package,
        &mut chain,
        &ProcessFoldSet::new()
    ));
    let home = Home::new();
    chain.fill(&home.config(&[]), &mut session);
    let arguments = Map::new();
    let declared = DeclaredEffects {
        effects: Vec::new(),
        reversible: true,
        paths: None,
        confined: false,
    };
    let answer = session.check_tool(&BeforeToolCall {
        tool: "write",
        arguments: &arguments,
        declared: &declared,
    });
    assert_eq!(
        answer.outcome,
        CheckOutcome::Refused(Refusal {
            extension: "fiber.test/proc".to_owned(),
            reason: "process check refused".to_owned(),
        })
    );
}

#[test]
fn process_tool_keep_uses_zero_for_an_omitted_or_null_side() {
    let cases = [
        (json!({"head": 7}), Bound { start: 7, end: 0 }),
        (json!({"head": 7, "tail": null}), Bound { start: 7, end: 0 }),
        (json!({"tail": 9}), Bound { start: 0, end: 9 }),
        (json!({"head": null, "tail": 9}), Bound { start: 0, end: 9 }),
    ];
    for (keep, expected) in cases {
        let dir = process_dir(json!({}));
        let mut session = SessionExtensions::default();
        let package = ProcessPackage::load_with_runtime(
            ProcessPackageOptions {
                dir: dir.path().to_path_buf(),
                clock: FakeClock::new(),
            },
            session.runtime.clone(),
        )
        .unwrap();
        let mut spec = process_tool_spec();
        spec["keep"] = keep;
        let outcome = process_register(&package, "c1", "fiber.tool", json!(["lookup", spec]));
        assert!(matches!(outcome, ProcessOutcome::Accepted), "{outcome:?}");
        let settled = ProcessFoldSet::from([("fiber.test/proc".to_owned(), "lookup".to_owned())]);
        let mut chain = super::chain::Builder::default();
        assert!(session.admit_process("fiber.test/proc", &package, &mut chain, &settled,));
        let tools = session.tools();
        let tool = tools
            .iter()
            .find(|(_, tool)| tool.definition().name == "lookup")
            .expect("the registered `lookup` tool folds");
        assert_eq!(tool.1.bound(), expected);
    }
}

#[test]
fn process_registrations_fold_into_the_session() {
    let dir = process_dir(json!({}));
    let package = process_package(&dir);
    register_every_family(&package);
    let mut session = SessionExtensions::default();
    let mut chain = super::chain::Builder::default();
    let settled: ProcessFoldSet<(String, String)> =
        ProcessFoldSet::from([("fiber.test/proc".to_owned(), "t".to_owned())]);
    assert!(
        session.admit_process("fiber.test/proc", &package, &mut chain, &settled),
        "a clean package folds"
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let tools = session.tools();
    // One `fiber.tool` call registers one tool (`docs/extensions.md`,
    // "Registering").
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].0, "fiber.test/proc");
    assert_eq!(tools[0].1.definition().name, "t");
    assert!(session.has_hooks());
    let home = Home::new();
    chain.fill(&home.config(&[]), &mut session);
    // One `fiber.hook` call registers one hook (`docs/extensions.md`,
    // "Registering"): one per point here.
    assert_eq!(session.lifecycle.turn_start.len(), 1);
    assert_eq!(session.after_tool.len(), 1);
    assert_eq!(
        session.after_tool[0].hook.command,
        ProcessCommandId("c3".to_owned())
    );
    let hook = &session.lifecycle.turn_start[0];
    assert!(hook.blocking);
    assert_eq!(hook.hook.command, ProcessCommandId("c2".to_owned()));
    assert_eq!(hook.hook.key, "run");
    // One `fiber.watch` call registers one watcher (`docs/extensions.md`,
    // "Registering").
    assert_eq!(session.watchers.len(), 1);
    assert_eq!(session.watchers[0].name, "fiber.test/proc");
    assert_eq!(session.watchers[0].kinds, vec!["notice".to_owned()]);
    assert_eq!(
        session.watchers[0].watcher.command,
        ProcessCommandId("c4".to_owned())
    );
}

#[test]
fn process_package_problems_become_session_notices() {
    let dir = process_dir(json!({}));
    let package = process_package(&dir);
    let refused = process_register(
        &package,
        "c1",
        "fiber.tool",
        json!(["t", {"description": "Does the thing.", "input_schema": {"type": "object"}, "effects": {"effects": [], "reversible": true}, "run": true}]),
    );
    assert!(
        matches!(refused, ProcessOutcome::Rejected { .. }),
        "{refused:?}"
    );
    let kept = process_register(
        &package,
        "c2",
        "fiber.hook",
        json!(["turn_end", process_hook_spec()]),
    );
    assert!(matches!(kept, ProcessOutcome::Accepted), "{kept:?}");
    // An accepted tool the settle dropped stays out of the session.
    let dropped = process_register(
        &package,
        "c3",
        "fiber.tool",
        json!(["u", process_tool_spec()]),
    );
    assert!(matches!(dropped, ProcessOutcome::Accepted), "{dropped:?}");
    let mut session = SessionExtensions::default();
    let mut chain = super::chain::Builder::default();
    assert!(
        session.admit_process(
            "fiber.test/proc",
            &package,
            &mut chain,
            &ProcessFoldSet::new()
        ),
        "a refusal keeps the package"
    );
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: "`t` tool not registered: missing `timeout`".to_owned(),
            extension: Some("fiber.test/proc".to_owned()),
        }],
    );
    assert!(session.tools().is_empty());
    assert!(session.has_hooks());
}

#[test]
fn process_package_failure_fails_the_session_fold() {
    let dir = process_dir(json!({}));
    let package = process_package(&dir);
    let fatal = process_register(
        &package,
        "c1",
        "fiber.provider",
        json!(["p", {"models": {"timeout": 1000, "run": true}}]),
    );
    assert!(
        matches!(fatal, ProcessOutcome::FailedPackage { .. }),
        "{fatal:?}"
    );
    let mut session = SessionExtensions::default();
    let mut chain = super::chain::Builder::default();
    assert!(
        !session.admit_process(
            "fiber.test/proc",
            &package,
            &mut chain,
            &ProcessFoldSet::new()
        ),
        "a failed package does not fold"
    );
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: "The extension `fiber.test/proc` registers the provider `p`, which its manifest does not declare under `providers`.".to_owned(),
            extension: Some("fiber.test/proc".to_owned()),
        }],
    );
    assert_eq!(session.rejected(), vec!["fiber.test/proc".to_owned()]);
    assert!(session.tools().is_empty());
    assert!(!session.has_hooks());
}

/// Installs `fiber.test/<short>` as a process package: a manifest with a
/// process program and no entry script, merged over `extra`.
fn install_process_with(home: &Home, short: &str, extra: serde_json::Value) {
    let src = home.root.path().join("src").join(short);
    fs::create_dir_all(&src).unwrap();
    let mut manifest = json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1, "process": {"program": "proc", "exit_timeout_ms": 1000}});
    for (key, value) in extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
    plan(
        &home.home(),
        &Request::Path(src),
        "0.1.0",
        &Origin::github(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

#[test]
fn process_package_loads_without_starting_a_vm() {
    let home = Home::new();
    install_process_with(&home, "proc", json!({}));
    let session = home.load(&[]);
    assert!(
        session
            .loaded_names()
            .contains(&"fiber.test/proc".to_owned()),
        "{:?}",
        session.loaded_names()
    );
    assert!(session.tools().is_empty());
    assert!(session.commands().is_empty());
    assert!(session.lua_providers().is_empty());
    assert!(session.lua.is_empty(), "a process package starts no VM");
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| notice.extension.as_deref() != Some("fiber.test/proc")),
        "{:?}",
        session.notices()
    );
}

#[test]
fn process_package_with_undeclared_provider_file_fails_the_load() {
    let home = Home::new();
    install_process_with(&home, "proc", json!({}));
    let installed = home.home().join("extensions").join("fiber.test-proc");
    fs::create_dir_all(installed.join("providers")).unwrap();
    fs::write(installed.join("providers").join("q.json"), "{}").unwrap();
    let session = home.load(&[]);
    assert!(
        !session
            .loaded_names()
            .contains(&"fiber.test/proc".to_owned()),
        "{:?}",
        session.loaded_names()
    );
    assert!(
        session.notices().iter().any(|notice| {
            notice.code == ErrorCode::ExtensionFailed
                && notice.extension.as_deref() == Some("fiber.test/proc")
                && notice.message.contains("registers the provider `q`")
        }),
        "{:?}",
        session.notices()
    );
}

#[test]
fn process_commands_and_search_join_session_sources() {
    let dir = process_dir(json!({}));
    let package = process_package(&dir);
    for (id, call, args) in [
        (
            "c5",
            "fiber.command",
            json!(["cmd", process_command_spec()]),
        ),
        (
            "c6",
            "fiber.search_backend",
            json!(["web", process_search_spec()]),
        ),
    ] {
        let outcome = process_register(&package, id, call, args);
        assert!(
            matches!(outcome, ProcessOutcome::Accepted),
            "{call}: {outcome:?}"
        );
    }
    let runtime = crate::runtime::Runtime::new(FakeClock::new());
    let started = super::Started {
        name: "fiber.test/proc".to_owned(),
        version: "v1.2.3".to_owned(),
        slug: "fiber.test-proc".to_owned(),
        dir: dir.path().to_path_buf(),
        manifest: package.manifest().clone(),
        prompt: None,
        extension: None,
        package: Some(Arc::clone(&package)),
        declared: None,
        commands: Vec::new(),
        tools: Vec::new(),
        backends: Vec::new(),
        providers: Vec::new(),
        lazy: None,
    };
    let meta = started.process_meta().unwrap();
    assert_eq!(meta.extension, "fiber.test/proc");
    assert_eq!(
        meta.commands,
        vec![("cmd".to_owned(), "Runs it.".to_owned())]
    );
    let source = started.process_command_source(&runtime).unwrap();
    assert_eq!(source.extension, "fiber.test/proc");
    let command = match &source.runner {
        crate::commands::CommandRunner::Process(runner) => {
            runner.commands.get("cmd").unwrap().command.clone()
        }
        crate::commands::CommandRunner::Lua(_) => {
            panic!("a process command runs through its runtime reference")
        }
    };
    assert_eq!(command, ProcessCommandId("c5".to_owned()));
    let home = Home::new();
    let commands = crate::commands::SessionCommands::build(&[source], &home.config(&[]));
    let listed: Vec<String> = commands
        .list()
        .iter()
        .map(|info| info.name.clone())
        .collect();
    assert_eq!(listed, vec!["cmd".to_owned()]);
    let backends = started.process_search_sources(&runtime);
    // One `fiber.search_backend` call registers one backend
    // (`docs/extensions.md`, "Registering").
    assert_eq!(backends.len(), 1);
    assert_eq!(backends[0].name, "web");
}

/// A process package whose manifest declares a provider, with no provider
/// file and no entry script, loads without starting a VM: its provider
/// declarations wait for native registrations, as data providers do.
#[test]
fn process_package_with_declared_providers_loads_without_a_vm() {
    let home = Home::new();
    install_process_with(&home, "proc", json!({"providers": {"p": []}}));
    let session = home.load(&[]);
    assert!(
        session
            .loaded_names()
            .contains(&"fiber.test/proc".to_owned()),
        "{:?}",
        session.loaded_names()
    );
    assert!(session.lua.is_empty(), "a process package starts no VM");
    assert!(session.lua_providers().is_empty());
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| notice.extension.as_deref() != Some("fiber.test/proc")),
        "{:?}",
        session.notices()
    );
}

/// Registrations list hooks in hook-point table order and run order, then
/// watchers in registration order, with `entry` only for declared
/// configuration rows and `phase` only when declared. An ordinary
/// registration that supplies `entry` keeps its hook but loses the key, so
/// flipping the extension check would leak it and flipping the explicit
/// flag would list an undeclared phase. The per-point run order follows
/// `docs/extensions.md`, "When several hooks share a point"; the
/// hooks-first list order and the `entry`/`phase` omission rules follow
/// `docs/events.md`, "`command_accepted`".
#[test]
fn registrations_list_hooks_in_point_and_run_order_then_watchers() {
    use contract::events::HookInfo;
    let home = Home::new();
    home.install(
        "a",
        Some(
            "fiber.hook(\"after_tool\", { timeout = 1, on_failure = \"non-blocking\", run = function() end })\n\
             fiber.hook(\"before_tool\", { phase = \"sanitize\", timeout = 1, on_failure = \"blocking\", run = function() end })\n\
             fiber.hook(\"after_tool\", { timeout = 1, on_failure = \"blocking\", entry = \"evil\", run = function() end })\n\
             fiber.watch({ \"turn_completed\" }, { timeout = 1, run = function() end })",
        ),
        &[],
    );
    home.install(
        "b",
        Some(
            "fiber.hook(\"after_tool\", { timeout = 1, on_failure = \"blocking\", run = function() end })\n\
             fiber.watch({ \"turn_completed\", \"turn_started\" }, { timeout = 1, run = function() end })",
        ),
        &[],
    );
    install_hooks(&home, include_str!("../../../extensions/hooks/init.lua"));
    let entries = r#"{"audit":{"point":"after_tool","command":"true","timeout":1000,"on_failure":"blocking","phase":"sanitize"}}"#;
    let session = load_with_hooks(&home, entries);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    let rows = session.registrations();
    let qualified = config::full_name("hooks");
    for row in &rows {
        match row {
            HookInfo::Hook {
                extension,
                entry: Some(_),
                ..
            }
            | HookInfo::Watcher {
                extension,
                entry: Some(_),
                ..
            } => {
                assert_eq!(
                    extension, "hooks",
                    "a declared row uses `hooks`, not its qualified name: {row:?}"
                );
                assert_ne!(
                    extension, &qualified,
                    "a declared row exposes its qualified name: {row:?}"
                );
            }
            HookInfo::Hook { .. } | HookInfo::Watcher { .. } => {}
        }
    }
    let first_watcher = rows
        .iter()
        .position(|row| matches!(row, HookInfo::Watcher { .. }))
        .expect("a watcher registers");
    assert!(first_watcher >= 4, "{rows:?}");
    for row in &rows[..first_watcher] {
        assert!(matches!(row, HookInfo::Hook { .. }), "{row:?}");
    }
    let points: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            HookInfo::Hook { point, .. } => Some(point.as_str()),
            HookInfo::Watcher { .. } => None,
        })
        .collect();
    let table = [
        "session_start",
        "before_message",
        "turn_start",
        "before_tool",
        "before_model_call",
        "after_tool",
        "turn_end",
        "before_handoff",
    ];
    let mut last = 0;
    for point in &points {
        let at = table.iter().position(|name| name == point).unwrap();
        assert!(at >= last, "point order breaks at {point}: {points:?}");
        last = at;
    }
    let declared = rows
        .iter()
        .find_map(|row| match row {
            HookInfo::Hook {
                extension,
                entry,
                point,
                on_failure,
                phase,
            } if extension == "hooks" && point == "after_tool" => {
                Some((entry, point, on_failure, phase))
            }
            HookInfo::Hook { .. } | HookInfo::Watcher { .. } => None,
        })
        .expect("a declared hook registers");
    assert_eq!(declared.0.as_deref(), Some("audit"));
    assert_eq!(declared.1, "after_tool");
    assert_eq!(declared.2, "blocking");
    assert_eq!(declared.3.as_deref(), Some("sanitize"));
    for row in &rows {
        match row {
            HookInfo::Hook {
                extension, entry, ..
            } if extension != "hooks" => {
                assert_eq!(entry, &None, "ordinary hook carries no entry: {row:?}");
            }
            HookInfo::Watcher {
                extension, entry, ..
            } if extension != "hooks" => {
                assert_eq!(entry, &None, "ordinary watcher carries no entry: {row:?}");
            }
            HookInfo::Hook { .. } | HookInfo::Watcher { .. } => {}
        }
    }
    let watchers: Vec<(&str, Vec<String>)> = rows
        .iter()
        .filter_map(|row| match row {
            HookInfo::Watcher {
                extension, kinds, ..
            } => Some((extension.as_str(), kinds.clone())),
            HookInfo::Hook { .. } => None,
        })
        .collect();
    assert_eq!(watchers.len(), 2, "{rows:?}");
    assert_eq!(watchers[0].0, "fiber.test/a");
    assert_eq!(watchers[0].1, ["turn_completed"]);
    assert_eq!(watchers[1].0, "fiber.test/b");
    // The door answers through the trait forward, so it serves the same
    // rows: replacing the forward with an empty list would empty the
    // `hooks` command.
    let seam: std::sync::Arc<dyn contract::hook::Hooks> = session.clone();
    assert_eq!(seam.registrations(), rows, "{rows:?}");
}

/// Starting a process extension's provider fails without starting a VM:
/// nothing registered the provider, and no Lua runs it.
#[test]
fn start_provider_on_a_process_extension_starts_no_vm() {
    use support::lock as process_lock;
    let home = Home::new();
    install_process_with(&home, "proc", json!({"providers": {"p": []}}));
    let session = home.load(&[]);
    let config = home.config(&[]);
    let caller = Arc::clone(&session);
    let result = bounded(move || {
        caller.start_provider(
            &config,
            FakeClock::new(),
            Arc::new(FakeLock::new()),
            "fiber.test/proc",
            "p",
        )
    });
    let Err(crate::Error::Lua { extension, message }) = result else {
        panic!("an unregistered process provider fails");
    };
    assert_eq!(extension, "fiber.test/proc");
    assert!(message.contains("registers no provider `p`"), "{message}");
    assert!(
        !process_lock(&session.provider_vms).contains_key("fiber.test/proc"),
        "a failed process provider start keeps no VM"
    );
}

/// Recording reason `call` changes nothing: `call` never starts a
/// session, so the set keeps its last true reason instead of
/// recording a contradictory one.
#[test]
fn recording_call_reason_changes_nothing() {
    use contract::events::ExtensionReason;
    let session = SessionExtensions::default();
    assert_eq!(session.latest_reason(), ExtensionReason::New);
    session.record_start_reason(ExtensionReason::Call);
    assert_eq!(
        session.latest_reason(),
        ExtensionReason::New,
        "a call reason records nothing"
    );
}
