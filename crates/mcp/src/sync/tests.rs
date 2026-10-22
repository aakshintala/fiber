//! The reload sync's commit and classification through the public
//! stage and commit entries: keeps install staged lists, replacements
//! swap the slot, and failures file under the right quadrant.

use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use contract::events::ServerFailure;
use contract::tool::ServerRecord;
use fakes::Deadline;
use serde_json::json;

use crate::Hints;
use crate::slot::Served;
use crate::test_support::FakeImages;
use crate::test_support::{Setup, WITHIN};
use crate::{Disposition, SyncRow, apply_sync, classify_sync, plan_sync, stage_sync};

const HI: &str = r#"{"content":[{"type":"text","text":"hi"}]}"#;

/// One sync's owned inputs: the locals `SyncInputs` borrows, so one
/// binding carries every stage and commit call in a test.
struct OwnedInputs {
    workspace: std::path::PathBuf,
    artifacts: std::path::PathBuf,
    images: std::sync::Arc<dyn contract::images::Images>,
    cache: std::path::PathBuf,
    clock: std::sync::Arc<dyn contract::clock::Clock>,
}

impl OwnedInputs {
    fn inputs(&self) -> crate::SyncInputs<'_> {
        crate::SyncInputs {
            workspace: &self.workspace,
            artifacts: &self.artifacts,
            images: &self.images,
            cache: &self.cache,
            clock: &self.clock,
            version: "test",
        }
    }
}

impl Setup {
    /// Stages one classification of `specs` against `servers`, on a
    /// thread: the wedge tests advance the fake clock past each bound
    /// while the stage parks. Sends the staged sync back with the servers
    /// it borrowed, so the test stops what the stage left behind.
    fn stage_on_thread(
        rows: Vec<SyncRow>,
        servers: crate::start::Servers,
        setup: &Setup,
        images: &std::sync::Arc<FakeImages>,
    ) -> mpsc::Receiver<(
        Result<crate::StagedSync, contract::shapes::Failure>,
        crate::start::Servers,
    )> {
        let owned = OwnedInputs {
            workspace: setup.workspace(),
            artifacts: setup.artifacts(),
            images: std::sync::Arc::<FakeImages>::clone(images),
            cache: setup.cache(),
            clock: setup.clock(),
        };
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let staged = stage_sync(rows, &servers, &owned.inputs());
            done.send((staged, servers)).expect("collected");
        });
        result
    }

    /// One sync's owned inputs, built from this fixture's directories.
    fn owned_inputs(&self, images: &std::sync::Arc<FakeImages>) -> OwnedInputs {
        OwnedInputs {
            workspace: self.workspace(),
            artifacts: self.artifacts(),
            images: std::sync::Arc::<FakeImages>::clone(images),
            cache: self.cache(),
            clock: self.clock(),
        }
    }
}

#[test]
fn commit_writes_no_cache_for_an_unchanged_keep() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    let cache_before = setup.cache_bytes();
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("an unchanged keep stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "equal lists leave the file untouched"
    );
    setup.stop(servers);
}

#[test]
fn install_relisted_installs_only_on_a_live_slot() {
    // The install-time liveness revalidation: a live slot installs the
    // re-list and answers true, so the commit's cache write persists
    // only lists a live server served; a slot whose server died keeps
    // what it holds instead of publishing lists its dead server never
    // served, so the commit cannot mark a dead keep as kept.
    for dead in [false, true] {
        let state = if dead { "dead" } else { "live" };
        let setup = Setup::new();
        setup.tools(&serde_json::json!([{"name": "echo"}]));
        let started = setup.start(vec![setup.spec("fx")]);
        let slot = setup.running(&started);
        if dead {
            setup.kill(&started);
            assert!(slot.is_running(), "the kill moves no slot");
        }
        assert_eq!(
            slot.install_relisted(crate::cache::Cached {
                tools: vec![
                    serde_json::from_value(json!({"name": "echo"})).expect("listed"),
                    serde_json::from_value(json!({"name": "plus"})).expect("listed"),
                ],
                prompts: Vec::new(),
                resources: false,
            }),
            !dead,
            "a {state} slot answers whether it installed",
        );
        let expected = if dead {
            vec!["echo".to_owned()]
        } else {
            vec!["echo".to_owned(), "plus".to_owned()]
        };
        let live: Vec<String> = {
            let mut names: Vec<String> = slot.live_names().into_iter().collect();
            names.sort();
            names
        };
        assert_eq!(live, expected, "a {state} slot holds the installed set");
        let listed: Vec<String> = {
            let mut names: Vec<String> = slot
                .listed_tools()
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            names.sort();
            names
        };
        assert_eq!(listed, expected, "a {state} slot lists the installed set");
        setup.stop(started.servers);
    }
}

