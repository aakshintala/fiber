//! The pure judgment (`docs/permissions.md`, "The order a call is judged
//! in" and "What a rule matches").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::PathBuf;

use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::events::{DecidedBy, Decision, Grant, RuleOffer, RuleScope};
use contract::rules::{Rule, RuleDecision, RulesError, StandingRules};
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::Effects;

use super::{Verdict, answer, credential_why, judge, matches};

fn declared(effects: Vec<Effect>, paths: Option<Vec<&str>>) -> DeclaredEffects {
    DeclaredEffects {
        effects,
        reversible: false,
        paths: paths.map(|paths| paths.iter().map(|p| (*p).to_owned()).collect()),
        confined: false,
    }
}

fn call(effects: Vec<Effect>, paths: Option<Vec<&str>>, subject: Option<&str>) -> Effects {
    Effects {
        declared: declared(effects, paths),
        subject: subject.map(str::to_owned),
        prefix: None,
        always_reviewed: false,
        review_when_confined: false,
        hosts: None,
    }
}

fn reviewed(effects: Effects) -> Effects {
    Effects {
        always_reviewed: true,
        review_when_confined: false,
        ..effects
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

/// A workspace, a Fiber home and a credentials directory that all exist.
fn dirs() -> (fakes::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = fakes::TempDir::new("fiber-permission");
    let workspace = root.path().join("ws");
    let home = root.path().join("home");
    let credentials = home.join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    // Canonicalised, as the loop holds them: the temporary directory may sit
    // under a symlink.
    let workspace = workspace.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();
    let credentials = credentials.canonicalize().unwrap();
    (root, workspace, home, credentials)
}

#[test]
fn inner_calls_take_only_the_credential_and_standing_deny_steps() {
    let (_root, workspace, _home, credentials) = dirs();
    let secret = credentials.join("token").display().to_string();
    let credential = call(vec![Effect::Reads], Some(vec![&secret]), Some("x"));
    assert!(matches!(
        super::inner_verdict(
            "inner",
            &credential,
            &empty_rules(),
            &workspace,
            &credentials,
            &[]
        ),
        Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            reason: "credentials",
            ..
        }
    ));
    let execute = reviewed(call(vec![Effect::Executes], None, Some("x")));
    assert!(matches!(
        super::inner_verdict(
            "inner",
            &execute,
            &rules(vec![rule(RuleDecision::Deny, "inner", "x")], Vec::new()),
            &workspace,
            &credentials,
            &[]
        ),
        Verdict::Deny {
            by: DecidedBy::StandingRule,
            reason: "standing_rule",
            ..
        }
    ));
    for standing in [
        empty_rules(),
        rules(vec![rule(RuleDecision::Ask, "inner", "x")], Vec::new()),
    ] {
        assert!(matches!(
            super::inner_verdict("inner", &execute, &standing, &workspace, &credentials, &[]),
            Verdict::Allow(None)
        ));
    }
    assert!(matches!(
        super::inner_verdict(
            "inner",
            &execute,
            &Err(RulesError("unreadable".into())),
            &workspace,
            &credentials,
            &[]
        ),
        Verdict::Deny {
            reason: "rules_unreadable",
            ..
        }
    ));
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
    let (_root, workspace, home, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let spelled = secret.display().to_string();
    let effects = call(vec![Effect::Reads], Some(vec![spelled.as_str()]), Some(""));
    // A standing allow matches the same call.
    let standing = rules(vec![rule(RuleDecision::Allow, "read", "")], Vec::new());
    let verdict = judge(
        "read",
        &effects,
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Deny { .. }));
}

#[test]
fn a_standing_deny_beats_an_allow_a_grant_and_a_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
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
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
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
    let (_root, workspace, home, credentials) = dirs();
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
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Ask(_)));
}

#[test]
fn a_fast_path_beats_a_grant_and_an_allow() {
    let (_root, workspace, home, credentials) = dirs();
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
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Allow(None)));
}

