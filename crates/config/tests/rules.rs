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

fn allow(prefix: &str) -> String {
    format!("{{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"{prefix}\"}}")
}

fn project_key() -> config::ProjectKey {
    ProjectKey::new("proj").unwrap()
}

#[test]
fn listing_keeps_physical_line_numbers_across_blank_lines() {
    let home = home();
    let first = allow("a");
    let second = allow("b");
    write(
        &home.path().join("rules"),
        &format!("{first}\n\n{second}\n"),
    );
    let (global, _) = config::list_rules(home.path(), &project_key());
    let lines = global.lines.unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].line, 1);
    assert_eq!(lines[0].text, first);
    assert_eq!(lines[0].rule.prefix, "a");
    assert_eq!(lines[1].line, 3);
    assert_eq!(lines[1].text, second);
    assert_eq!(lines[1].rule.prefix, "b");
}

#[test]
fn a_bad_line_is_the_files_error_naming_its_line_and_the_other_file_still_lists() {
    let home = home();
    write(
        &home.path().join("rules"),
        "{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"ok\"}\n{\"decision\":\"maybe\"}\n",
    );
    write(
        &home.path().join("projects/proj/rules"),
        &format!("{}\n", allow("p")),
    );
    let expected = files(&home, "proj").read().unwrap_err().to_string();
    assert!(expected.contains(":2:"), "{expected}");
    let (global, project) = config::list_rules(home.path(), &project_key());
    let message = global.lines.unwrap_err().to_string();
    assert_eq!(message, expected);
    let lines = project.lines.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].rule.prefix, "p");
}

#[test]
fn missing_and_empty_files_list_no_lines() {
    let home = home();
    let (global, project) = config::list_rules(home.path(), &project_key());
    assert_eq!(global.file, home.path().join("rules"));
    assert!(global.lines.unwrap().is_empty());
    assert_eq!(project.file, home.path().join("projects/proj/rules"));
    assert!(project.lines.unwrap().is_empty());
    write(&home.path().join("rules"), "");
    write(&home.path().join("projects/proj/rules"), "\n\n");
    let (global, project) = config::list_rules(home.path(), &project_key());
    assert!(global.lines.unwrap().is_empty());
    assert!(project.lines.unwrap().is_empty());
}

#[test]
fn a_crlf_line_lists_without_its_cr() {
    let home = home();
    let line = allow("a");
    write(&home.path().join("rules"), &format!("{line}\r\n"));
    let (global, _) = config::list_rules(home.path(), &project_key());
    let lines = global.lines.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].line, 1);
    assert_eq!(lines[0].text, line);
}

#[test]
fn remove_deletes_only_the_selected_occurrence() {
    let home = home();
    let line = allow("same");
    write(
        &home.path().join("projects/proj/rules"),
        &format!("{line}\n{line}\n"),
    );
    let project = project_key();
    assert!(
        config::remove_rule(home.path(), &project, config::RulesScope::Project, 2, &line).unwrap()
    );
    let left = std::fs::read_to_string(home.path().join("projects/proj/rules")).unwrap();
    assert_eq!(left, format!("{line}\n"));
    let (global, listing) = config::list_rules(home.path(), &project);
    assert!(global.lines.unwrap().is_empty());
    let lines = listing.lines.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].line, 1);
}

#[test]
fn remove_keeps_every_other_byte() {
    let home = home();
    let first = allow("a");
    let unknown = "{\"decision\":\"deny\",\"tool\":\"shell\",\"prefix\":\"b\",\"extra\":1}";
    let crlf = allow("c");
    let last = allow("d");
    let before = format!("{first}\n\n{unknown}\n{crlf}\r\n{last}");
    write(&home.path().join("projects/proj/rules"), &before);
    let project = project_key();
    assert!(
        config::remove_rule(home.path(), &project, config::RulesScope::Project, 4, &crlf).unwrap()
    );
    let after = std::fs::read(home.path().join("projects/proj/rules")).unwrap();
    assert_eq!(after, format!("{first}\n\n{unknown}\n{last}").into_bytes());
}