#[test]
fn commit_writes_a_prompts_only_change_for_an_unchanged_tool_list() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.prompts(&serde_json::json!([{"name": "old"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    setup.prompts(&serde_json::json!([{"name": "new"}]));
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a prompts-only change stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    let names: Vec<String> = setup
        .cached_prompts()
        .iter()
        .map(|prompt| prompt.name.clone())
        .collect();
    assert_eq!(names, vec!["new".to_owned()]);
    setup.stop(servers);
}

#[test]
fn commit_installs_a_keep_dead_at_install_as_listed() {
    // A keep that dies after its re-list installs its staged lists as
    // kept: the commit stages nothing and cannot fail, so the dead
    // keep's slot and cache stay exactly as the stage left them —
    // listed but not live — and the next call restarts it.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let pid = setup.pid();
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Keep { .. }]));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a keep stages");
    let cache_before = setup.cache_bytes();
    setup.kill_slot(&slot);
    assert!(!slot.is_live(), "the kept slot died before the commit");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert!(synced.servers.restarted.is_empty());
    assert!(synced.records.is_empty(), "a keep writes no records");
    assert!(synced.failed.is_empty());
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the dead keep's cache stays as the stage left it"
    );
    assert!(!slot.is_live(), "the dead keep stays listed but not live");
    let echo = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the staged lists still declare");
    assert!(
        setup.run(&echo.1).error.is_none(),
        "the next call restarts the dead keep"
    );
    assert_ne!(setup.pid(), pid, "the restart spawns its own server");
    assert!(slot.is_live(), "the keep serves again");
    setup.stop(servers);
}

#[test]
fn commit_never_writes_a_declined_keep_payload() {
    // A keep whose server died after staging never persists the staged
    // lists: the install declines them, so the cache keeps the old bytes
    // while the staged tools still declare and the next call restarts it.
    // The staged list differs from the cache here, so an unconditional
    // write would show: equal lists would pass either way.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let cache_before = setup.cache_bytes();
    setup.tools(&serde_json::json!([{"name": "echo"}, {"name": "plus"}]));
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Keep { .. }]));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a keep stages");
    setup.kill_slot(&slot);
    assert!(!slot.is_live(), "the kept slot died before the commit");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the declined staged lists never reach the cache"
    );
    assert!(
        synced
            .tools
            .iter()
            .any(|(_, _, info)| info.name == "mcp__fx__plus"),
        "the staged lists still declare"
    );
    let echo = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the staged lists still declare");
    assert!(
        setup.run(&echo.1).error.is_none(),
        "the next call restarts the dead keep"
    );
    assert!(slot.is_live(), "the keep serves again");
    setup.stop(servers);
}