/// A machine data directory holding one Markdown file, for the ordering
/// tests: the write below would take the fast path, so any other verdict
/// proves an earlier step judged it.
fn data_markdown(home: &std::path::Path) -> String {
    std::fs::create_dir_all(home.join("data/n")).unwrap();
    let kept = home.join("data/n/x.md");
    std::fs::write(&kept, "kept\n").unwrap();
    kept.display().to_string()
}

#[test]
fn a_markdown_write_in_the_memory_data_directories_takes_the_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    for dir in ["data/memory", "projects/k/data/memory"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
        let page = home.join(dir).join("x.md").display().to_string();
        let effects = call(vec![Effect::Writes], Some(vec![page.as_str()]), Some(""));
        let verdict = judge(
            "write",
            &effects,
            &rules(Vec::new(), Vec::new()),
            &[],
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &[],
            &home,
            &credentials,
            &[],
        );
        assert!(matches!(verdict, Verdict::Allow(None)), "{dir}");
    }
}

#[test]
fn a_standing_deny_beats_the_data_directory_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    let kept = data_markdown(&home);
    let effects = call(vec![Effect::Writes], Some(vec![kept.as_str()]), Some(""));
    let standing = rules(vec![rule(RuleDecision::Deny, "write", "")], Vec::new());
    let verdict = judge(
        "write",
        &effects,
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
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
fn a_standing_ask_beats_the_data_directory_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    let kept = data_markdown(&home);
    let effects = call(vec![Effect::Writes], Some(vec![kept.as_str()]), Some(""));
    let standing = rules(vec![rule(RuleDecision::Ask, "write", "")], Vec::new());
    let verdict = judge(
        "write",
        &effects,
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Ask(_)));
}

#[test]
fn the_credential_deny_beats_the_data_directory_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    let kept = data_markdown(&home);
    let secret = credentials.join("k");
    std::fs::write(&secret, "s3cret").unwrap();
    // A call that also writes a credential file: the Markdown path would
    // take the fast path on its own.
    let spelled = secret.display().to_string();
    let effects = call(
        vec![Effect::Writes],
        Some(vec![kept.as_str(), spelled.as_str()]),
        Some(""),
    );
    let verdict = judge(
        "write",
        &effects,
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            reason: "credentials",
            ..
        }
    ));
    // A Markdown link resolving into the credentials directory.
    #[cfg(unix)]
    {
        let token = credentials.join("c.md");
        std::fs::write(&token, "s3cret").unwrap();
        std::os::unix::fs::symlink(&token, home.join("data/n/c.md")).unwrap();
        let linked = home.join("data/n/c.md").display().to_string();
        let effects = call(vec![Effect::Writes], Some(vec![linked.as_str()]), Some(""));
        let verdict = judge(
            "write",
            &effects,
            &empty_rules(),
            &[],
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &[],
            &home,
            &credentials,
            &[],
        );
        assert!(matches!(
            verdict,
            Verdict::Deny {
                by: DecidedBy::CredentialDeny,
                reason: "credentials",
                ..
            }
        ));
    }
}

#[test]
fn a_data_directory_markdown_write_takes_the_fast_path_before_a_grant_and_an_allow() {
    let (_root, workspace, home, credentials) = dirs();
    let kept = data_markdown(&home);
    let effects = call(vec![Effect::Writes], Some(vec![kept.as_str()]), Some(""));
    // With no rule the write takes the fast path: no decision line.
    let verdict = judge(
        "write",
        &effects,
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Allow(None)));
    // A session grant and a standing allow do not change that.
    let standing = rules(vec![rule(RuleDecision::Allow, "write", "")], Vec::new());
    let grants = [Grant {
        tool: "write".into(),
        prefix: String::new(),
    }];
    let verdict = judge(
        "write",
        &effects,
        &standing,
        &grants,
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Allow(None)));
}

#[test]
fn a_session_grant_beats_a_standing_allow() {
    let (_root, workspace, home, credentials) = dirs();
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
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
}

