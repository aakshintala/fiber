//! The listing workload (`docs/performance.md`, "Budgets"): listing 1,000
//! sessions in one project, warm cache. One real session is generated, then
//! cloned 999 times with its id replaced; the hub serves the project while
//! `fiber sessions --json` is timed end to end, from just before the spawn
//! to its stdout closing.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::busy::{self, expect};
use crate::home::Home;
use crate::idle::{Ctx, Samples, Workload};
use crate::resume::{self, Fixture};
use crate::run::{self, Proc};

pub(crate) const SAMPLED: [Workload; 1] = [Workload {
    name: "sessions list",
    timing: true,
    run: sessions_list,
}];

/// How long one listing may take.
const LIST: Duration = Duration::from_secs(30);

/// The seed session the clones copy: one short turn.
const SEED: Fixture = Fixture {
    metric: "listing_seed",
    turns: 1,
    reply_bytes: 64,
    handoffs: 0,
};

/// Makes the workspace a git repository, so `fiber sessions` lists the
/// repository's project.
fn init_git(ctx: &Ctx<'_>, home: &Home) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .current_dir(home.workspace())
        .env_clear()
        .env("PATH", ctx.path.as_deref().unwrap_or_default())
        .env("HOME", home.root())
        .env(crate::home::KEY_VAR, "sk-bench")
        .arg("init")
        .arg("-q")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = command
        .status()
        .map_err(|err| format!("running git init: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git init exited with {status}"))
    }
}

/// Copies the seed session's directory under `id`, with the seed id
/// replaced by `id` in every file's text.
fn copy_session(sessions: &Path, seed: &str, id: &str) -> Result<(), String> {
    copy_dir(&sessions.join(seed), &sessions.join(id), seed, id)
}

fn copy_dir(from: &Path, to: &Path, seed: &str, id: &str) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|err| format!("creating {}: {err}", to.display()))?;
    let entries = fs::read_dir(from).map_err(|err| format!("reading {}: {err}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("reading {}: {err}", from.display()))?;
        let target = to.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|err| format!("reading {}: {err}", from.display()))?
            .is_dir()
        {
            copy_dir(&entry.path(), &target, seed, id)?;
        } else {
            let text = fs::read_to_string(entry.path())
                .map_err(|err| format!("reading {}: {err}", entry.path().display()))?;
            fs::write(&target, text.replace(seed, id))
                .map_err(|err| format!("writing {}: {err}", target.display()))?;
        }
    }
    Ok(())
}

/// Clones the seed session `copies` times: each copy's directory holds the
/// seed's files with its own id, and `recent.jsonl` gains one row per copy
/// with the copy's id and the seed's `ts` plus its index. Returns every id,
/// the seed's first.
pub(crate) fn clone_sessions(
    home: &Path,
    workspace: &Path,
    seed: &str,
    copies: usize,
) -> Result<Vec<String>, String> {
    let sessions = log::sessions_dir(home, &doors::project(workspace));
    let recent = home.join("recent.jsonl");
    let text = fs::read_to_string(&recent)
        .map_err(|err| format!("reading {}: {err}", recent.display()))?;
    let template = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|row| row.get("session_id") == Some(&json!(seed)))
        .ok_or_else(|| format!("no recent.jsonl row names {seed}"))?;
    let ts = template
        .get("ts")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("the {seed} row has no ts"))?;
    let mut ids = vec![seed.to_owned()];
    for index in 1..=copies {
        let id = doors::mint("s_");
        copy_session(&sessions, seed, &id)?;
        let mut row = template.clone();
        let row = row
            .as_object_mut()
            .ok_or_else(|| format!("the {seed} row is not an object"))?;
        row.insert("session_id".to_owned(), json!(&id));
        let step = u64::try_from(index).map_err(|err| format!("counting copies: {err}"))?;
        row.insert("ts".to_owned(), json!(ts.saturating_add(step)));
        let mut line =
            serde_json::to_vec(&row).map_err(|err| format!("serializing a recent row: {err}"))?;
        line.push(b'\n');
        fs::OpenOptions::new()
            .append(true)
            .open(&recent)
            .and_then(|mut file| file.write_all(&line))
            .map_err(|err| format!("appending to {}: {err}", recent.display()))?;
        ids.push(id);
    }
    Ok(ids)
}