#[test]
fn an_optional_replacement_failure_still_writes_the_detected_death() {
    // An optional replacement whose shadow fails still records the death
    // the sync detected: the failure row carries the old deaths, and the
    // commit writes them before the failure (`docs/mcp.md`, "Each death
    // is recorded as `mcp_server_failed`").
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    setup.running(&started);
    setup.kill(&started);
    // The replacement cannot start, but the death is already detected.
    setup.write("fail-start", "");
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(
        rows.as_slice(),
        [SyncRow::Shadow {
            disposition: Disposition::Restarted,
            ..
        }]
    ));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("an optional shadow failure stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.restarted, vec!["fx".to_owned()]);
    assert_eq!(
        synced.records.len(),
        2,
        "the death rides beside the failure"
    );
    let reasons: Vec<ServerFailure> = synced
        .records
        .iter()
        .map(|record| match record {
            ServerRecord::Failed(failed) => failed.reason,
            ServerRecord::Ready(_) => panic!("a failure installs no ready"),
        })
        .collect();
    assert_eq!(
        reasons,
        vec![ServerFailure::Died, ServerFailure::StartFailed]
    );
    assert_eq!(synced.failed.len(), 1, "the failure rides the entry");
    setup.stop(servers);
}

#[test]
fn commit_declares_mcp_resources_only_when_a_slot_offers_them() {
    for resources in [true, false] {
        let setup = Setup::new();
        let spec = setup.spec("fx");
        crate::cache::write(
            &setup.cache(),
            "fx",
            &spec.key(),
            &crate::cache::Cached {
                tools: Vec::new(),
                prompts: Vec::new(),
                resources,
            },
        );
        let started = setup.start(vec![spec]);
        assert!(!setup.spawned(), "the cache declares without spawning");
        let images = FakeImages::ok();
        let result = Setup::stage_on_thread(Vec::new(), started.servers, &setup, &images);
        let (outcome, servers) = Deadline::after(WITHIN)
            .recv(&result)
            .expect("the stage ends");
        let staged = outcome.expect("an empty stage");
        let owned = setup.owned_inputs(&images);
        let synced = plan_sync(&staged, &servers, &owned.inputs());
        apply_sync(staged, &servers, &owned.inputs());
        let offered = synced
            .tools
            .iter()
            .any(|(_, _, info)| info.name == crate::resources::RESOURCES_TOOL);
        assert_eq!(offered, resources, "resources: {resources}");
        setup.stop(servers);
    }
}

#[test]
fn classify_starts_a_lazy_slot_and_commit_swaps_it_for_a_live_one() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let spec = setup.spec("fx");
    let images = FakeImages::ok();
    // The first start writes the cache; stopping it leaves the cache
    // behind, so the second start declares from the cache without
    // spawning: the slot never serves.
    let first = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
    let pid = setup.pid();
    setup.stop(first.servers);
    assert!(!fakes::pid_alive(pid), "pid {pid} to be reaped");
    let started = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
    assert_eq!(setup.pid(), pid, "the lazy start spawns nothing");
    let slot = started
        .servers
        .slot("fx")
        .expect("the lazy slot is adopted");
    assert!(!slot.is_running());
    assert!(!slot.is_live());
    let rows = classify_sync(vec![spec], &started.servers);
    assert!(matches!(
        rows.as_slice(),
        [SyncRow::Shadow {
            disposition: Disposition::Started,
            ..
        }]
    ));
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = fakes::Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a fresh start stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.started, ["fx".to_owned()]);
    assert!(
        matches!(synced.records.as_slice(), [ServerRecord::Ready(_)]),
        "a fresh start reads ready, not failed"
    );
    let echo = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the new server is declared");
    assert!(setup.run(&echo.1).error.is_none());
    assert!(
        servers.slot("fx").is_some_and(|slot| slot.is_live()),
        "the commit swapped the lazy slot for a live one"
    );
    setup.stop(servers);
}