#[test]
fn a_grant_added_by_an_answer_matches_the_next_call_judged() {
    let (_root, workspace, home, credentials) = dirs();
    let effects = call(vec![Effect::Executes], None, Some("npm test --watch"));
    let before = judge(
        "shell",
        &effects,
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
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
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        after,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
}

#[test]
fn a_grant_matches_only_its_tool_and_prefix() {
    let (_root, workspace, home, credentials) = dirs();
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
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &[],
            &home,
            &credentials,
            &[],
        );
        assert!(matches!(verdict, Verdict::Review));
    }
}

#[test]
fn no_match_on_an_executes_call_is_review() {
    let (_root, workspace, home, credentials) = dirs();
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, None),
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn a_call_no_rule_can_match_matches_no_deny() {
    let (_root, workspace, home, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Deny, "shell", "npm test")],
        Vec::new(),
    );
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, None),
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn at_the_same_step_the_project_rule_is_used() {
    let (_root, workspace, home, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Ask, "read", "global")],
        vec![rule(RuleDecision::Ask, "read", "project")],
    );
    let Verdict::Ask(asked) = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("project")),
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    ) else {
        panic!("a standing ask matches");
    };
    assert_eq!(asked.scope, RuleScope::Project);
    assert_eq!(asked.prefix, "project");
}

