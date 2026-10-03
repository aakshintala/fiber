//! The tool seam's registration, fast paths, and what a person's allow
//! remembers: session grants, project rules, and a rule that cannot be saved.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::PathBuf;

use std::sync::{Arc, Mutex, mpsc};

use contract::RequestId;
use contract::SessionId;
use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::events::{DecidedBy, Decision, Grant, RuleOffer, ToolReplaced};
use contract::inbox::Delivery;
use contract::provider::ToolDefinition;
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::{Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use super::{fast_path, register};
use crate::permission::{Verdict, judge};
use crate::{Loop, Model};
use log::Log;

/// A tool known only by its name and description.
struct Named(&'static str, &'static str);

impl Tool for Named {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.0.to_owned(),
            description: self.1.to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        unreachable!("registration never asks for effects")
    }

    fn run(&self, _: &Map<String, Value>) -> Output {
        unreachable!("registration never runs a tool")
    }
}

fn by(registered_by: &str, tool: Named) -> (String, Arc<dyn Tool>) {
    (registered_by.to_owned(), Arc::new(tool))
}

#[test]
fn each_tool_is_registered_with_who_registered_it() {
    let (tools, replaced) = register(vec![
        by("builtin", Named("read", "Reads.")),
        by("github", Named("mcp__github__search", "Searches.")),
    ]);
    let owners: Vec<(&str, &str)> = tools
        .iter()
        .map(|(name, (by, _, _))| (name.as_str(), by.as_str()))
        .collect();
    assert_eq!(
        owners,
        [("mcp__github__search", "github"), ("read", "builtin")]
    );
    assert!(replaced.is_empty());
}

#[test]
fn a_later_tool_of_a_taken_name_replaces_the_earlier_and_is_recorded() {
    let (tools, replaced) = register(vec![
        by("builtin", Named("read", "Reads.")),
        by("lint", Named("read", "Reads, linted.")),
        by("builtin", Named("write", "Writes.")),
        by("audit", Named("read", "Reads, audited.")),
    ]);
    let (owner, _, definition) = &tools["read"];
    assert_eq!(owner, "audit");
    assert_eq!(definition.description, "Reads, audited.");
    assert_eq!(tools.len(), 2);
    let step = |from: &str, to: &str| ToolReplaced {
        name: "read".to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
    };
    assert_eq!(replaced, [step("builtin", "lint"), step("lint", "audit")]);
}

