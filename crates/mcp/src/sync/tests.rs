//! The reload sync's commit and classification through the public
//! stage and commit entries: keeps install staged lists, replacements
//! swap the slot, and failures file under the right quadrant.

use std::sync::Arc;
use std::sync::mpsc;

use contract::events::ServerFailure;
use contract::tool::ServerRecord;
use fakes::Deadline;
use serde_json::json;

use crate::test_support::FakeImages;
use crate::test_support::{Setup, WITHIN};
use crate::{SyncRow, classify_sync, commit_sync, stage_sync};

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
    let synced = commit_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "equal lists leave the file untouched"
    );
    setup.stop(servers);
}

#[test]
fn install_relisted_refuses_a_slot_whose_server_died() {
    // The install-time liveness revalidation: installing staged lists
    // on a slot whose server died keeps what the slot holds instead of
    // publishing lists its dead server never served, so the commit
    // cannot mark a dead keep as kept.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    setup.kill(&started);
    assert!(slot.is_running(), "the kill moves no slot");
    assert!(
        !slot.install_relisted(crate::cache::Cached {
            tools: vec![
                serde_json::from_value(json!({"name": "echo"})).expect("listed"),
                serde_json::from_value(json!({"name": "plus"})).expect("listed"),
            ],
            prompts: Vec::new(),
            resources: false,
        }),
        "a dead slot declines the install"
    );
    let live: Vec<String> = {
        let mut names: Vec<String> = slot.live_names().into_iter().collect();
        names.sort();
        names
    };
    assert_eq!(
        live,
        vec!["echo".to_owned()],
        "a dead slot keeps what it holds"
    );
    let listed: Vec<String> = slot
        .listed_tools()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(listed, vec!["echo".to_owned()]);
    setup.stop(started.servers);
}

#[test]
fn install_relisted_installs_on_a_live_slot() {
    // A live slot installs the re-list and answers true, so the commit's
    // cache write persists only lists a live server served.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    assert!(
        slot.install_relisted(crate::cache::Cached {
            tools: vec![
                serde_json::from_value(json!({"name": "echo"})).expect("listed"),
                serde_json::from_value(json!({"name": "plus"})).expect("listed"),
            ],
            prompts: Vec::new(),
            resources: false,
        }),
        "a live slot installs the re-list"
    );
    let live: Vec<String> = {
        let mut names: Vec<String> = slot.live_names().into_iter().collect();
        names.sort();
        names
    };
    assert_eq!(live, vec!["echo".to_owned(), "plus".to_owned()]);
    setup.stop(started.servers);
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
    let synced = commit_sync(staged, &servers, &owned.inputs());
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
    let synced = commit_sync(staged, &servers, &owned.inputs());
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
    let synced = commit_sync(staged, &servers, &owned.inputs());
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
    assert!(matches!(rows.as_slice(), [SyncRow::Replace { .. }]));
    let images = FakeImages::ok();
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("an optional shadow failure stages");
    let owned = setup.owned_inputs(&images);
    let synced = commit_sync(staged, &servers, &owned.inputs());
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
        let synced = commit_sync(staged, &servers, &owned.inputs());
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
    assert!(matches!(rows.as_slice(), [SyncRow::Start { .. }]));
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = fakes::Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a fresh start stages");
    let owned = setup.owned_inputs(&images);
    let synced = commit_sync(staged, &servers, &owned.inputs());
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
