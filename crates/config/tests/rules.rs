//! Standing rules files (`docs/configuration.md`, "Standing rules").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use config::{ProjectKey, RulesFiles};
use contract::SessionId;
use contract::rules::Rules;
use fakes::TempDir;
use fakes::clock::FakeClock;

fn home() -> TempDir {
    TempDir::new("fiber-rules")
}

fn files(home: &TempDir, project: &str) -> RulesFiles {
    RulesFiles::new(
        home.path().to_path_buf(),
        ProjectKey::new(project).unwrap(),
        FakeClock::new(),
    )
}

fn write(path: &std::path::Path, text: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

#[test]
fn missing_files_read_as_empty() {
    let home = home();
    let rules = files(&home, "proj").read().unwrap();
    assert!(rules.global.is_empty());
    assert!(rules.project.is_empty());
}

#[test]
fn global_and_project_lines_read_into_their_scopes_in_file_order() {
    let home = home();
    write(
        &home.path().join("rules"),
        "{\"decision\":\"deny\",\"tool\":\"shell\",\"prefix\":\"rm\"}\n\
         \n\
         {\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"npm test\",\"added\":1700000000000,\"session_id\":\"s_0123456789abcdef\"}\n",
    );
    write(
        &home.path().join("projects/proj/rules"),
        "{\"decision\":\"ask\",\"tool\":\"shell\",\"prefix\":\"npm publish\"}\n",
    );
    let rules = files(&home, "proj").read().unwrap();
    assert_eq!(rules.global.len(), 2);
    assert_eq!(rules.global[0].prefix, "rm");
    assert_eq!(rules.global[1].prefix, "npm test");
    assert_eq!(rules.global[1].added, Some(1_700_000_000_000));
    assert_eq!(
        rules.global[1].session_id,
        Some(SessionId("s_0123456789abcdef".into()))
    );
    assert_eq!(rules.project.len(), 1);
    assert_eq!(rules.project[0].prefix, "npm publish");
}

#[test]
fn a_bad_line_errors_naming_the_file_and_line_number() {
    let home = home();
    write(
        &home.path().join("rules"),
        "{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"ok\"}\n\
         {\"decision\":\"maybe\",\"tool\":\"shell\",\"prefix\":\"bad\"}\n",
    );
    let error = files(&home, "proj").read().unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&home.path().join("rules").display().to_string()),
        "{message}"
    );
    assert!(message.contains(":2:"), "{message}");
}

#[test]
fn a_symlinked_rules_file_errors() {
    let home = home();
    let target = home.path().join("elsewhere");
    write(
        &target,
        "{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"ok\"}\n",
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, home.path().join("rules")).unwrap();
    let error = files(&home, "proj").read().unwrap_err();
    assert!(error.to_string().contains("rules"), "{}", error.to_string());
}

#[test]
fn remember_appends_an_allow_line_and_reads_back() {
    let home = home();
    let rules = files(&home, "proj");
    let session = SessionId("s_0123456789abcdef".into());
    rules.remember("shell", "npm test", &session).unwrap();
    let project_file = home.path().join("projects/proj/rules");
    let first = std::fs::read_to_string(&project_file).unwrap();
    let line: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
    assert_eq!(line["decision"], "allow");
    assert_eq!(line["tool"], "shell");
    assert_eq!(line["prefix"], "npm test");
    assert_eq!(line["added"], 1_700_000_000_000u64);
    assert_eq!(line["session_id"], "s_0123456789abcdef");

    rules.remember("shell", "npm run", &session).unwrap();
    let both = std::fs::read_to_string(&project_file).unwrap();
    let lines: Vec<&str> = both.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], first.trim());

    let read = rules.read().unwrap();
    assert_eq!(read.project.len(), 2);
    assert_eq!(read.project[0].prefix, "npm test");
    assert_eq!(read.project[0].added, Some(1_700_000_000_000));
    assert_eq!(read.project[0].session_id, Some(session.clone()));
    assert_eq!(read.project[1].prefix, "npm run");
    assert!(read.project[1].added.is_some());
}
