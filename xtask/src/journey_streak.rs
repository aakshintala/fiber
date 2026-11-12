use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};

pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    match args.first().map(String::as_str) {
        Some("select") => {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| e.to_string())?;
            let runs: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
            for id in select(&runs) {
                println!("{id}");
            }
            Ok(true)
        }
        Some("compute") => compute(args.get(1..).ok_or("missing command arguments")?),
        _ => Err("usage: cargo xtask journey-streak <select|compute>".into()),
    }
}

fn select(runs: &Value) -> Vec<String> {
    let mut candidates: Vec<(u64, String)> = runs
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|run| {
            let event = run.get("event")?.as_str()?;
            let branch = run.get("headBranch")?.as_str()?;
            let conclusion = run.get("conclusion")?.as_str()?;
            let eligible = branch == "main"
                && match event {
                    "push" => matches!(conclusion, "success" | "failure"),
                    "schedule" | "workflow_dispatch" => conclusion == "success",
                    _ => false,
                };
            if !eligible {
                return None;
            }
            Some((
                run.get("databaseId")?.as_u64()?,
                run.get("databaseId")?.to_string(),
            ))
        })
        .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.into_iter().take(20).map(|(_, id)| id).collect()
}

fn compute(args: &[String]) -> Result<bool, String> {
    let flag = |name: &str| -> Result<&str, String> {
        let index = args
            .iter()
            .position(|arg| arg == name)
            .ok_or_else(|| format!("missing {name}"))?;
        args.get(index + 1)
            .map(String::as_str)
            .ok_or_else(|| format!("missing value for {name}"))
    };
    let current_id = flag("--current-id")?;
    let current_sha = flag("--current-sha")?;
    let failures_path = flag("--failures")?;
    let runs_path = flag("--runs")?;
    let history_dir = flag("--history-dir")?;
    let output_path = flag("--output")?;
    let current = std::fs::read_to_string(failures_path).map_err(|e| e.to_string())?;
    let runs: Value = serde_json::from_slice(&std::fs::read(runs_path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut history = Vec::new();
    for id in select(&runs) {
        if id == current_id {
            continue;
        }
        let run = runs
            .as_array()
            .and_then(|runs| {
                runs.iter().find(|run| {
                    run.get("databaseId")
                        .is_some_and(|value| id.parse::<u64>().ok() == value.as_u64())
                })
            })
            .ok_or("selected run disappeared")?;
        let sha = run
            .get("headSha")
            .and_then(Value::as_str)
            .ok_or("selected run has no head SHA")?;
        let dir = Path::new(history_dir).join(&id);
        let view: Value = match std::fs::read(dir.join("view.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(view) => view,
                Err(_) => return Ok(true),
            },
            Err(_) => return Ok(true),
        };
        let Some(producer) = producer_job(&view) else {
            continue;
        };
        let Some(conclusion) = producer.get("conclusion").and_then(Value::as_str) else {
            continue;
        };
        if conclusion == "skipped" {
            continue;
        }
        let artifact = std::fs::read_to_string(dir.join("journey-failures.txt")).ok();
        history.push((sha.to_owned(), artifact, true));
        if history.len() == 20 {
            break;
        }
    }
    let streaks = streaks(current_sha, &current, &history);
    let metadata: Map<String, Value> = streaks
        .into_iter()
        .filter_map(|(name, (count, first_sha))| {
            (count >= 2).then(|| {
                (
                    name,
                    serde_json::json!({"count": count, "first_sha": first_sha}),
                )
            })
        })
        .collect();
    if metadata.is_empty() {
        if Path::new(output_path).exists() {
            std::fs::remove_file(output_path).map_err(|e| e.to_string())?;
        }
    } else {
        std::fs::write(output_path, Value::Object(metadata).to_string())
            .map_err(|e| e.to_string())?;
    }
    Ok(true)
}

fn producer_job(view: &Value) -> Option<&Value> {
    view.get("jobs")?.as_array()?.iter().find(|job| {
        job.get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.contains("journey suite"))
    })
}

fn streaks(
    current_sha: &str,
    current: &str,
    history: &[(String, Option<String>, bool)],
) -> BTreeMap<String, (usize, String)> {
    let mut results = BTreeMap::new();
    for journey in current.lines().filter(|line| !line.is_empty()) {
        let mut count = 1;
        let mut first = current_sha.to_owned();
        for (sha, failures, ran) in history {
            if !ran {
                continue;
            }
            let Some(failures) = failures else {
                break;
            };
            if !failures.lines().any(|name| name == journey) {
                break;
            }
            count += 1;
            first.clone_from(sha);
        }
        results.insert(journey.to_owned(), (count, first));
    }
    results
}

#[cfg(test)]
#[path = "journey_streak_tests.rs"]
mod journey_streak_tests;