#[test]
fn a_global_deny_beats_a_project_allow() {
    let (_root, workspace, home, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Deny, "shell", "npm test")],
        vec![rule(RuleDecision::Allow, "shell", "npm test")],
    );
    let verdict = judge(
        "shell",
        &call(vec![Effect::Executes], None, Some("npm test")),
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
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
    let (root, workspace, home, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let deny = |paths: Option<Vec<String>>| {
        let effects = Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        };
        judge(
            "read",
            &effects,
            &empty_rules(),
            &[],
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &[],
            &home,
            &credentials,
            &[],
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

#[test]
fn a_path_that_contains_the_credentials_touches_them() {
    let (root, workspace, home, credentials) = dirs();
    let why = |path: &str| {
        let declared = declared(vec![Effect::Reads], Some(vec![path]));
        credential_why(&declared, &workspace, &credentials, &[])
    };
    let spelled = |path: &std::path::Path| path.display().to_string();
    // Fiber home, the root, and the workspace's parent spelled `..`: a
    // recursive read of any of them reads every stored key.
    assert!(why(&spelled(&home)).is_some());
    assert!(why("/").is_some());
    assert!(why("..").is_some());
    // A symlink whose target is Fiber home.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&home, workspace.join("home-link")).unwrap();
        assert!(why("home-link").is_some());
    }
    // Containment is by whole components: a sibling sharing a string
    // prefix, and a directory in Fiber home beside the credentials, do not
    // contain them.
    std::fs::create_dir_all(root.path().join("home-other")).unwrap();
    std::fs::create_dir_all(home.join("cred")).unwrap();
    std::fs::create_dir_all(home.join("data")).unwrap();
    assert!(why(&spelled(&root.path().join("home-other"))).is_none());
    assert!(why(&spelled(&home.join("cred"))).is_none());
    assert!(why(&spelled(&home.join("data"))).is_none());
}

#[test]
fn a_path_touching_a_configured_credential_file_is_refused() {
    let (root, workspace, _home, credentials) = dirs();
    // A configured `file` source outside the workspace and Fiber home.
    let keys = root.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let key = keys.join("openrouter");
    std::fs::write(&key, "sk-file-secret").unwrap();
    std::fs::write(keys.join("notes"), "nothing").unwrap();
    let files = [key.canonicalize().unwrap()];
    let why = |path: &str, files: &[PathBuf]| {
        let declared = declared(vec![Effect::Reads], Some(vec![path]));
        credential_why(&declared, &workspace, &credentials, files)
    };
    let spelled = |path: &std::path::Path| path.display().to_string();
    let file = Some("The call touches a configured credential file.".to_owned());
    // The file itself, and the directory holding it.
    assert_eq!(why(&spelled(&key), &files), file);
    assert_eq!(why(&spelled(&keys), &files), file);
    // A `..` spelling, from the workspace.
    assert_eq!(why("../keys/openrouter", &files), file);
    // A symlink to the file.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&key, workspace.join("key-link")).unwrap();
        assert_eq!(why("key-link", &files), file);
    }
    // A sibling file, and a sibling sharing a string prefix, are not it.
    assert_eq!(why(&spelled(&keys.join("notes")), &files), None);
    assert_eq!(why(&spelled(&keys.join("openrouter-old")), &files), None);
    // With no configured files, the file is not protected.
    assert_eq!(why(&spelled(&key), &[]), None);
    // The credentials directory keeps its own reason.
    assert_eq!(
        why(&spelled(&credentials), &files),
        Some("The call touches Fiber's credential directory.".to_owned())
    );
}

#[test]
fn a_read_of_fiber_home_is_denied_before_the_read_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    let spelled = home.display().to_string();
    let verdict = judge(
        "read",
        &call(vec![Effect::Reads], Some(vec![spelled.as_str()]), Some("")),
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            reason: "credentials",
            ..
        }
    ));
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
    let (_root, workspace, home, credentials) = dirs();
    let error: Result<StandingRules, RulesError> = Err(RulesError("/home/rules:2: bad".into()));
    let verdict = judge(
        "read",
        &call(vec![Effect::Reads], None, Some("")),
        &error,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
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
    let (_root, workspace, home, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let spelled = secret.display().to_string();
    let effects = call(vec![Effect::Reads], Some(vec![spelled.as_str()]), Some(""));
    let error: Result<StandingRules, RulesError> = Err(RulesError("/home/rules:2: bad".into()));
    let verdict = judge(
        "read",
        &effects,
        &error,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
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

#[test]
fn an_always_reviewed_call_skips_a_session_grant() {
    let (_root, workspace, home, credentials) = dirs();
    let grants = [Grant {
        tool: "shell".into(),
        prefix: "npm test".into(),
    }];
    let plain = Effects {
        subject: Some("npm test --watch".into()),
        prefix: Some("npm test".into()),
        ..call(vec![Effect::Executes], None, Some("npm test --watch"))
    };
    let verdict = judge(
        "shell",
        &plain,
        &empty_rules(),
        &grants,
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Allow(Some(DecidedBy::SessionGrant))
    ));
    let verdict = judge(
        "shell",
        &reviewed(plain),
        &empty_rules(),
        &grants,
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn an_always_reviewed_call_skips_a_standing_allow() {
    let (_root, workspace, home, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Allow, "shell", "npm test")],
        Vec::new(),
    );
    let plain = call(vec![Effect::Executes], None, Some("npm test --watch"));
    let verdict = judge(
        "shell",
        &plain,
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Allow(Some(DecidedBy::StandingRule))
    ));
    let verdict = judge(
        "shell",
        &reviewed(plain),
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn an_always_reviewed_markdown_write_skips_the_data_directory_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    let kept = data_markdown(&home);
    let plain = call(vec![Effect::Writes], Some(vec![kept.as_str()]), Some(""));
    let verdict = judge(
        "write",
        &plain,
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Allow(None)));
    let verdict = judge(
        "write",
        &reviewed(plain),
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn an_always_reviewed_read_skips_the_read_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    let plain = call(vec![Effect::Reads], None, Some(""));
    let verdict = judge(
        "read",
        &plain,
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Allow(None)));
    let verdict = judge(
        "read",
        &reviewed(plain),
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Review));
}

#[test]
fn an_always_reviewed_call_still_meets_the_credential_deny() {
    let (_root, workspace, home, credentials) = dirs();
    let secret = credentials.join("token");
    std::fs::write(&secret, "s3cret").unwrap();
    let spelled = secret.display().to_string();
    let plain = call(vec![Effect::Reads], Some(vec![spelled.as_str()]), Some(""));
    let verdict = judge(
        "read",
        &plain,
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            ..
        }
    ));
    let verdict = judge(
        "read",
        &reviewed(plain),
        &empty_rules(),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            ..
        }
    ));
}

