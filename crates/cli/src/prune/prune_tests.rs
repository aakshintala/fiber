#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::sync::Arc;

use contract::rules::{Rule, RuleDecision, Rules};

use super::*;

#[test]
fn prune_then_launch_migrates_rules_only_to_the_claiming_identity() {
    let home = fakes::TempDir::new("cli-prune-project-migration");
    let root = home.path().join("workspaces");
    let first = root.join("a-b");
    let second = root.join("a/b");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let first_identity = fs::canonicalize(&first).unwrap();
    let second_identity = fs::canonicalize(&second).unwrap();
    let legacy = support::project_key::legacy_project_slug(&first_identity);
    let legacy_rules = home.path().join("projects").join(&legacy).join("rules");
    fs::create_dir_all(legacy_rules.parent().unwrap()).unwrap();
    let rule = Rule {
        decision: RuleDecision::Allow,
        tool: "shell".to_owned(),
        prefix: "trusted".to_owned(),
        added: None,
        session_id: None,
    };
    fs::write(&legacy_rules, format!("{}\n", serde_json::to_string(&rule).unwrap())).unwrap();

    // Prune ensures the running identity's layout before project selection;
    // the next launch observes the same migrated project state.
    config::ensure_project_layout(home.path(), &first_identity).unwrap();
    config::ensure_project_layout(home.path(), &second_identity).unwrap();
    let first_rules = config::RulesFiles::new(
        home.path().to_path_buf(),
        config::ProjectKey::new(log::project_key(&first_identity)).unwrap(),
        Arc::new(fakes::clock::FakeClock::new()),
    );
    let second_rules = config::RulesFiles::new(
        home.path().to_path_buf(),
        config::ProjectKey::new(log::project_key(&second_identity)).unwrap(),
        Arc::new(fakes::clock::FakeClock::new()),
    );

    assert_eq!(first_rules.read().unwrap().project, [rule]);
    assert!(second_rules.read().unwrap().project.is_empty());
}
