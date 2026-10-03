//! The pure judgment (`docs/permissions.md`, "The order a call is judged
//! in" and "What a rule matches").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::PathBuf;

use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::events::{DecidedBy, Decision, Grant, RuleOffer, RuleScope};
use contract::rules::{Rule, RuleDecision, RulesError, StandingRules};
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::Effects;

use super::{Verdict, answer, judge, matches};

fn declared(effects: Vec<Effect>, paths: Option<Vec<&str>>) -> DeclaredEffects {
    DeclaredEffects {
        effects,
        reversible: false,
        paths: paths.map(|paths| paths.iter().map(|p| (*p).to_owned()).collect()),
    }
}

fn call(effects: Vec<Effect>, paths: Option<Vec<&str>>, subject: Option<&str>) -> Effects {
    Effects {
        declared: declared(effects, paths),
        subject: subject.map(str::to_owned),
        prefix: None,
    }
}

fn rule(decision: RuleDecision, tool: &str, prefix: &str) -> Rule {
    Rule {
        decision,
        tool: tool.into(),
        prefix: prefix.into(),
        added: None,
        session_id: None,
    }
}

fn rules(global: Vec<Rule>, project: Vec<Rule>) -> Result<StandingRules, RulesError> {
    Ok(StandingRules { global, project })
}

fn empty_rules() -> Result<StandingRules, RulesError> {
    rules(Vec::new(), Vec::new())
}

/// A workspace and a credentials directory that both exist.
fn dirs() -> (fakes::TempDir, PathBuf, PathBuf) {
    let root = fakes::TempDir::new("fiber-permission");
    let workspace = root.path().join("ws");
    let credentials = root.path().join("home/credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    // Canonicalised, as the loop holds them: the temporary directory may sit
    // under a symlink.
    let workspace = workspace.canonicalize().unwrap();
    let credentials = credentials.canonicalize().unwrap();
    (root, workspace, credentials)
}

#[test]
fn a_prefix_ending_in_slash_matches_every_subject_that_starts_with_it() {
    assert!(matches("src/", Some("src/main.rs")));
    assert!(matches("src/", Some("src/")));
    assert!(!matches("src/", Some("other/main.rs")));
    assert!(!matches("src/", Some("")));
    assert!(!matches("src/", None));
}

#[test]
fn any_other_prefix_matches_an_equal_subject_and_one_going_on_with_a_space() {
    assert!(matches("npm test", Some("npm test")));
    assert!(matches("npm test", Some("npm test --watch")));
    assert!(!matches("npm test", Some("npm testing")));
    assert!(!matches("npm test", Some("npm")));
    assert!(!matches("npm test", None));
}

#[test]
fn an_empty_subject_matches_only_an_empty_prefix() {
    assert!(matches("", Some("")));
    assert!(!matches("npm test", Some("")));
    assert!(!matches("src/", Some("")));
}

#[test]
fn the_credential_deny_runs_before_a_standing_allow() {
    let (_root, workspace, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let spelled = secret.display().to_string();
    let effects = call(vec![Effect::Reads], Some(vec![spelled.as_str()]), Some(""));
    // A standing allow matches the same call.
    let standing = rules(vec![rule(RuleDecision::Allow, "read", "")], Vec::new());
    let verdict = judge("read", &effects, &standing, &[], &workspace, &credentials);
    assert!(matches!(verdict, Verdict::Deny { .. }));
}

#[test]
fn a_standing_deny_beats_an_allow_a_grant_and_a_fast_path() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(
        vec![
            rule(RuleDecision::Deny, "read", ""),
            rule(RuleDecision::Allow, "read", ""),
        ],
        Vec::new(),
    );
    let grants = [Grant {
        tool: "read".into(),
        prefix: String::new(),
    }];
    // A reads call: the fast path, a grant and an allow all match.
    let verdict = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("")),
        &standing,
        &grants,
        &workspace,
        &credentials,
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::StandingRule,
            reason: "standing_rule",
            ..
        }
    ));
}

#[test]
fn a_standing_ask_beats_a_fast_path_and_an_allow() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(
        vec![
            rule(RuleDecision::Ask, "read", ""),
            rule(RuleDecision::Allow, "read", ""),
        ],
        Vec::new(),
    );
    let verdict = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("")),
        &standing,
        &[],
        &workspace,
        &credentials,
    );
    assert!(matches!(verdict, Verdict::Ask(_)));
}

#[test]
fn a_fast_path_beats_a_grant_and_an_allow() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(vec![rule(RuleDecision::Allow, "read", "")], Vec::new());
    let grants = [Grant {
        tool: "read".into(),
        prefix: String::new(),
    }];
    let verdict = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("")),
        &standing,
        &grants,
        &workspace,
        &credentials,
    );
    assert!(matches!(verdict, Verdict::Allow(None)));
}

