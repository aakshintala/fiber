use super::*;
use extensions::{Quota, Window, WindowUse};
use serde_json::json;
use super::super::{Ack, Point};
use crate::switch::tests::{assembled, install_extension};
use std::sync::mpsc;
use std::path::{Path, PathBuf};
use fakes::{Deadline, MUST_SUCCEED_WITHIN};

fn receive<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(rx, what)
}

struct Fixture {
    _root: fakes::TempDir,
    switching: Arc<Switching>,
    counter: PathBuf,
    marker: PathBuf,
}

fn install(home: &Path, root: &Path, name: &str, quota: bool, counter: &Path) {
    let src = root.join(name);
    install_extension(root, name,
        json!({"name":name, "version":"v0.0.0", "fiber":"0.0.0", "api":1}),
        &[(name, json!({"name":name,"models":[{"id":"m", "protocol":"openai-responses", "base_url":"http://127.0.0.1:9/v1", "context_window":1000}]}))]);
    std::fs::create_dir_all(src.join("providers")).unwrap();
    let function = if quota { "quota = { timeout = 1000, run = function() return { windows = { {name = 'weekly', used_percent = 35} } } end }" } else { "" };
    std::fs::write(src.join(format!("providers/{name}.lua")), format!("local ok, seen = pcall(host.fs.read, '{counter}')\nif not ok or seen == nil then seen = '' end\nhost.fs.write('{counter}', seen .. 'x')\nfiber.provider('{name}', {{ {function} }})", counter=counter.display())).unwrap();
    extensions::plan(home, &extensions::Request::Path(src), "0.1.0", &extensions::Origin::github(), &*fakes::clock::FakeClock::new()).unwrap().commit().unwrap();
}

fn fixture(extra: bool, all_failed: bool, overrides: &[&str]) -> Fixture {
    let root = fakes::TempDir::new("fiber-quota-store");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let counter = root.path().join("qp-runs");
    let marker = root.path().join("nq-read");
    install(&home, root.path(), "qp", true, &counter);
    std::fs::create_dir_all(home.join("credentials/qp")).unwrap();
    for label in ["work", "home", "broken"] {
        std::fs::write(home.join(format!("credentials/qp/{label}")), if all_failed || label == "broken" { "" } else { "key" }).unwrap();
    }
    let mut options: Vec<String> = overrides.iter().map(|s| (*s).into()).collect();
    if extra {
        install(&home, root.path(), "empty", true, &root.path().join("empty-runs"));
        install(&home, root.path(), "nq", false, &root.path().join("nq-runs"));
        install(&home, root.path(), "zq", true, &root.path().join("zq-runs"));
        options.push(format!("providers.nq.credentials.work.command={}", json!(["sh", "-c", format!("echo ran > '{}'; echo key", marker.display())])));
        std::fs::create_dir_all(home.join("credentials/zq")).unwrap();
        std::fs::write(home.join("credentials/zq/work"), "key").unwrap();
    }
    let config = crate::test_support::load(&home, &workspace, "test", &options.iter().map(String::as_str).collect::<Vec<_>>());
    let switching = assembled(&home, config, BTreeMap::new(), &[]);
    Fixture { _root: root, switching, counter, marker }
}

fn load(store: &Arc<Store>) {
    let (tx, rx) = mpsc::channel();
    store.note_loaded(tx);
    assert!(store.answer().0.is_empty());
    receive(&rx, "the loader's end");
}

fn cached(store: &Arc<Store>) -> Vec<QuotaInfo> {
    let labels = store.state.lock().unwrap().labels.clone();
    for label in labels {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || tx.send(label.lua.quota(&label.pair, label.key.as_ref())).unwrap());
        assert!(matches!(receive(&rx, "the fetched value"), QuotaEntry::Reported(_)));
    }
    store.answer().0
}

#[test]
fn the_loader_keeps_successful_labels_orders_rows_and_never_reads_non_quota_keys() {
    let fixture = fixture(true, false, &["quota.notice_at=70"]);
    let store = fixture.switching.quota_store();
    load(&store);
    let nq = fixture.switching.started.started.lock().unwrap().get("nq").unwrap().clone();
    assert!(nq.upgrade().is_none(), "a provider without quota has no owner");
    assert!(!fixture.marker.exists(), "no key is read without quota");
    assert!(!fixture.switching.started.started.lock().unwrap().contains_key("empty"), "no label means no start");
    std::fs::write(fixture._root.path().join("home/credentials/qp/broken"), "repaired").unwrap();
    let lua = store.held_instance("qp");
    let weak = Arc::downgrade(&lua);
    drop(lua);
    drop(fixture.switching);
    assert!(weak.upgrade().is_some(), "the store owns quota providers");
    let rows = cached(&store);
    assert_eq!(rows.iter().map(|r| (r.provider.as_str(), r.credential.as_str())).collect::<Vec<_>>(), [("qp","broken"),("qp","home"),("qp","work"),("zq","work")]);
    assert_eq!(serde_json::to_value(&rows[0]).unwrap(), json!({"provider":"qp","credential":"broken","unreachable":true}));
    assert_eq!(store.answer().1, 70.0);
    assert_eq!(std::fs::read_to_string(&fixture.counter).unwrap(), "xx", "startup and one loader");
}

#[test]
fn all_failed_labels_drop_the_provider_and_keep_unreachable_answers() {
    let fixture = fixture(false, true, &[]);
    let store = fixture.switching.quota_store();
    load(&store);
    assert!(fixture.switching.started.started.lock().unwrap().get("qp").unwrap().upgrade().is_none());
    assert_eq!(store.answer().1, 80.0);
    assert_eq!(store.answer().0, [failed("qp", "broken"), failed("qp", "home"), failed("qp", "work")]);
}