#[test]
fn classify_replaces_a_nonlive_slot_and_the_commit_files_it_restarted() {
    // A slot that served before and serves no more — `Down` with its
    // restart unused, or `Dead` after a failed restart — takes the
    // replace path beside the old slot, and the commit files it under
    // `restarted`, never `started`.
    for dead in [false, true] {
        let state = if dead { "dead" } else { "down" };
        let setup = Setup::new();
        setup.tools(&serde_json::json!([{"name": "echo"}]));
        setup.result("echo", HI);
        let spec = setup.spec("fx");
        let images = FakeImages::ok();
        // The first start writes the cache; stopping it leaves the
        // cache behind. A `Down` slot declares from the cache without
        // spawning and fails its first serve; a `Dead` slot runs, is
        // killed, and fails its restart.
        let started = if dead {
            let started = setup.start(vec![spec.clone()]);
            let slot = setup.running(&started);
            setup.kill(&started);
            setup.write("fail-start", "");
            assert!(
                matches!(setup.serve(&slot), Served::Failed(_)),
                "the restart fails"
            );
            started
        } else {
            let first = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
            setup.stop(first.servers);
            let started = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
            let slot = started.servers.slot("fx").expect("the lazy slot");
            setup.write("fail-start", "");
            assert!(
                matches!(setup.serve(&slot), Served::Failed(_)),
                "the first serve fails"
            );
            started
        };
        let rows = classify_sync(vec![spec], &started.servers);
        assert!(
            matches!(
                rows.as_slice(),
                [SyncRow::Shadow {
                    disposition: Disposition::Restarted,
                    ..
                }]
            ),
            "a {state} slot takes the replace path"
        );
        std::fs::remove_file(setup.dir.path().join("fail-start")).expect("fail-start");
        let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
        let (outcome, servers) = Deadline::after(WITHIN)
            .recv(&result)
            .expect("the stage ends");
        let staged = outcome.expect("a replacement stages");
        let owned = setup.owned_inputs(&images);
        let synced = plan_sync(&staged, &servers, &owned.inputs());
        apply_sync(staged, &servers, &owned.inputs());
        assert_eq!(synced.servers.restarted, vec!["fx".to_owned()]);
        assert!(
            synced.servers.started.is_empty(),
            "a {state} slot never files under started"
        );
        let echo = synced
            .tools
            .iter()
            .find(|(_, _, info)| info.name == "mcp__fx__echo")
            .expect("the replacement is declared");
        assert!(
            setup.run(&echo.1).error.is_none(),
            "the replacement answers"
        );
        assert!(
            servers.slot("fx").is_some_and(|slot| slot.is_live()),
            "the commit swapped the {state} slot for a live one"
        );
        setup.stop(servers);
    }
}

#[test]
fn classify_files_a_changed_key_by_whether_the_slot_ran_this_session() {
    // A changed start key replaces the slot either way, but the commit
    // files the name under `started` when the old slot never started this
    // session and under `restarted` when it ran (a running slot does too,
    // served or not; this test covers the served, nonlive case).
    for served in [false, true] {
        let state = if served { "served" } else { "never-started" };
        let setup = Setup::new();
        setup.tools(&serde_json::json!([{"name": "echo"}]));
        setup.result("echo", HI);
        let spec = setup.spec("fx");
        let images = FakeImages::ok();
        // The first start writes the cache; stopping it leaves the cache
        // behind, so the second start declares from the cache without
        // spawning: the slot never serves until its first serve, which
        // fails here so the slot has served before and is `Down`.
        let first = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
        setup.stop(first.servers);
        let started = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
        let slot = started.servers.slot("fx").expect("the lazy slot");
        if served {
            setup.write("fail-start", "");
            assert!(
                matches!(setup.serve(&slot), Served::Failed(_)),
                "the first serve fails"
            );
            std::fs::remove_file(setup.dir.path().join("fail-start")).expect("fail-start");
        }
        assert_eq!(slot.served_before(), served);
        assert!(!slot.is_live());
        let mut changed = spec;
        let crate::Transport::Stdio(stdio) = &mut changed.transport else {
            unreachable!("the fixture spec is stdio");
        };
        stdio.env.insert("FX_CHANGED".to_owned(), "1".to_owned());
        assert_ne!(slot.spec().key(), changed.key());
        let rows = classify_sync(vec![changed], &started.servers);
        let disposition = if served {
            Disposition::Restarted
        } else {
            Disposition::Started
        };
        assert!(
            matches!(
                rows.as_slice(),
                [SyncRow::Shadow { disposition: row, .. }] if *row == disposition
            ),
            "a {state} slot with a changed key files {disposition:?}"
        );
        let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
        let (outcome, servers) = Deadline::after(WITHIN)
            .recv(&result)
            .expect("the stage ends");
        let staged = outcome.expect("a replacement stages");
        let owned = setup.owned_inputs(&images);
        let synced = plan_sync(&staged, &servers, &owned.inputs());
        apply_sync(staged, &servers, &owned.inputs());
        let (filed, other) = if served {
            (&synced.servers.restarted, &synced.servers.started)
        } else {
            (&synced.servers.started, &synced.servers.restarted)
        };
        assert_eq!(filed, &vec!["fx".to_owned()], "a {state} slot's report");
        assert!(other.is_empty(), "a {state} slot files under one quadrant");
        setup.stop(servers);
    }
}