#[test]
fn an_always_reviewed_call_still_meets_a_standing_deny() {
    let (_root, workspace, home, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Deny, "shell", "npm test")],
        Vec::new(),
    );
    let plain = call(vec![Effect::Executes], None, Some("npm test"));
    let verdict = judge(
        "shell",
        &plain,
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(
        verdict,
        Verdict::Deny {
            by: DecidedBy::StandingRule,
            ..
        }
    ));
    let verdict = judge(
        "shell",
        &reviewed(plain),
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
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
fn an_always_reviewed_call_still_meets_a_standing_ask() {
    let (_root, workspace, home, credentials) = dirs();
    let standing = rules(
        vec![rule(RuleDecision::Ask, "shell", "npm test")],
        Vec::new(),
    );
    let plain = call(vec![Effect::Executes], None, Some("npm test"));
    let verdict = judge(
        "shell",
        &plain,
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Ask(_)));
    let verdict = judge(
        "shell",
        &reviewed(plain),
        &standing,
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &workspace,
        &[],
        &home,
        &credentials,
        &[],
    );
    assert!(matches!(verdict, Verdict::Ask(_)));
}

#[test]
fn permission_known_network_fast_path_respects_declared_hosts_and_prior_steps() {
    use crate::known_hosts::KnownHosts;
    let mut known = KnownHosts::default();
    known.fold(&contract::events::Event::PermissionResolved(
        contract::events::PermissionResolved {
            hosts: Some(vec!["https://example.com/".into()]),
            ..crate::completion::resolved(None, Decision::Allow, DecidedBy::Person, None, None)
        },
    ));
    let (_root, workspace, home, credentials) = dirs();
    let judge_call = |effects: &Effects, standing: &Result<StandingRules, RulesError>| {
        judge(
            "lookup",
            effects,
            standing,
            &[],
            &known,
            &workspace,
            &[],
            &home,
            &credentials,
            &[],
        )
    };
    for hosts in [
        Some(vec![]),
        Some(vec!["https://EXAMPLE.com/a?q=1".into()]),
        Some(vec!["https://example.com/b?other=2".into()]),
    ] {
        let mut effects = call(vec![Effect::Network], None, Some("url"));
        effects.hosts = hosts;
        assert!(matches!(
            judge_call(&effects, &empty_rules()),
            Verdict::Allow(None)
        ));
        effects.always_reviewed = true;
        assert!(matches!(
            judge_call(&effects, &empty_rules()),
            Verdict::Review
        ));
        effects.always_reviewed = false;
        assert!(matches!(
            judge_call(
                &effects,
                &rules(vec![rule(RuleDecision::Deny, "lookup", "url")], vec![])
            ),
            Verdict::Deny {
                by: DecidedBy::StandingRule,
                reason: "standing_rule",
                ..
            }
        ));
        assert!(matches!(
            judge_call(
                &effects,
                &rules(vec![rule(RuleDecision::Ask, "lookup", "url")], vec![])
            ),
            Verdict::Ask(_)
        ));
    }
    for hosts in [
        None,
        Some(vec!["https://unknown.test/".into()]),
        Some(vec![
            "https://example.com/".into(),
            "https://unknown.test/".into(),
        ]),
        Some(vec!["bare.test".into()]),
    ] {
        let mut effects = call(vec![Effect::Network], None, Some("url"));
        effects.hosts = hosts;
        assert!(matches!(
            judge_call(&effects, &empty_rules()),
            Verdict::Review
        ));
    }
    for declared in [
        vec![Effect::Network, Effect::Reads],
        vec![Effect::Network, Effect::Writes],
        vec![Effect::Network, Effect::Executes],
    ] {
        let mut effects = call(declared, None, Some("url"));
        effects.hosts = Some(vec!["https://example.com/".into()]);
        assert!(matches!(
            judge_call(&effects, &empty_rules()),
            Verdict::Review
        ));
    }
}