/// A fresh workspace, symlinks resolved, holding `real/` and a link `out`
/// to a directory outside it.
fn workspace() -> (fakes::TempDir, PathBuf) {
    let root = fakes::TempDir::new("fiber-calls");
    let canon = root.path().canonicalize().unwrap();
    let workspace = canon.join("ws");
    std::fs::create_dir_all(workspace.join("real")).unwrap();
    std::fs::create_dir_all(canon.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(canon.join("elsewhere"), workspace.join("out")).unwrap();
    (root, workspace)
}

fn declared(effects: &[Effect], paths: Option<&[&str]>) -> DeclaredEffects {
    DeclaredEffects {
        effects: effects.to_vec(),
        reversible: false,
        paths: paths.map(|p| p.iter().map(|s| (*s).to_owned()).collect()),
    }
}

#[test]
fn reads_and_no_effect_take_the_fast_path_wherever_they_point() {
    let (_root, ws) = workspace();
    assert!(fast_path(&declared(&[], None), &ws));
    assert!(fast_path(
        &declared(&[Effect::Reads], Some(&["/etc/hosts"])),
        &ws
    ));
}

#[test]
fn a_write_takes_the_fast_path_only_inside_the_workspace_and_outside_git_and_fiber() {
    let (_root, ws) = workspace();
    let writes = |paths: &[&str]| {
        fast_path(
            &declared(&[Effect::Reads, Effect::Writes], Some(paths)),
            &ws,
        )
    };
    let inside = ws.join("real/new/file.rs").display().to_string();
    assert!(writes(&["real/a.rs", "new.rs", &inside]));
    for outside in [
        "/tmp/x",
        "../x",
        "real/../../x",
        // Through a link that leaves the workspace.
        "out/x",
        "out/../x",
        // `..` past a directory that does not exist yet.
        "new/../real/x",
        ".git/config",
        "real/.git/hooks/pre-commit",
        ".fiber/config.json",
    ] {
        assert!(!writes(&[outside]), "{outside}");
    }
    assert!(!writes(&["real/a.rs", ".git/x"]));
    // No paths, or none declared, is reviewed.
    assert!(!writes(&[]));
    assert!(!fast_path(&declared(&[Effect::Writes], None), &ws));
}

#[test]
fn executes_and_network_never_take_the_fast_path() {
    let (_root, ws) = workspace();
    for effect in [Effect::Executes, Effect::Network] {
        assert!(!fast_path(
            &declared(&[Effect::Reads, effect], Some(&["real/a"])),
            &ws
        ));
    }
}

/// Standing rules in memory that record what was remembered, and optionally
/// fail: what a project remember is checked against.
#[derive(Default)]
struct FakeRules {
    /// Every (`tool`, `prefix`, `session`) remembered, in order.
    remembered: Mutex<Vec<(String, String, SessionId)>>,
    /// What `remember` fails with, once set.
    fail: Mutex<Option<RulesError>>,
}

impl FakeRules {
    /// `remember` fails with `error` from now on.
    fn fail(&self, error: RulesError) {
        *self.fail.lock().unwrap() = Some(error);
    }
}

impl Rules for FakeRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, tool: &str, prefix: &str, session: &SessionId) -> Result<(), RulesError> {
        if let Some(error) = self.fail.lock().unwrap().clone() {
            return Err(error);
        }
        self.remembered
            .lock()
            .unwrap()
            .push((tool.into(), prefix.into(), session.clone()));
        Ok(())
    }
}

/// A loop on a fresh log with the fakes clock, holding `rules`: the session
/// is `s_test`. The returned directories keep the log's files alive.
fn start(rules: Arc<FakeRules>) -> (Loop, fakes::TempDir, PathBuf, PathBuf) {
    let home = fakes::TempDir::new("fiber-remember");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let log = Arc::new(
        Log::create(
            home.path(),
            SessionId("s_test".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    let (_inbox, rx) = mpsc::channel::<Delivery>();
    let rules: Arc<dyn Rules> = rules;
    let looped = Loop::start(
        log,
        Arc::new(fakes::ScriptedProvider::new(Vec::new())),
        Model {
            reference: "fake/model".into(),
            cost: None,
            subscription: false,
        },
        "You are terse.".into(),
        rx,
        Vec::new(),
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials: credentials.clone(),
            rules,
        },
    )
    .unwrap();
    (looped, home, workspace, credentials)
}

/// The rule a reviewer will offer (#294): the subject `npm test --watch`,
/// widened to `npm test`.
fn offer() -> RuleOffer {
    RuleOffer {
        subject: "npm test --watch".into(),
        prefix: "npm test".into(),
    }
}

/// An allow remembering `prefix` in `scope`.
fn allow(scope: RememberScope, prefix: &str) -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: Some(Remember {
            scope,
            prefix: prefix.into(),
        }),
    }
}

/// An `executes` call of `tool` with `subject`, declaring no paths.
fn judged(subject: &str) -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        },
        subject: Some(subject.into()),
        prefix: None,
    }
}