#[test]
fn stage_sync_starts_shadows_in_parallel() {
    // Two shadow starts each park in their spawn guard until both have
    // entered: serial staging parks the first guard forever and never
    // sends the second, so the wall-clock Deadline on the entered wait
    // below is the proving signal; parallel staging releases both.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let empty = setup.start(vec![]);
    let (entered_tx, entered) = mpsc::channel::<String>();
    let gate = Arc::new(Barrier::new(3));
    let mut specs = Vec::new();
    for name in ["aa", "bb"] {
        let base = setup.spec(name);
        let (command, args, env, secrets, reader) = match &base.transport {
            crate::Transport::Stdio(stdio) => (
                stdio.command.clone(),
                stdio.args.clone(),
                stdio.env.clone(),
                stdio.secrets.clone(),
                stdio.reader.clone(),
            ),
            crate::Transport::Http { .. } => panic!("the fixture specs are stdio"),
        };
        let tag = name.to_owned();
        let report = entered_tx.clone();
        let gate = Arc::clone(&gate);
        // The guard runs immediately before the spawn: parking here
        // proves the shadow started without launching anything yet.
        let guard = Arc::new(move || {
            report.send(tag.clone()).expect("collected");
            gate.wait();
            Ok(())
        });
        specs.push(crate::ServerSpec {
            transport: crate::Transport::Stdio(crate::StdioSpec {
                command: command.clone(),
                args: args.clone(),
                env,
                secrets,
                reader,
                run: Some(crate::Program {
                    command,
                    args,
                    guard,
                }),
            }),
            ..base
        });
    }
    let rows = classify_sync(specs, &empty.servers);
    assert!(matches!(
        rows.as_slice(),
        [SyncRow::Shadow { .. }, SyncRow::Shadow { .. }]
    ));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, empty.servers, &setup, &images);
    // Both shadows enter before either launches: on serial code the
    // second send never comes and this Deadline fails the test.
    let mut entered_names = Vec::new();
    for _ in 0..2 {
        entered_names.push(
            Deadline::after(WITHIN)
                .recv(&entered)
                .expect("both shadows enter before either launches"),
        );
    }
    entered_names.sort();
    assert_eq!(entered_names, vec!["aa".to_owned(), "bb".to_owned()]);
    gate.wait();
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("fresh starts stage");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    apply_sync(staged, &servers, &owned.inputs());
    assert_eq!(
        synced.servers.started,
        vec!["aa".to_owned(), "bb".to_owned()]
    );
    setup.stop(servers);
}

// Reload, round 23: each `||` of the changed-spec chain is independently
// decisive — a spec differing in only one of `startup_timeout`,
// `call_timeout` or `required` takes the replace path, never Keep — the
// unapplied stage stops its shadows, the required guard rejects, and the
// resources scan counts only slots the apply keeps.

#[test]
fn classify_replaces_a_startup_timeout_only_change() {
    // Bites the `:144` `||`→`&&`: under the mutant every unchanged
    // comparison is false, so the chain reads false and the row stays
    // Keep; the single differing timeout must replace.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    let mut changed = setup.spec("fx");
    changed.startup_timeout = Duration::from_secs(97);
    let rows = classify_sync(vec![changed], &started.servers);
    assert!(
        matches!(
            rows.as_slice(),
            [SyncRow::Shadow {
                disposition: Disposition::Restarted,
                ..
            }]
        ),
        "a startup-timeout-only change replaces"
    );
    setup.stop(started.servers);
}