#[test]
fn writes_in_an_added_folder_take_the_fast_path_with_per_root_exclusions() {
    let (root, workspace, home, credentials) = dirs();
    let shared = root.path().join("shared");
    std::fs::create_dir_all(shared.join("sub")).unwrap();
    let shared = shared.canonicalize().unwrap();
    let root = root.path().canonicalize().unwrap();
    let added = [shared.clone()];
    let writes = |paths: &[&str]| {
        judge(
            "write",
            &call(
                vec![Effect::Reads, Effect::Writes],
                Some(paths.to_vec()),
                Some(""),
            ),
            &empty_rules(),
            &[],
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &added,
            &home,
            &credentials,
            &[],
        )
    };
    // At the root, just below it and deeper: all fast.
    for inside in [
        shared.display().to_string(),
        shared.join("new.rs").display().to_string(),
        shared.join("sub/file.rs").display().to_string(),
    ] {
        assert!(
            matches!(writes(&[&inside]), Verdict::Allow(None)),
            "{inside}"
        );
    }
    // Beside the root, holding no credentials: reviewed.
    for outside in [
        root.join("sibling/x").display().to_string(),
        root.join("other/x").display().to_string(),
    ] {
        assert!(matches!(writes(&[&outside]), Verdict::Review), "{outside}");
    }
    // The root itself holds the credentials directory, so the credential
    // deny refuses it before any fast path.
    assert!(matches!(
        writes(&[&root.display().to_string()]),
        Verdict::Deny { .. }
    ));
    // The exclusions apply below the added root too.
    for excluded in [
        shared.join(".git/config").display().to_string(),
        shared.join("sub/.fiber/x").display().to_string(),
    ] {
        assert!(
            matches!(writes(&[&excluded]), Verdict::Review),
            "{excluded}"
        );
    }
    // The exclusions still apply below the workspace with roots present.
    let workspace_git = workspace.join(".git/config").display().to_string();
    assert!(matches!(writes(&[&workspace_git]), Verdict::Review));
}

#[test]
fn a_metadata_root_itself_grants_no_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    // The command rejects such a root outright; this pins the judgment
    // closed if one ever arrives another way
    // (`docs/permissions.md`, "Fast paths").
    let outer = _root.path().join("outer");
    std::fs::create_dir_all(outer.join(".git")).unwrap();
    let outer = outer.canonicalize().unwrap();
    let added = [outer.join(".git")];
    let writes = |paths: &[&str]| {
        judge(
            "write",
            &call(
                vec![Effect::Reads, Effect::Writes],
                Some(paths.to_vec()),
                Some(""),
            ),
            &empty_rules(),
            &[],
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &added,
            &home,
            &credentials,
            &[],
        )
    };
    // At the metadata root, just below it and deeper: all reviewed.
    for denied in [
        outer.join(".git").display().to_string(),
        outer.join(".git/hooks").display().to_string(),
        outer.join(".git/hooks/x").display().to_string(),
    ] {
        assert!(matches!(writes(&[&denied]), Verdict::Review), "{denied}");
    }
}

#[test]
fn a_root_nested_below_metadata_grants_no_fast_path() {
    let (_root, workspace, home, credentials) = dirs();
    // A root nested below a metadata directory grants nothing either
    // (`docs/permissions.md`, "Fast paths").
    let outer = _root.path().join("outer");
    std::fs::create_dir_all(outer.join(".git/hooks")).unwrap();
    let outer = outer.canonicalize().unwrap();
    let added = [outer.join(".git/hooks")];
    let writes = |paths: &[&str]| {
        judge(
            "write",
            &call(
                vec![Effect::Reads, Effect::Writes],
                Some(paths.to_vec()),
                Some(""),
            ),
            &empty_rules(),
            &[],
            &crate::known_hosts::KnownHosts::default(),
            &workspace,
            &added,
            &home,
            &credentials,
            &[],
        )
    };
    // At the nested root and below it: reviewed, never fast.
    for denied in [
        outer.join(".git/hooks").display().to_string(),
        outer.join(".git/hooks/x").display().to_string(),
    ] {
        assert!(matches!(writes(&[&denied]), Verdict::Review), "{denied}");
    }
}