#[test]
fn a_session_remember_adds_a_grant_the_next_call_judged_matches() {
    let (mut looped, _home, workspace, credentials) = start(Arc::new(FakeRules::default()));
    let answered = looped
        .answered(
            "shell",
            Some(&offer()),
            &allow(RememberScope::Session, "npm test"),
        )
        .unwrap();
    assert_eq!(
        answered.grant,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        })
    );
    assert_eq!(answered.rule, None);
    assert_eq!(answered.reason, None);
    // The next call judged in the same step matches the added grant.
    let standing: Result<StandingRules, RulesError> = Ok(StandingRules::default());
    let verdict = judge(
        "shell",
        &judged("npm test --watch"),
        &standing,
        &looped.grants,
        &workspace,
        &credentials,
    );
    assert!(matches!(
        verdict,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
}

#[test]
fn a_project_remember_appends_a_rule_and_names_it_on_the_line() {
    let rules = Arc::new(FakeRules::default());
    let (mut looped, _home, _workspace, _credentials) = start(Arc::clone(&rules));
    let answered = looped
        .answered(
            "shell",
            Some(&offer()),
            &allow(RememberScope::Project, "npm test"),
        )
        .unwrap();
    assert_eq!(answered.grant, None);
    assert_eq!(
        answered.rule,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        })
    );
    assert_eq!(answered.reason, None);
    assert_eq!(
        *rules.remembered.lock().unwrap(),
        [(
            "shell".to_owned(),
            "npm test".to_owned(),
            SessionId("s_test".into())
        )]
    );
}

#[test]
fn a_failing_project_remember_still_allows_the_call_and_says_why() {
    let rules = Arc::new(FakeRules::default());
    rules.fail(RulesError("projects/k/rules:1: bad line".into()));
    let (mut looped, _home, _workspace, _credentials) = start(Arc::clone(&rules));
    let answered = looped
        .answered(
            "shell",
            Some(&offer()),
            &allow(RememberScope::Project, "npm test"),
        )
        .unwrap();
    assert_eq!(answered.grant, None);
    assert_eq!(answered.rule, None);
    let reason = answered.reason.unwrap();
    assert!(reason.contains("could not be saved"), "{reason}");
    assert!(reason.contains("projects/k/rules:1: bad line"), "{reason}");
    assert!(rules.remembered.lock().unwrap().is_empty());
}

#[test]
fn a_standing_ask_passes_no_offer_so_a_remember_does_not_fit() {
    let (mut looped, _home, _workspace, _credentials) = start(Arc::new(FakeRules::default()));
    assert!(
        looped
            .answered("shell", None, &allow(RememberScope::Session, "npm test"))
            .is_none()
    );
    assert!(looped.grants.is_empty());
}

#[test]
fn a_persons_allow_line_carries_what_was_remembered() {
    let remembered = |grant: Option<Grant>, rule: Option<Grant>, reason: Option<String>| {
        super::Answered {
            decision: Decision::Allow,
            feedback: None,
            grant,
            rule,
            reason,
        }
        .allow(RequestId("r_1".into()))
    };
    let grant = Grant {
        tool: "shell".into(),
        prefix: "npm test".into(),
    };
    // A remembered session grant rides `grant`.
    let line = remembered(Some(grant.clone()), None, None);
    assert_eq!(line.request_id, Some(RequestId("r_1".into())));
    assert_eq!(line.decision, Decision::Allow);
    assert_eq!(line.decided_by, DecidedBy::Person);
    assert_eq!(line.grant, Some(grant));
    assert_eq!(line.rule, None);
    assert_eq!(line.reason, None);
    // A remembered project rule rides `rule`.
    let line = remembered(
        None,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        }),
        None,
    );
    assert_eq!(line.grant, None);
    assert_eq!(
        line.rule,
        Some(Grant {
            tool: "shell".into(),
            prefix: "npm test".into(),
        })
    );
    assert_eq!(line.reason, None);
    // A rule that could not be saved rides `reason`, with no `rule`.
    let line = remembered(None, None, Some("The rule could not be saved.".into()));
    assert_eq!(line.grant, None);
    assert_eq!(line.rule, None);
    assert_eq!(line.reason, Some("The rule could not be saved.".into()));
}