#[test]
fn answers_do_not_wait_and_only_one_loader_starts() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    let (entered, at_labels) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (done, ended) = mpsc::channel();
    store.pause_at_labels(entered, released);
    store.note_loaded(done);
    assert!(store.answer().0.is_empty());
    receive(&at_labels, "the loader before label reads");
    let (tx, rx) = mpsc::channel();
    let second = store.clone();
    std::thread::spawn(move || tx.send(second.answer()).unwrap());
    assert!(receive(&rx, "the answer while loading").0.is_empty());
    release.send(()).unwrap();
    receive(&ended, "the loader's end");
    assert_eq!(cached(&store).len(), 3);
    assert_eq!(std::fs::read_to_string(&fixture.counter).unwrap(), "xx");
}

#[test]
fn cancellation_discards_a_label_read_in_flight() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    let (entered, at_labels) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (done, ended) = mpsc::channel();
    store.pause_at_labels(entered, released);
    store.note_loaded(done);
    assert!(store.answer().0.is_empty());
    receive(&at_labels, "the loader before label reads");
    fixture.switching.reads.cancel();
    release.send(()).unwrap();
    receive(&ended, "the cancelled loader's end");
    assert!(store.answer().0.is_empty());
}

#[test]
fn provider_start_and_registration_failures_are_unreachable_for_every_label() {
    for start_failure in [true, false] {
        let fixture = fixture(false, false, &[]);
        let dir = fixture.switching.loader.extensions.dirs().into_iter().find(|(name, _)| name == "qp").unwrap().1;
        if start_failure { std::fs::remove_file(dir.join("extension.json")).unwrap(); }
        else { std::fs::write(dir.join("providers/qp.lua"), "error('broken provider')").unwrap(); }
        let store = fixture.switching.quota_store();
        load(&store);
        assert_eq!(store.answer().0, [failed("qp", "broken"), failed("qp", "home"), failed("qp", "work")]);
        assert!(store.state.lock().unwrap().labels.is_empty());
    }
}

#[test]
fn a_reader_outliving_switching_starts_nothing() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    drop(fixture.switching);
    load(&store);
    assert!(store.answer().0.is_empty());
    assert_eq!(std::fs::read_to_string(&fixture.counter).unwrap(), "x");
}

fn ack(rx: &mpsc::Receiver<Ack>, point: Point, thread: &str) -> Ack {
    let ack = receive(rx, "the start-lock checkpoint");
    assert_eq!((&ack.point, ack.thread.as_str()), (&point, thread));
    ack
}

#[test]
fn a_switch_racing_the_loader_reuses_its_instance() {
    let fixture = fixture(false, false, &[]);
    let switching = fixture.switching.clone();
    let store = switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    let (tx, rx) = mpsc::channel();
    switching.started.pause_at_lock("qp", tx);
    assert!(store.answer().0.is_empty());
    ack(&rx, Point::P, "fiber-switch-read");
    // The test owns the only release sender: unwinding drops it and
    // releases the loader at Q, including when the next checkpoint fails.
    let loader_release = ack(&rx, Point::Q, "fiber-switch-read").release.unwrap();
    let (tx, got) = mpsc::channel();
    let other = switching.clone();
    let thread = std::thread::Builder::new().name("switch".into()).spawn(move || tx.send(other.read(other.want("qp", Some("work")))).unwrap()).unwrap();
    let switch_p = receive(&rx, "step 6: switch P before taking the start lock held by the loader at Q");
    assert_eq!((&switch_p.point, switch_p.thread.as_str()), (&Point::P, "switch"));
    loader_release.send(()).unwrap();
    let switch_release = ack(&rx, Point::Q, "switch").release.unwrap();
    switch_release.send(()).unwrap();
    let got = receive(&got, "the switch's result").unwrap();
    receive(&ended, "the loader's end");
    switching.started.unpause();
    let (joined, join) = mpsc::channel();
    std::thread::spawn(move || joined.send(thread.join()).unwrap());
    receive(&join, "the switch thread's join").unwrap();
    assert!(Arc::ptr_eq(&store.held_instance("qp"), got.access.lua.as_ref().unwrap()), "the switch reuses the store's instance");
    assert_eq!(std::fs::read_to_string(&fixture.counter).unwrap(), "xx");
}

#[test]
fn mapping_keeps_values_but_no_age_and_omits_empty_caches() {
    let window = Quota::Windows(vec![Window { name: "weekly".into(), used: WindowUse::Percent { used: 35.0, resets_at: None } }, Window { name: "daily".into(), used: WindowUse::Reset }]);
    for (entry, expected) in [
        (QuotaEntry::NoQuota, None),
        (QuotaEntry::Unreachable { last: None }, None),
        (QuotaEntry::Reported(Quota::Windows(vec![])), None),
        (QuotaEntry::Reported(window.clone()), Some(json!({"provider":"p", "credential":"work", "windows":[{"name":"weekly", "percent_used":35.0}, {"name":"daily", "reset":true}]}))),
        (QuotaEntry::Unreachable { last: Some((window, std::time::Duration::from_secs(999))) }, Some(json!({"provider":"p", "credential":"work", "windows":[{"name":"weekly", "percent_used":35.0}, {"name":"daily", "reset":true}], "unreachable":true}))),
        (QuotaEntry::Reported(Quota::Credit { remaining: 4.2, limit: None }), Some(json!({"provider":"p", "credential":"work", "credit":{"remaining":4.2}}))),
        (QuotaEntry::Reported(Quota::Credit { remaining: 4.2, limit: Some(10.0) }), Some(json!({"provider":"p", "credential":"work", "credit":{"remaining":4.2, "limit":10.0}}))),
    ] {
        assert_eq!(info("p", "work", entry).map(|value| serde_json::to_value(value).unwrap()), expected);
    }
}