#[test]
fn a_session_grant_beats_a_standing_allow() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Allow, "shell", "npm test")],
        Vec::new(),
    );
    let grants = [Grant {
        tool: "shell".into(),
        prefix: "npm test".into(),
    }];
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, Some("npm test --watch")),
        &standing,
        &grants,
        &workspace,
        &credentials,
    );
    assert!(matches!(
        verdict,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
}

#[test]
fn a_grant_added_by_an_answer_matches_the_next_call_judged() {
    let (_root, workspace, credentials) = dirs();
    let effects = call(vec![Effect::Executes], None, Some("npm test --watch"));
    let before = judge(
        "shell",
        &effects,
        &empty_rules(),
        &[],
        &workspace,
        &credentials,
    );
    assert!(matches!(before, Verdict::Review));
    // The grant is what applying the person's allow remembered, not a
    // literal: the answer check fixes the remembered prefix.
    let reply = ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: Some(Remember {
            scope: RememberScope::Session,
            prefix: "npm test".into(),
        }),
    };
    let remembered = answer(Some(&offer()), &reply).unwrap().remember.unwrap();
    let grants = [remembered.grant("shell")];
    let after = judge(
        "shell",
        &effects,
        &empty_rules(),
        &grants,
        &workspace,
        &credentials,
    );
    assert!(matches!(
        after,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
}

#[test]
fn a_grant_matches_only_its_tool_and_prefix() {
    let (_root, workspace, credentials) = dirs();
    let effects = call(vec![Effect::Executes], None, Some("npm test --watch"));
    // The prefix matches but the tool does not, and the other way round:
    // neither grants the call.
    for grant in [
        Grant {
            tool: "other".into(),
            prefix: "npm test".into(),
        },
        Grant {
            tool: "shell".into(),
            prefix: "npm run".into(),
        },
    ] {
        let verdict = judge(
            "shell",
            &effects,
            &empty_rules(),
            &[grant],
            &workspace,
            &credentials,
        );
        assert!(matches!(verdict, Verdict::Review));
    }
}

#[test]
fn no_match_on_an_executes_call_is_review() {
    let (_root, workspace, credentials) = dirs();
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, None),
        &empty_rules(),
        &[],
        &workspace,
        &credentials,
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn a_call_no_rule_can_match_matches_no_deny() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Deny, "shell", "npm test")],
        Vec::new(),
    );
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, None),
        &standing,
        &[],
        &workspace,
        &credentials,
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn at_the_same_step_the_project_rule_is_used() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Ask, "read", "global")],
        vec![rule(RuleDecision::Ask, "read", "project")],
    );
    let Verdict::Ask(asked) = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("project")),
        &standing,
        &[],
        &workspace,
        &credentials,
    ) else {
        panic!("a standing ask matches");
    };
    assert_eq!(asked.scope, RuleScope::Project);
    assert_eq!(asked.prefix, "project");
}

#[test]
fn a_global_deny_beats_a_project_allow() {
    let (_root, workspace, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Deny, "shell", "npm test")],
        vec![rule(RuleDecision::Allow, "shell", "npm test")],
    );
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, Some("npm test")),
        &standing,
        &[],
        &workspace,
        &credentials,
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::StandingRule,
            ..
        }
    ));
}

#[test]
fn credential_paths_match_inside_the_directory_and_fail_closed() {
    let (root, workspace, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let deny = |paths: Option<Vec<String>>| {
        let effects = Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths,
            },
            subject: Some(String::new()),
            prefix: None,
        };
        judge(
            "read",
            &effects,
            &empty_rules(),
            &[],
            &workspace,
            &credentials,
        )
    };
    let is_deny = |verdict: Verdict| {
        matches!(
            verdict,
            Verdict::Deny {
                by: DecidedBy::CredentialDeny,
                reason: "credentials",
                ..
            }
        )
    };
    // Absolute and relative spellings of a file inside.
    assert!(is_deny(deny(Some(vec![secret.display().to_string()]))));
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    let relative = pathdiff(&secret, &workspace);
    assert!(is_deny(deny(Some(vec![relative]))));
    // A symlink into the credentials.
    #[cfg(unix)]
    {
        let link = workspace.join("link");
        std::os::unix::fs::symlink(&credentials, &link).unwrap();
        assert!(is_deny(deny(Some(vec!["link/token".into()]))));
    }
    // A `..` spelling.
    let dotdot = format!("{}/../home/credentials/token", workspace.display());
    assert!(is_deny(deny(Some(vec![dotdot]))));
    // A path that does not resolve.
    assert!(is_deny(deny(Some(vec!["missing/../token".into()]))));
    // The directory itself.
    assert!(is_deny(deny(Some(vec![credentials.display().to_string()]))));
    // Anything else, and no paths, never matches.
    assert!(!is_deny(deny(Some(vec!["src/main.rs".into()]))));
    assert!(!is_deny(deny(Some(vec![
        root.path().join("elsewhere").display().to_string()
    ]))));
    assert!(!is_deny(deny(Some(Vec::new()))));
    assert!(!is_deny(deny(None)));
}