#[test]
fn classify_replaces_a_call_timeout_only_change() {
    // Bites the `:145` `||`→`&&`, as above for `call_timeout`.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    let mut changed = setup.spec("fx");
    changed.call_timeout = Duration::from_secs(97);
    let rows = classify_sync(vec![changed], &started.servers);
    assert!(
        matches!(
            rows.as_slice(),
            [SyncRow::Shadow {
                disposition: Disposition::Restarted,
                ..
            }]
        ),
        "a call-timeout-only change replaces"
    );
    setup.stop(started.servers);
}

#[test]
fn classify_replaces_a_required_only_change() {
    // Bites the `:146` `||`→`&&`, as above for `required`.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    let mut changed = setup.spec("fx");
    changed.required = true;
    let rows = classify_sync(vec![changed], &started.servers);
    assert!(
        matches!(
            rows.as_slice(),
            [SyncRow::Shadow {
                disposition: Disposition::Restarted,
                ..
            }]
        ),
        "a required-only change replaces"
    );
    setup.stop(started.servers);
}

#[test]
fn dropping_an_unapplied_stage_stops_its_shadows() {
    // Bites the `Drop for StagedSync`→`()` mutant: a staged sync dropped
    // without `apply_sync` stops every held shadow, so no replacement
    // child outlives the rejection while the old slots keep serving.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let empty = setup.start(vec![]);
    let rows = classify_sync(vec![setup.spec("fx")], &empty.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Shadow { .. }]));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, empty.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a fresh start stages");
    let shadow = staged
        .rows
        .iter()
        .find_map(|row| match row {
            super::StagedRow::ShadowUp { slot, .. } => Some(Arc::clone(slot)),
            super::StagedRow::Keep { .. }
            | super::StagedRow::ShadowFailed { .. }
            | super::StagedRow::Remove { .. } => None,
        })
        .expect("a staged shadow");
    assert!(shadow.is_live(), "the shadow serves before the drop");
    drop(staged);
    assert!(
        !shadow.is_running(),
        "dropping an unapplied stage stops every held shadow"
    );
    assert!(!shadow.is_live(), "the stopped shadow serves no more");
    setup.stop(servers);
}

/// A spec refusing every spawn: the stage's shadow start fails without
/// launching anything.
fn refusing_spec(setup: &Setup, name: &str, required: bool) -> crate::ServerSpec {
    let mut spec = setup.spec(name);
    spec.required = required;
    spec.stdio_mut().run = Some(crate::Program {
        command: fakes::mcp_fixture().display().to_string(),
        args: vec![setup.dir.path().display().to_string()],
        guard: Arc::new(|| Err("the test refused the spawn".to_owned())),
    });
    spec
}

#[test]
fn a_required_shadow_failure_rejects_and_stops_every_shadow() {
    // Bites the `job.spec.required`→`false` guard: a required shadow
    // that fails to start rejects with the start's code, and the stage
    // stops every shadow it started, so no replacement child outlives
    // the rejection.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let good_dirs = Setup::new();
    good_dirs.tools(&serde_json::json!([{"name": "echo"}]));
    let bad = refusing_spec(&setup, "bad", true);
    let good = good_dirs.spec("good");
    let empty = setup.start(vec![]);
    let rows = vec![
        SyncRow::Shadow {
            spec: bad,
            disposition: Disposition::Restarted,
        },
        SyncRow::Shadow {
            spec: good,
            disposition: Disposition::Started,
        },
    ];
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, empty.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let failure = match outcome {
        Err(failure) => failure,
        Ok(_) => panic!("a required shadow failure rejects"),
    };
    assert_eq!(
        failure.code,
        contract::ErrorCode::McpRequiredServerFailed,
        "the rejection carries the start's code"
    );
    assert!(!setup.spawned(), "the refused shadow spawns nothing");
    assert!(
        !fakes::pid_alive(good_dirs.pid()),
        "the rejection stops every shadow it started"
    );
    setup.stop(servers);
}

