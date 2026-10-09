use std::collections::BTreeSet;
use std::fs;

use fakes::TempDir;
use serde_json::{Value, json};

use super::{check_listing, clone_sessions};

/// A home with a seed session: its directory holding an `events.jsonl`
/// naming `seed`, and one `recent.jsonl` row for it.
fn seeded(home: &TempDir, workspace: &std::path::Path, seed: &str, project: &str, ts: u64) {
    let dir = home
        .path()
        .join("projects")
        .join(project)
        .join("sessions")
        .join(seed);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        format!("{{\"kind\":\"session_started\",\"session_id\":\"{seed}\"}}\n"),
    )
    .unwrap();
    fs::write(
        home.path().join("recent.jsonl"),
        format!(
            "{}\n",
            json!({
                "session_id": seed,
                "ts": ts,
                "project": project,
                "workspace": workspace.to_string_lossy(),
                "name": "",
                "how": "exited",
            })
        ),
    )
    .unwrap();
}

fn rows(home: &TempDir) -> Vec<Value> {
    fs::read_to_string(home.path().join("recent.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn cloning_writes_n_directories_and_n_rows_that_parse_as_recent_rows() {
    let home = TempDir::new("fiber-bench-listing");
    let workspace = home.path().join("w");
    fs::create_dir_all(&workspace).unwrap();
    let project = log::project_key(&doors::project(&workspace));
    seeded(&home, &workspace, "s_seed", &project, 1_700_000_000_000);
    let ids = clone_sessions(home.path(), &workspace, "s_seed", 3).unwrap();
    assert_eq!(ids.len(), 4);
    assert_eq!(BTreeSet::from_iter(ids.iter().cloned()).len(), 4);

    let sessions = home.path().join("projects").join(&project).join("sessions");
    for id in &ids {
        let log = fs::read_to_string(sessions.join(id).join("events.jsonl")).unwrap();
        assert!(log.contains(id), "{id}: {log}");
        if *id != "s_seed" {
            assert!(!log.contains("s_seed"), "{id}: {log}");
        }
    }
    let parsed = rows(&home);
    assert_eq!(parsed.len(), 4);
    let mut seen = BTreeSet::new();
    let mut stamps = BTreeSet::new();
    for row in &parsed {
        let recent: hub::RecentRow = serde_json::from_value(row.clone()).unwrap();
        assert_eq!(recent.project, project);
        seen.insert(recent.session_id.0.clone());
        stamps.insert(recent.ts);
    }
    assert_eq!(seen, BTreeSet::from_iter(ids.iter().cloned()));
    assert_eq!(
        stamps,
        BTreeSet::from([
            1_700_000_000_000,
            1_700_000_000_001,
            1_700_000_000_002,
            1_700_000_000_003
        ])
    );
}

#[test]
fn the_listing_check_passes_on_the_fixture_ids_and_notes_999_lines() {
    let ids: Vec<String> = (0..1000).map(|n| format!("s_{n:04}")).collect();
    let stdout = ids
        .iter()
        .map(|id| json!({"id": id, "state": "exited"}).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(check_listing(&stdout, &ids), Vec::<String>::new());

    let short = stdout.lines().take(999).collect::<Vec<_>>().join("\n");
    let notes = check_listing(&short, &ids);
    assert!(!notes.is_empty(), "999 of 1,000 lines passed");
    assert!(notes.iter().any(|note| note.contains("999")), "{notes:?}");
}

#[test]
fn the_listing_check_notes_a_line_that_is_not_json() {
    let ids: Vec<String> = (0..1000).map(|n| format!("s_{n:04}")).collect();
    let mut lines: Vec<String> = ids.iter().map(|id| json!({"id": id}).to_string()).collect();
    lines[500] = "panicked at sessions.rs".to_owned();
    let notes = check_listing(&lines.join("\n"), &ids);
    assert!(!notes.is_empty(), "a non-JSON line passed");

    lines[500] = json!({"state": "exited"}).to_string();
    let notes = check_listing(&lines.join("\n"), &ids);
    assert!(!notes.is_empty(), "a row without an id passed");
}

#[test]
fn the_listing_check_notes_an_id_outside_the_fixture() {
    let ids: Vec<String> = (0..1000).map(|n| format!("s_{n:04}")).collect();
    let mut lines: Vec<String> = ids.iter().map(|id| json!({"id": id}).to_string()).collect();
    lines[0] = json!({"id": "s_intruder"}).to_string();
    let notes = check_listing(&lines.join("\n"), &ids);
    assert!(!notes.is_empty(), "an unknown id passed");
    assert!(
        notes.iter().any(|note| note.contains("s_intruder")),
        "{notes:?}"
    );
}