/// `target` relative to `base`, both existing, without leaving the tree.
fn pathdiff(target: &std::path::Path, base: &std::path::Path) -> String {
    let target = target.canonicalize().unwrap();
    let base = base.canonicalize().unwrap();
    let mut back = String::new();
    let mut at = base.as_path();
    while !target.starts_with(at) {
        back.push_str("../");
        at = at.parent().unwrap();
    }
    let rest = target.strip_prefix(at).unwrap().display().to_string();
    format!("{back}{rest}")
}

#[test]
fn an_unreadable_rules_file_denies_at_step_two() {
    let (_root, workspace, credentials) = dirs();
    let error: Result<StandingRules, RulesError> = Err(RulesError("/home/rules:2: bad".into()));
    let verdict = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("")),
        &error,
        &[],
        &workspace,
        &credentials,
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::StandingRule,
            reason: "rules_unreadable",
            ..
        }
    ));
}

#[test]
fn a_credential_path_is_denied_even_when_the_rules_are_unreadable() {
    let (_root, workspace, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let spelled = secret.display().to_string();
    let effects = call(vec![Effect::Reads], Some(vec![spelled.as_str()]), Some(""));
    let error: Result<StandingRules, RulesError> = Err(RulesError("/home/rules:2: bad".into()));
    let verdict = judge("read", &effects, &error, &[], &workspace, &credentials);
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            reason: "credentials",
            ..
        }
    ));
}

fn approval(decision: Decision) -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision,
        feedback: None,
        remember: None,
    }
}

fn offer() -> RuleOffer {
    RuleOffer {
        subject: "npm test --watch".into(),
        prefix: "npm test".into(),
    }
}

#[test]
fn a_plain_allow_and_a_deny_with_feedback_fit() {
    let allow = answer(Some(&offer()), &approval(Decision::Allow)).unwrap();
    assert_eq!(allow.decision, Decision::Allow);
    assert!(allow.remember.is_none());
    let mut denied = approval(Decision::Deny);
    let ReplyAnswer::Approval { feedback, .. } = &mut denied else {
        unreachable!();
    };
    *feedback = Some("not on main".into());
    let denied = answer(Some(&offer()), &denied).unwrap();
    assert_eq!(denied.decision, Decision::Deny);
    assert_eq!(denied.feedback.as_deref(), Some("not on main"));
}

#[test]
fn remembering_the_subject_or_the_prefix_fits() {
    for prefix in ["npm test --watch", "npm test"] {
        let reply = ReplyAnswer::Approval {
            decision: Decision::Allow,
            feedback: None,
            remember: Some(contract::commands::Remember {
                scope: RememberScope::Session,
                prefix: prefix.into(),
            }),
        };
        let answered = answer(Some(&offer()), &reply).unwrap();
        assert_eq!(answered.remember.unwrap().prefix, prefix);
    }
    let reply = ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: Some(contract::commands::Remember {
            scope: RememberScope::Project,
            prefix: "npm test".into(),
        }),
    };
    let answered = answer(Some(&offer()), &reply).unwrap();
    assert_eq!(answered.remember.unwrap().scope, RememberScope::Project);
}

#[test]
fn an_ill_fitting_reply_does_not_fit() {
    let remembered = || contract::commands::Remember {
        scope: RememberScope::Session,
        prefix: "npm test".into(),
    };
    // Another kind's answer keys.
    assert!(answer(Some(&offer()), &ReplyAnswer::Confirmed { confirmed: true }).is_none());
    // Feedback with allow.
    let reply = ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: Some("fine".into()),
        remember: None,
    };
    assert!(answer(Some(&offer()), &reply).is_none());
    // Remember on a request with no rule.
    let reply = ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: Some(remembered()),
    };
    assert!(answer(None, &reply).is_none());
    // A prefix the request did not offer.
    let reply = ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: Some(contract::commands::Remember {
            scope: RememberScope::Session,
            prefix: "npm".into(),
        }),
    };
    assert!(answer(Some(&offer()), &reply).is_none());
    // Remember with deny.
    let reply = ReplyAnswer::Approval {
        decision: Decision::Deny,
        feedback: None,
        remember: Some(remembered()),
    };
    assert!(answer(Some(&offer()), &reply).is_none());
}