#[test]
fn an_optional_shadow_failure_stages_beside_a_live_shadow() {
    // The required guard's other polarity: an optional shadow that fails
    // to start stages its failure without stopping the shadow that
    // started, and the plan files the failure beside the ready.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let good_dirs = Setup::new();
    good_dirs.tools(&serde_json::json!([{"name": "echo"}]));
    let bad = refusing_spec(&setup, "bad", false);
    let good = good_dirs.spec("good");
    let empty = setup.start(vec![]);
    let rows = vec![
        SyncRow::Shadow {
            spec: bad,
            disposition: Disposition::Started,
        },
        SyncRow::Shadow {
            spec: good,
            disposition: Disposition::Started,
        },
    ];
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, empty.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("an optional shadow failure stages");
    let live = staged
        .rows
        .iter()
        .find_map(|row| match row {
            super::StagedRow::ShadowUp { slot, .. } => Some(Arc::clone(slot)),
            super::StagedRow::Keep { .. }
            | super::StagedRow::ShadowFailed { .. }
            | super::StagedRow::Remove { .. } => None,
        })
        .expect("the successful shadow stages up");
    assert!(live.is_live(), "an optional failure stops no other shadow");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    assert_eq!(synced.failed.len(), 1, "the failure rides the entry");
    assert_eq!(
        synced.servers.started,
        vec!["bad".to_owned(), "good".to_owned()]
    );
    apply_sync(staged, &servers, &owned.inputs());
    assert!(
        servers.slot("good").is_some_and(|slot| slot.is_live()),
        "the successful shadow installs live"
    );
    setup.stop(servers);
}

#[test]
fn plan_ignores_resources_of_a_dropped_slot() {
    // Bites the `:532` `&&`→`||`: the kept-slot scan counts only slots
    // the apply keeps. The old slot offers resources through its cache
    // but the row drops it, and the staged replacement offers none, so
    // nothing declares `mcp_resources`; the mutant counts the dropped
    // slot's offer.
    let setup = Setup::new();
    let spec = setup.spec("fx");
    crate::cache::write(
        &setup.cache(),
        "fx",
        &spec.key(),
        &crate::cache::Cached {
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: true,
        },
    );
    let started = setup.start(vec![spec.clone()]);
    assert!(!setup.spawned(), "the cache declares without spawning");
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let rows = classify_sync(vec![spec], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Shadow { .. }]));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a fresh start stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    assert!(
        !synced
            .tools
            .iter()
            .any(|(_, _, info)| info.name == crate::resources::RESOURCES_TOOL),
        "a dropped slot's resources offer declares nothing"
    );
    apply_sync(staged, &servers, &owned.inputs());
    setup.stop(servers);
}

#[test]
fn plan_declares_no_resources_for_a_keep_offering_none() {
    // The `:532` conjunction's other side: a kept live slot offering no
    // resources declares no `mcp_resources`; the mutant drops the kept
    // check and declares from the row alone.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Keep { .. }]));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("an unchanged keep stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert!(
        !synced
            .tools
            .iter()
            .any(|(_, _, info)| info.name == crate::resources::RESOURCES_TOOL),
        "a keep offering no resources declares none"
    );
    apply_sync(staged, &servers, &owned.inputs());
    setup.stop(servers);
}