#[test]
fn removing_the_only_line_leaves_an_empty_file() {
    let home = home();
    let line = allow("a");
    write(
        &home.path().join("projects/proj/rules"),
        &format!("{line}\n"),
    );
    let project = project_key();
    assert!(
        config::remove_rule(home.path(), &project, config::RulesScope::Project, 1, &line).unwrap()
    );
    let after = std::fs::read(home.path().join("projects/proj/rules")).unwrap();
    assert!(after.is_empty());
}

#[test]
fn removing_an_unterminated_final_line_is_exact() {
    let rows: &[(&str, usize, &str, Option<&str>)] = &[
        ("a\nb", 2, "b", Some("a\n")),
        ("b", 1, "b", Some("")),
        ("a\r\nb\r", 2, "b\r", Some("a\r\n")),
        ("a\nb", 3, "b", None),
        ("a\nb", 4, "b", None),
    ];
    for (before, line, text, expected) in rows {
        let home = home();
        write(&home.path().join("projects/proj/rules"), before);
        let project = project_key();
        let removed = config::remove_rule(
            home.path(),
            &project,
            config::RulesScope::Project,
            *line,
            text,
        )
        .unwrap();
        let after = std::fs::read(home.path().join("projects/proj/rules")).unwrap();
        match expected {
            Some(want) => {
                assert!(removed, "line {line} of {before:?}");
                assert_eq!(after, want.as_bytes(), "line {line} of {before:?}");
            }
            None => {
                assert!(!removed, "line {line} of {before:?}");
                assert_eq!(after, before.as_bytes(), "line {line} of {before:?}");
            }
        }
    }
}

#[test]
fn a_changed_line_is_stale_and_writes_nothing() {
    let home = home();
    let line = allow("a");
    let file = home.path().join("projects/proj/rules");
    write(&file, &format!("{line}\n"));
    let before = std::fs::read(&file).unwrap();
    let project = project_key();
    assert!(
        !config::remove_rule(
            home.path(),
            &project,
            config::RulesScope::Project,
            1,
            &allow("b")
        )
        .unwrap()
    );
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn line_zero_and_lines_past_the_end_are_stale() {
    let home = home();
    let rows = [allow("a"), allow("b"), allow("c")];
    let file = home.path().join("projects/proj/rules");
    write(&file, &format!("{}\n{}\n{}\n", rows[0], rows[1], rows[2]));
    let project = project_key();
    for line in [0, 4, 5] {
        assert!(
            !config::remove_rule(home.path(), &project, config::RulesScope::Project, line, "")
                .unwrap(),
            "line {line}"
        );
    }
    assert!(
        config::remove_rule(
            home.path(),
            &project,
            config::RulesScope::Project,
            3,
            &rows[2]
        )
        .unwrap()
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        format!("{}\n{}\n", rows[0], rows[1]).into_bytes()
    );
}

#[test]
fn a_deleted_file_is_stale_and_creates_nothing() {
    let home = home();
    let project = project_key();
    assert!(
        !config::remove_rule(home.path(), &project, config::RulesScope::Project, 1, "x").unwrap()
    );
    assert!(!home.path().join("projects/proj").exists());
    assert!(!home.path().join("projects/proj/rules.lock").exists());
}

#[test]
fn remove_rule_picks_the_scopes_file() {
    let home = home();
    let line = allow("same");
    write(&home.path().join("rules"), &format!("{line}\n"));
    write(
        &home.path().join("projects/proj/rules"),
        &format!("{line}\n"),
    );
    let project = project_key();
    assert!(
        config::remove_rule(home.path(), &project, config::RulesScope::Global, 1, &line).unwrap()
    );
    assert_eq!(
        std::fs::read(home.path().join("rules")).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("projects/proj/rules")).unwrap(),
        format!("{line}\n")
    );
    assert!(
        config::remove_rule(home.path(), &project, config::RulesScope::Project, 1, &line).unwrap()
    );
    assert_eq!(
        std::fs::read(home.path().join("projects/proj/rules")).unwrap(),
        Vec::<u8>::new()
    );
}