/// The notes on one listing's stdout: its exit is checked by the caller.
/// Exactly the fixture's lines, each JSON, with exactly the fixture's ids.
pub(crate) fn check_listing(stdout: &str, ids: &[String]) -> Vec<String> {
    let mut notes = Vec::new();
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.len() != ids.len() {
        notes.push(format!(
            "{} listing lines, expected {}",
            lines.len(),
            ids.len()
        ));
    }
    let mut seen = BTreeSet::new();
    for line in &lines {
        match serde_json::from_str::<Value>(line) {
            Ok(row) => match row.get("id").and_then(Value::as_str) {
                Some(id) => {
                    seen.insert(id.to_owned());
                }
                None => notes.push(format!("a listing row has no id: {line:?}")),
            },
            Err(_) => notes.push(format!("a listing line is not JSON: {line:?}")),
        }
    }
    let want: BTreeSet<String> = ids.iter().cloned().collect();
    if seen != want {
        notes.push(format!(
            "listing ids differ: {} missing and {} outside the fixture, such as {:?}",
            want.difference(&seen).count(),
            seen.difference(&want).count(),
            want.symmetric_difference(&seen).take(3).collect::<Vec<_>>(),
        ));
    }
    notes
}

/// One `fiber sessions --json` in the workspace, timed to its stdout
/// closing, with its exit status.
fn list_once(ctx: &Ctx<'_>, home: &Home) -> Result<(run::Finished, Duration), String> {
    let mut command = run::command(home.fiber(), home.root(), &home.home(), ctx.path.as_deref());
    command.arg("sessions").arg("--json");
    command.current_dir(home.workspace());
    run::timed_to_end(&mut command, ctx.clock, LIST, "fiber sessions")
}

/// The listing fixture, ready to time: its home, its hub and the ids
/// the checks compare each listing against. The warm listing ran during
/// preparation, so no timed sample shares the hub or the cache with
/// setup.
pub(crate) struct ListingPrepared {
    pub(crate) home: Home,
    pub(crate) hub: Proc,
    pub(crate) ids: Vec<String>,
}

/// Prepares the listing fixture: one real session generated, cloned to
/// 1,000, the hub up, and one listing already run untimed. A failed
/// prepare still removes its home and kills its processes.
pub(crate) fn prepare_listing(
    ctx: &Ctx<'_>,
    notes: &mut Vec<String>,
) -> Result<ListingPrepared, String> {
    let home = ctx.home.child(SEED.script())?;
    let prepared = (|| {
        init_git(ctx, &home)?;
        let seed = doors::mint("s_");
        resume::generate(ctx, &home, &SEED, &seed, notes)?;
        expect(
            notes,
            "seed model requests",
            home.server().requests().len(),
            SEED.script().len() - 1,
        );
        let ids = clone_sessions(&home.home(), &home.workspace(), &seed, 999)?;
        let hub = busy::start_hub(ctx, &home)?;
        let (warm, _) = list_once(ctx, &home)?;
        if !warm.status.success() {
            return Err(format!("fiber sessions exited with {}", warm.status));
        }
        notes.extend(check_listing(&warm.stdout, &ids));
        Ok::<_, String>((hub, ids))
    })();
    match prepared {
        Ok((hub, ids)) => Ok(ListingPrepared { home, hub, ids }),
        Err(err) => {
            let _finished = home.finish();
            Err(err)
        }
    }
}

/// One timed listing against the prepared fixture: `ctx.home` is the
/// fixture's home.
pub(crate) fn sample_listing(
    prepared: &ListingPrepared,
    ctx: &Ctx<'_>,
    notes: &mut Vec<String>,
) -> Result<Samples, String> {
    let (finished, took) = list_once(ctx, &prepared.home)?;
    if !finished.status.success() {
        return Err(format!("fiber sessions exited with {}", finished.status));
    }
    notes.extend(check_listing(&finished.stdout, &prepared.ids));
    Ok(vec![(
        "sessions_list_ms",
        json!(took.as_secs_f64() * 1000.0),
    )])
}

/// Stops the fixture's hub and removes its home: the hub's error first,
/// as the single-call run reported it before the home's.
pub(crate) fn finish_listing(prepared: ListingPrepared, clock: &dyn Clock) -> Result<(), String> {
    let ListingPrepared { home, hub, .. } = prepared;
    let stopped = hub.stop(clock);
    let finished = home.finish();
    stopped.and(finished)
}

/// Generates one real session, clones it to 1,000, then lists the project
/// once untimed and `runs` times timed, through the installed hub.
fn sessions_list(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let prepared = prepare_listing(ctx, notes)?;
    let measured = (|| {
        let mut samples = Samples::new();
        for _ in 0..ctx.runs {
            samples.extend(sample_listing(&prepared, ctx, notes)?);
        }
        Ok(samples)
    })();
    // The row's hub and home go away even when a sample errs, as the old
    // `in_home` run removed them before.
    let finished = finish_listing(prepared, ctx.clock);
    match measured {
        Ok(samples) => finished.map(|()| samples),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