#[test]
fn keep_redeclare_with_a_newly_disabled_tool_drops_it_and_keeps_the_new_spec() {
    // A declare-only change re-declares in place: the disabled tool
    // disappears from the planned set, the commit files the server as
    // kept on the same live slot, and the slot carries the new spec so
    // the next classify reads it.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}, {"name": "plus"}]));
    setup.result("echo", HI);
    setup.result("plus", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let pid = setup.pid();
    let mut changed = setup.spec("fx");
    changed.disabled = vec!["plus".to_owned()];
    let rows = classify_sync(vec![changed], &started.servers);
    assert!(
        matches!(rows.as_slice(), [SyncRow::Keep { .. }]),
        "a declare-only change keeps the server connected"
    );
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a keep stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert!(synced.servers.restarted.is_empty());
    let names: Vec<String> = synced
        .tools
        .iter()
        .map(|(_, _, info)| info.name.clone())
        .collect();
    assert_eq!(names, vec!["mcp__fx__echo".to_owned()]);
    apply_sync(staged, &servers, &owned.inputs());
    let kept = servers.slot("fx").expect("the kept slot");
    assert!(
        Arc::ptr_eq(&slot, &kept),
        "the keep restarts nothing: the slot is identical"
    );
    assert!(kept.is_live(), "the kept slot still serves");
    assert_eq!(
        kept.spec().disabled,
        vec!["plus".to_owned()],
        "the keep persists the new spec on the slot"
    );
    assert_eq!(setup.pid(), pid, "the keep spawns nothing");
    setup.stop(servers);
}

#[test]
fn keep_redeclare_with_a_newly_enabled_tool_adds_it_and_keeps_the_new_spec() {
    // The newly-enabled polarity: starting from an `enabled` allow-list
    // and reloading with the allow-list lifted declares the newly
    // enabled tool, still on the same live slot carrying the new spec.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}, {"name": "plus"}]));
    setup.result("echo", HI);
    setup.result("plus", HI);
    let mut spec = setup.spec("fx");
    spec.enabled = Some(vec!["echo".to_owned()]);
    let started = setup.start(vec![spec]);
    let slot = setup.running(&started);
    let pid = setup.pid();
    let changed = setup.spec("fx");
    assert_eq!(changed.enabled, None, "the reload lifts the allow-list");
    let rows = classify_sync(vec![changed], &started.servers);
    assert!(
        matches!(rows.as_slice(), [SyncRow::Keep { .. }]),
        "lifting the allow-list keeps the server connected"
    );
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a keep stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    let mut names: Vec<String> = synced
        .tools
        .iter()
        .map(|(_, _, info)| info.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["mcp__fx__echo".to_owned(), "mcp__fx__plus".to_owned()]
    );
    apply_sync(staged, &servers, &owned.inputs());
    let kept = servers.slot("fx").expect("the kept slot");
    assert!(
        Arc::ptr_eq(&slot, &kept),
        "the keep restarts nothing: the slot is identical"
    );
    assert_eq!(
        kept.spec().enabled,
        None,
        "the keep persists the lifted allow-list on the slot"
    );
    assert_eq!(setup.pid(), pid, "the keep spawns nothing");
    setup.stop(servers);
}

#[test]
fn keep_redeclare_with_a_changed_hint_shows_in_the_declared_effects() {
    // A hint-only change re-declares in place: the tool row carries the
    // person's override in its declared effects, and the slot carries
    // the new hints so the next classify reads them.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let mut changed = setup.spec("fx");
    changed.hints.insert(
        "echo".to_owned(),
        Hints {
            read_only: None,
            destructive: Some(false),
            open_world: None,
        },
    );
    let rows = classify_sync(vec![changed], &started.servers);
    assert!(
        matches!(rows.as_slice(), [SyncRow::Keep { .. }]),
        "a hint-only change keeps the server connected"
    );
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a keep stages");
    let owned = setup.owned_inputs(&images);
    let synced = plan_sync(&staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    let (_, tool, _) = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the staged lists still declare");
    let effects = tool.effects(&Default::default()).expect("classifiable");
    assert_eq!(
        effects.declared.effects,
        vec![
            contract::shapes::Effect::Writes,
            contract::shapes::Effect::Network
        ],
        "the re-declaration carries the person's override"
    );
    assert!(effects.declared.reversible);
    apply_sync(staged, &servers, &owned.inputs());
    let kept = servers.slot("fx").expect("the kept slot");
    assert!(
        Arc::ptr_eq(&slot, &kept),
        "the keep restarts nothing: the slot is identical"
    );
    assert_eq!(
        kept.spec().hints.get("echo"),
        Some(&Hints {
            read_only: None,
            destructive: Some(false),
            open_world: None,
        }),
        "the keep persists the new hints on the slot"
    );
    setup.stop(servers);
}
