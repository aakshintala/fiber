use super::super::{Ack, Point};
use super::*;
use crate::switch::tests::{assembled, install_extension};
use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::{CredentialPair, Quota, QuotaEntry, Window, WindowUse};
use fakes::{Deadline, MUST_SUCCEED_WITHIN};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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
    install_extension(
        root,
        name,
        json!({"name":name, "version":"v0.0.0", "fiber":"0.0.0", "api":1}),
        &[(
            name,
            json!({"name":name,"models":[{"id":"m", "protocol":"openai-responses", "base_url":"http://127.0.0.1:9/v1", "context_window":1000}]}),
        )],
    );
    std::fs::create_dir_all(src.join("providers")).unwrap();
    let function = if quota {
        "quota = { timeout = 1000, run = function() return { windows = { {name = 'weekly', used_percent = 35} } } end }"
    } else {
        ""
    };
    std::fs::write(src.join(format!("providers/{name}.lua")), format!("local ok, seen = pcall(host.fs.read, '{counter}')\nif not ok or seen == nil then seen = '' end\nhost.fs.write('{counter}', seen .. 'x')\nfiber.provider('{name}', {{ {function} }})", counter=counter.display())).unwrap();
    extensions::plan(
        home,
        &extensions::Request::Path(src),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
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
        std::fs::write(
            home.join(format!("credentials/qp/{label}")),
            if all_failed || label == "broken" {
                ""
            } else {
                "key"
            },
        )
        .unwrap();
    }
    let mut options: Vec<String> = overrides.iter().map(|s| (*s).into()).collect();
    if extra {
        install(
            &home,
            root.path(),
            "empty",
            true,
            &root.path().join("empty-runs"),
        );
        install(
            &home,
            root.path(),
            "nq",
            false,
            &root.path().join("nq-runs"),
        );
        install(&home, root.path(), "zq", true, &root.path().join("zq-runs"));
        options.push(format!(
            "providers.nq.credentials.work.command={}",
            json!([
                "sh",
                "-c",
                format!("echo ran > '{}'; echo key", marker.display())
            ])
        ));
        std::fs::create_dir_all(home.join("credentials/zq")).unwrap();
        std::fs::write(home.join("credentials/zq/work"), "key").unwrap();
    }
    let config = crate::test_support::load(
        &home,
        &workspace,
        "test",
        options.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let switching = assembled(&home, config, BTreeMap::new(), &[]);
    Fixture {
        _root: root,
        switching,
        counter,
        marker,
    }
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
        std::thread::spawn(move || {
            tx.send(label.lua.quota(&label.pair, label.key.as_ref()))
                .unwrap()
        });
        assert!(matches!(
            receive(&rx, "the fetched value"),
            QuotaEntry::Reported(_)
        ));
    }
    store.answer().0
}

#[test]
fn the_loader_keeps_successful_labels_orders_rows_and_never_reads_non_quota_keys() {
    let fixture = fixture(true, false, &["quota.notice_at=70"]);
    let store = fixture.switching.quota_store();
    load(&store);
    let nq = fixture
        .switching
        .started
        .started
        .lock()
        .unwrap()
        .get("nq")
        .unwrap()
        .clone();
    assert!(
        nq.upgrade().is_none(),
        "a provider without quota has no owner"
    );
    assert!(!fixture.marker.exists(), "no key is read without quota");
    assert!(
        !fixture
            .switching
            .started
            .started
            .lock()
            .unwrap()
            .contains_key("empty"),
        "no label means no start"
    );
    std::fs::write(
        fixture._root.path().join("home/credentials/qp/broken"),
        "repaired",
    )
    .unwrap();
    let lua = store.held_instance("qp");
    let weak = Arc::downgrade(&lua);
    drop(lua);
    drop(fixture.switching);
    assert!(weak.upgrade().is_some(), "the store owns quota providers");
    let rows = cached(&store);
    assert_eq!(
        rows.iter()
            .map(|r| (r.provider.as_str(), r.credential.as_str()))
            .collect::<Vec<_>>(),
        [
            ("qp", "broken"),
            ("qp", "home"),
            ("qp", "work"),
            ("zq", "work")
        ]
    );
    assert_eq!(
        serde_json::to_value(&rows[0]).unwrap(),
        json!({"provider":"qp","credential":"broken","unreachable":true})
    );
    assert_eq!(store.answer().1, 70.0);
    assert_eq!(
        std::fs::read_to_string(&fixture.counter).unwrap(),
        "xx",
        "startup and one loader"
    );
}

#[test]
fn all_failed_labels_drop_the_provider_and_keep_unreachable_answers() {
    let fixture = fixture(false, true, &[]);
    let store = fixture.switching.quota_store();
    load(&store);
    assert!(
        fixture
            .switching
            .started
            .started
            .lock()
            .unwrap()
            .get("qp")
            .unwrap()
            .upgrade()
            .is_none()
    );
    assert_eq!(store.answer().1, 80.0);
    assert_eq!(
        store.answer().0,
        [
            failed("qp", "broken"),
            failed("qp", "home"),
            failed("qp", "work")
        ]
    );
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
fn reload_refreshes_notice_threshold_for_an_existing_quota_store() {
    let fixture = fixture(false, false, &["quota.notice_at=0.25"]);
    let store = fixture.switching.quota_store();
    assert_eq!(store.answer_with_spawn(|_| Ok(())).1, 0.25);
    let mut now = fixture.switching.reloadable();
    now.config = Arc::new(crate::test_support::load(
        &fixture._root.path().join("home"),
        &fixture._root.path().join("workspace"),
        "test",
        ["quota.notice_at=0.75"],
    ));
    fixture.switching.reloaded(now);
    assert_eq!(store.answer_with_spawn(|_| Ok(())).1, 0.75);
}

#[test]
fn quota_load_uses_the_reloaded_set_and_startup_credential_labels() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    let config = crate::test_support::load(
        &fixture._root.path().join("home"),
        &fixture._root.path().join("workspace"),
        "test",
        ["extensions.qp.enabled=false"],
    );
    let mut now = fixture.switching.reloadable();
    let extensions = now.loader.extensions.reload(
        &fixture._root.path().join("home"),
        &config,
        now.loader.clock.clone(),
        now.loader.locks.clone(),
    );
    assert!(!extensions.has_extensions());
    now.loader.extensions = Arc::new(extensions);
    now.config = Arc::new(config);
    fixture.switching.reloaded(now);
    let held = store.clone();
    fakes::within(
        "quota loading from the current set",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || held.load(),
    );
    let rows = store.answer_with_spawn(|_| Ok(())).0;
    assert_eq!(
        rows.iter()
            .map(|row| (row.provider.as_str(), row.credential.as_str()))
            .collect::<Vec<_>>(),
        [("qp", "broken"), ("qp", "home"), ("qp", "work")]
    );
}

#[test]
fn provider_start_and_registration_failures_are_unreachable_for_every_label() {
    for start_failure in [true, false] {
        let fixture = fixture(false, false, &[]);
        let dir = fixture
            .switching
            .reloadable()
            .loader
            .extensions
            .dirs()
            .into_iter()
            .find(|(name, _)| name == "qp")
            .unwrap()
            .1;
        if start_failure {
            std::fs::remove_file(dir.join("extension.json")).unwrap();
        } else {
            std::fs::write(dir.join("providers/qp.lua"), "error('broken provider')").unwrap();
        }
        let store = fixture.switching.quota_store();
        load(&store);
        assert_eq!(
            store.answer().0,
            [
                failed("qp", "broken"),
                failed("qp", "home"),
                failed("qp", "work")
            ]
        );
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
    let thread = std::thread::Builder::new()
        .name("switch".into())
        .spawn(move || tx.send(other.read(other.want("qp", Some("work")))).unwrap())
        .unwrap();
    let switch_p = receive(
        &rx,
        "step 6: switch P before taking the start lock held by the loader at Q",
    );
    assert_eq!(
        (&switch_p.point, switch_p.thread.as_str()),
        (&Point::P, "switch")
    );
    loader_release.send(()).unwrap();
    let switch_release = ack(&rx, Point::Q, "switch").release.unwrap();
    switch_release.send(()).unwrap();
    let got = receive(&got, "the switch's result").unwrap();
    receive(&ended, "the loader's end");
    switching.started.unpause();
    let (joined, join) = mpsc::channel();
    std::thread::spawn(move || joined.send(thread.join()).unwrap());
    receive(&join, "the switch thread's join").unwrap();
    assert!(
        Arc::ptr_eq(&store.held_instance("qp"), got.access.lua.as_ref().unwrap()),
        "the switch reuses the store's instance"
    );
    assert_eq!(std::fs::read_to_string(&fixture.counter).unwrap(), "xx");
}

#[test]
fn the_notice_reader_reads_only_existing_caches() {
    let fixture = fixture(true, false, &[]);
    let store = fixture.switching.quota_store();
    let before = std::fs::read(&fixture.counter).ok();
    assert_eq!(store.cached(), (Vec::new(), 80.0));
    assert!(!store.state.lock().unwrap().started);
    assert_eq!(std::fs::read(&fixture.counter).ok(), before);
    let readers = fixture.switching.quota_readers();
    assert_eq!((readers.notice)(), (Vec::new(), 80.0));
    assert_eq!(std::fs::read(&fixture.counter).ok(), before);
    load(&store);
    let expected = cached(&store);
    assert!(!expected.is_empty());
    assert_eq!(store.cached(), (expected, 80.0));
    assert_eq!(store.cached(), store.answer());
}

#[test]
fn mapping_keeps_values_but_no_age_and_omits_empty_caches() {
    let reset_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000);
    let reset_window = Quota::Windows(vec![Window {
        name: "weekly".into(),
        used: WindowUse::Percent {
            used: 35.0,
            resets_at: Some(reset_time),
        },
    }]);
    assert_eq!(
        serde_json::to_value(info("p", "work", QuotaEntry::Reported(reset_window)).unwrap())
            .unwrap(),
        json!({"provider":"p", "credential":"work", "windows":[{"name":"weekly", "percent_used":35.0, "resets_at":1760000000000_u64}]}),
    );
    let window = Quota::Windows(vec![
        Window {
            name: "weekly".into(),
            used: WindowUse::Percent {
                used: 35.0,
                resets_at: None,
            },
        },
        Window {
            name: "daily".into(),
            used: WindowUse::Reset,
        },
    ]);
    for (entry, expected) in [
        (QuotaEntry::NoQuota, None),
        (QuotaEntry::Unreachable { last: None }, None),
        (QuotaEntry::Reported(Quota::Windows(vec![])), None),
        (
            QuotaEntry::Reported(window.clone()),
            Some(
                json!({"provider":"p", "credential":"work", "windows":[{"name":"weekly", "percent_used":35.0}, {"name":"daily", "reset":true}]}),
            ),
        ),
        (
            QuotaEntry::Unreachable {
                last: Some((window, std::time::Duration::from_secs(999))),
            },
            Some(
                json!({"provider":"p", "credential":"work", "windows":[{"name":"weekly", "percent_used":35.0}, {"name":"daily", "reset":true}], "unreachable":true}),
            ),
        ),
        (
            QuotaEntry::Reported(Quota::Credit {
                remaining: 4.2,
                limit: None,
            }),
            Some(json!({"provider":"p", "credential":"work", "credit":{"remaining":4.2}})),
        ),
        (
            QuotaEntry::Reported(Quota::Credit {
                remaining: 4.2,
                limit: Some(10.0),
            }),
            Some(
                json!({"provider":"p", "credential":"work", "credit":{"remaining":4.2, "limit":10.0}}),
            ),
        ),
    ] {
        assert_eq!(
            info("p", "work", entry).map(|value| serde_json::to_value(value).unwrap()),
            expected
        );
    }
}

/// A quota fetch that asks `<server>/<label>` over HTTP, so a test can hold
/// its flight open on a held server and land it with a release.
fn install_http(home: &Path, root: &Path, server: &str) {
    let name = "qp";
    let src = root.join(name);
    install_extension(
        root,
        name,
        json!({"name":name, "version":"v0.0.0", "fiber":"0.0.0", "api":1}),
        &[(
            name,
            json!({"name":name,"models":[{"id":"m", "protocol":"openai-responses", "base_url":"http://127.0.0.1:9/v1", "context_window":1000}]}),
        )],
    );
    std::fs::create_dir_all(src.join("providers")).unwrap();
    std::fs::write(
        src.join("providers/qp.lua"),
        format!(
            "fiber.provider('qp', {{ quota = {{ timeout = 60000, run = function(a)\n\
             local reply = host.http({{ url = \"{server}/\" .. a.label }})\n\
             if reply.status ~= 200 then error(\"denied \" .. reply.status) end\n\
             return json.decode(reply.body)\n\
             end }} }})"
        ),
    )
    .unwrap();
    extensions::plan(
        home,
        &extensions::Request::Path(src),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

struct HttpFixture {
    _root: fakes::TempDir,
    switching: Arc<Switching>,
    server: fakes::ProviderServer,
}

fn http_fixture(responses: Vec<fakes::Response>) -> HttpFixture {
    let root = fakes::TempDir::new("fiber-quota-model-view");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let server = fakes::ProviderServer::start(responses).unwrap();
    install_http(&home, root.path(), &server.url());
    std::fs::create_dir_all(home.join("credentials/qp")).unwrap();
    std::fs::write(home.join("credentials/qp/work"), "key").unwrap();
    let config = crate::test_support::load(&home, &workspace, "test", Vec::<String>::new());
    let switching = assembled(&home, config, BTreeMap::new(), &[]);
    HttpFixture {
        _root: root,
        switching,
        server,
    }
}

fn quota_value() -> Quota {
    Quota::Windows(vec![Window {
        name: "primary".to_owned(),
        used: WindowUse::Percent {
            used: 42.5,
            resets_at: None,
        },
    }])
}

fn windows_body() -> Vec<u8> {
    b"{\"windows\": [{\"name\": \"primary\", \"used_percent\": 42.5}]}".to_vec()
}

fn denied() -> fakes::Response {
    fakes::Response::status(500, "{\"error\": \"denied\"}")
}

/// Reads `pair` to its fetch's end on a thread, failing at the suite's
/// deadline instead of parking past it.
fn read_to_end(lua: Arc<extensions::LuaProvider>, pair: CredentialPair) -> QuotaEntry {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(lua.quota(&pair, None)));
    receive(&rx, "the fetch's end")
}

fn pair_of(label: &str) -> CredentialPair {
    pair_in("qp", label)
}

fn pair_in(credential: &str, label: &str) -> CredentialPair {
    CredentialPair {
        credential: credential.to_owned(),
        label: label.to_owned(),
    }
}

#[test]
fn a_label_whose_first_fetch_is_held_open_is_pending() {
    let fixture = http_fixture(vec![denied()]);
    fixture.server.hold();
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    // The fetch is still held open: no last value, still pending.
    let QuotaView::Loaded(held) = store.entries() else {
        panic!("the held fetch still lists its label");
    };
    assert_eq!(
        held,
        [LabelQuota {
            provider: "qp".to_owned(),
            credential: "work".to_owned(),
            entry: QuotaEntry::Unreachable { last: None },
            pending: true,
        }]
    );
    // Landing the failed fetch clears the pending flag, keeping
    // `unreachable` with no last value.
    fixture.server.release();
    let lua = store.held_instance("qp");
    assert_eq!(
        read_to_end(Arc::clone(&lua), pair_of("work")),
        QuotaEntry::Unreachable { last: None }
    );
    let QuotaView::Loaded(failed) = store.entries() else {
        panic!("the failed fetch still lists unreachable");
    };
    assert_eq!(
        failed,
        [LabelQuota {
            provider: "qp".to_owned(),
            credential: "work".to_owned(),
            entry: QuotaEntry::Unreachable { last: None },
            pending: false,
        }]
    );
}

#[test]
fn the_model_view_lists_a_no_quota_provider_and_an_unreachable_label_with_no_last_value() {
    let fixture = fixture(true, false, &[]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    // The fetches land at once: the reads below wait for each held label's
    // own flight, so the view after them is deterministic.
    let lua = store.held_instance("qp");
    let weekly = Quota::Windows(vec![Window {
        name: "weekly".to_owned(),
        used: WindowUse::Percent {
            used: 35.0,
            resets_at: None,
        },
    }]);
    for label in ["work", "home"] {
        assert_eq!(
            read_to_end(Arc::clone(&lua), pair_of(label)),
            QuotaEntry::Reported(weekly.clone()),
            "{label}"
        );
    }
    let zq = store.held_instance("zq");
    assert_eq!(
        read_to_end(zq, pair_in("zq", "work")),
        QuotaEntry::Reported(weekly.clone())
    );
    let QuotaView::Loaded(view) = store.entries() else {
        panic!("the loaded view lists every label");
    };
    // The render sorts the rows; the view only owes the row set.
    let mut rows = view;
    rows.sort_by(|a, b| (&a.provider, &a.credential).cmp(&(&b.provider, &b.credential)));
    let reported = |label: &str| LabelQuota {
        provider: "qp".to_owned(),
        credential: label.to_owned(),
        entry: QuotaEntry::Reported(weekly.clone()),
        pending: false,
    };
    assert_eq!(
        rows,
        [
            LabelQuota {
                provider: "nq".to_owned(),
                credential: "work".to_owned(),
                entry: QuotaEntry::NoQuota,
                pending: false,
            },
            LabelQuota {
                provider: "qp".to_owned(),
                credential: "broken".to_owned(),
                entry: QuotaEntry::Unreachable { last: None },
                pending: false,
            },
            reported("home"),
            reported("work"),
            LabelQuota {
                provider: "zq".to_owned(),
                credential: "work".to_owned(),
                entry: QuotaEntry::Reported(weekly),
                pending: false,
            },
        ]
    );
}

#[test]
fn a_label_whose_fetch_lands_between_the_two_reads_shows_its_value() {
    let fixture = http_fixture(vec![fakes::Response::status(200, windows_body())]);
    fixture.server.hold();
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    // Hold the view between its `quota_pending` read and its `quota_now`
    // read, so the fetch landing there is deterministic.
    let (entered, at_read) = mpsc::channel();
    let (release, released) = mpsc::channel();
    store.pause_between_reads(entered, released);
    let (found, got) = mpsc::channel();
    let reader = store.clone();
    std::thread::spawn(move || found.send(reader.entries()));
    receive(&at_read, "the view between its two reads");
    fixture.server.release();
    let lua = store.held_instance("qp");
    assert_eq!(
        read_to_end(lua, pair_of("work")),
        QuotaEntry::Reported(quota_value())
    );
    release.send(()).unwrap();
    let view = receive(&got, "the view past its second read");
    let QuotaView::Loaded(view) = view else {
        panic!("the fetch landing between the reads shows its value");
    };
    assert_eq!(
        view,
        [LabelQuota {
            provider: "qp".to_owned(),
            credential: "work".to_owned(),
            entry: QuotaEntry::Reported(quota_value()),
            pending: true,
        }]
    );
}

#[test]
fn a_failed_provider_read_keeps_the_other_providers_labels() {
    let mut state = Held::default();
    state.failed.push(failed("zq", "work"));
    let refused = |code| Failure {
        code,
        message: "the read failed".to_owned(),
        retry_after_ms: None,
        provider: None,
    };
    // A failed read records that provider's labels as failed and continues.
    let (keep_going, held) = apply_outcome(
        &mut state,
        "qp",
        &["work".to_owned(), "home".to_owned()],
        Err(refused(ErrorCode::IoFailed)),
    );
    assert!(keep_going);
    assert!(held.is_empty());
    assert_eq!(
        state.failed,
        [
            failed("zq", "work"),
            failed("qp", "work"),
            failed("qp", "home")
        ]
    );
    // A shutdown stops the loader instead.
    let (keep_going, _) = apply_outcome(
        &mut state,
        "qp",
        &["work".to_owned()],
        Err(refused(ErrorCode::Closing)),
    );
    assert!(!keep_going);
}

#[test]
fn a_failed_provider_read_through_load_lists_unreachable_and_still_loads() {
    let fixture = fixture(true, false, &[]);
    let store = fixture.switching.quota_store();
    store.fail_reads_for("qp");
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    // The read below waits for zq's held label's own flight, so the view
    // after it is deterministic.
    let zq = store.held_instance("zq");
    let weekly = Quota::Windows(vec![Window {
        name: "weekly".to_owned(),
        used: WindowUse::Percent {
            used: 35.0,
            resets_at: None,
        },
    }]);
    assert_eq!(
        read_to_end(zq, pair_in("zq", "work")),
        QuotaEntry::Reported(weekly.clone()),
    );
    // The failed provider's labels show unreachable with no last value
    // and no pending flag, while the other providers still list.
    let QuotaView::Loaded(view) = store.entries() else {
        panic!("a non-closing read error still leaves loading false");
    };
    for label in ["work", "home", "broken"] {
        assert!(
            view.iter().any(|row| row.provider == "qp"
                && row.credential == label
                && row.entry == QuotaEntry::Unreachable { last: None }
                && !row.pending),
            "the failed provider's labels are unreachable: {view:?}",
        );
    }
    assert!(
        view.iter().any(|row| row.provider == "zq"
            && row.credential == "work"
            && row.entry == QuotaEntry::Reported(weekly.clone())
            && !row.pending),
        "the other providers still list: {view:?}",
    );
    assert!(
        view.iter().any(|row| row.provider == "nq"
            && row.credential == "work"
            && row.entry == QuotaEntry::NoQuota
            && !row.pending),
        "the provider without quota still lists: {view:?}",
    );
}

#[test]
fn the_model_view_never_reads_a_key_for_a_provider_without_quota() {
    let fixture = fixture(true, false, &[]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    assert!(
        !fixture.marker.exists(),
        "listing labels read a key for the provider without quota"
    );
    let QuotaView::Loaded(view) = store.entries() else {
        panic!("the loaded view lists every label");
    };
    assert!(
        view.iter().any(|row| row.provider == "nq"
            && row.credential == "work"
            && row.entry == QuotaEntry::NoQuota
            && !row.pending),
        "the silent label is listed without a key read: {:?}",
        view
    );
}

#[test]
fn the_model_view_is_loading_until_the_loader_has_checked_every_provider() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    let (entered, at_register) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (done, ended) = mpsc::channel();
    store.pause_at_register(entered, released);
    store.note_loaded(done);
    let (found, got) = mpsc::channel();
    let reader = store.clone();
    std::thread::spawn(move || found.send(reader.entries()));
    // The loader is still held before its quota check: the view above
    // already returned, without waiting for it.
    receive(&at_register, "the loader before the quota check");
    let first = receive(&got, "the model view while loading");
    assert!(matches!(first, QuotaView::Loading));
    release.send(()).unwrap();
    receive(&ended, "the loader's end");
    let lua = store.held_instance("qp");
    let weekly = Quota::Windows(vec![Window {
        name: "weekly".to_owned(),
        used: WindowUse::Percent {
            used: 35.0,
            resets_at: None,
        },
    }]);
    for label in ["work", "home"] {
        assert_eq!(
            read_to_end(Arc::clone(&lua), pair_of(label)),
            QuotaEntry::Reported(weekly.clone()),
            "{label}"
        );
    }
    let QuotaView::Loaded(second) = store.entries() else {
        panic!("the released loader lists the label's entry");
    };
    assert_eq!(second.len(), 3);
    assert!(
        second.iter().any(|row| row.provider == "qp"
            && row.credential == "work"
            && row.entry == QuotaEntry::Reported(weekly.clone())
            && !row.pending),
        "the loaded view lists the label's entry: {:?}",
        second
    );
}

#[test]
fn both_readers_share_one_loader() {
    let fixture = fixture(false, false, &[]);
    // Readers over one store share its start claim: the second never
    // spawns, counted through the doors reader's spawn step.
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    let (entered, at_labels) = mpsc::channel();
    let (release, released) = mpsc::channel();
    store.pause_at_labels(entered, released);
    let reader = store.clone();
    let model = move || reader.entries();
    assert!(matches!(model(), QuotaView::Loading));
    receive(&at_labels, "the loader before label reads");
    let starts = std::sync::atomic::AtomicUsize::new(0);
    let second = store.clone();
    let doors = || {
        second.answer_with_spawn(|loaded| {
            starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            loaded.load();
            Ok(())
        })
    };
    // The loader is still held: the second reader answers at once without
    // starting another loader.
    assert!(doors().0.is_empty());
    assert_eq!(
        starts.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the second reader started another loader"
    );
    release.send(()).unwrap();
    receive(&ended, "the loader's end");
    // Both readers over one store: the model reader loads the session's
    // labels, and the doors reader lists them once the shared fetches
    // land. The store behind them carries the test's loaded note, so the
    // test waits on the loader's end instead of spinning on the view.
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    let doors = {
        let shared = Arc::clone(&store);
        move || shared.answer()
    };
    let model = {
        let shared = Arc::clone(&store);
        move || shared.entries()
    };
    assert!(matches!(model(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    let QuotaView::Loaded(view) = model() else {
        panic!("the shared loader lists the session's labels");
    };
    assert_eq!(view.len(), 3);
    let lua = fixture.switching.started.started.lock().unwrap()["qp"]
        .upgrade()
        .unwrap();
    let weekly = Quota::Windows(vec![Window {
        name: "weekly".to_owned(),
        used: WindowUse::Percent {
            used: 35.0,
            resets_at: None,
        },
    }]);
    for label in ["work", "home"] {
        assert_eq!(
            read_to_end(Arc::clone(&lua), pair_of(label)),
            QuotaEntry::Reported(weekly.clone()),
            "{label}"
        );
    }
    assert_eq!(doors().0.len(), 3);
}

#[test]
fn a_model_reader_outliving_switching_starts_nothing() {
    let fixture = fixture(false, false, &[]);
    let before = std::fs::read_to_string(&fixture.counter).unwrap();
    let QuotaReaders { model, .. } = fixture.switching.quota_readers();
    drop(fixture.switching);
    // The loader thread runs, finds no session and ends it: no provider
    // ever starts, so the view stays loading and the counter never moves.
    let view = model();
    assert!(matches!(view, QuotaView::Loading));
    assert_eq!(std::fs::read_to_string(&fixture.counter).unwrap(), before);
}

#[test]
fn a_failed_loader_spawn_is_retried_by_the_model_reader() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    store.snapshot_starting(|_| Err(std::io::Error::other("spawn refused")));
    assert!(!store.state.lock().unwrap().started);
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    // The model reader retries the failed spawn with a real loader, which
    // runs to its end.
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the retried loader's end");
    assert!(store.state.lock().unwrap().started);
}

#[test]
fn the_door_reader_fetches_only_with_the_flag() {
    let fixture = http_fixture(vec![
        fakes::Response::status(200, windows_body()),
        fakes::Response::status(
            200,
            b"{\"windows\": [{\"name\": \"primary\", \"used_percent\": 11.0}]}".to_vec(),
        ),
    ]);
    // The readers build their own store, as `Switching::quota_readers`
    // wires them: the flag on the door reader selects the path.
    let readers = fixture.switching.quota_readers();
    let fetch = Arc::clone(&readers.door);
    let (rows, notice_at) = fakes::within(
        "the door reader's fetch answer",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || fetch(true),
    );
    assert_eq!(fixture.server.requests().len(), 1, "no fetch started");
    assert_eq!(notice_at, 80.0);
    assert_eq!(
        rows,
        [info("qp", "work", QuotaEntry::Reported(quota_value())).unwrap()]
    );
    let cached = Arc::clone(&readers.door);
    let (rows, _) = fakes::within(
        "the door reader's cached answer",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || cached(false),
    );
    assert_eq!(fixture.server.requests().len(), 1, "a fetch started");
    assert_eq!(
        rows,
        [info("qp", "work", QuotaEntry::Reported(quota_value())).unwrap()]
    );
    let refetch = Arc::clone(&readers.door);
    let (rows, _) = fakes::within(
        "the door reader's refetch answer",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || refetch(true),
    );
    assert_eq!(fixture.server.requests().len(), 2, "no fetch started");
    assert_eq!(
        rows,
        [info(
            "qp",
            "work",
            QuotaEntry::Reported(Quota::Windows(vec![Window {
                name: "primary".to_owned(),
                used: WindowUse::Percent {
                    used: 11.0,
                    resets_at: None,
                },
            }])),
        )
        .unwrap()]
    );
}

#[test]
fn fetch_true_starts_a_fetch_on_a_fresh_cache() {
    let fixture = http_fixture(vec![
        fakes::Response::status(200, windows_body()),
        fakes::Response::status(
            200,
            b"{\"windows\": [{\"name\": \"primary\", \"used_percent\": 11.0}]}".to_vec(),
        ),
    ]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    let first = QuotaEntry::Reported(quota_value());
    let lua = store.held_instance("qp");
    assert_eq!(read_to_end(Arc::clone(&lua), pair_of("work")), first);
    assert_eq!(fixture.server.requests().len(), 1);
    // The cache is fresh: no clock ever moved, so its age is far below the
    // 5-minute freshness bound and a due-check would never refetch. (The
    // providers' clocks live inside the fixture, so the test cannot advance
    // one; the frozen clock keeps the primed cache fresh by itself.) The
    // fetch path still starts one fetch per label and answers its value.
    let fetch = Arc::clone(&store);
    let (rows, notice_at) = fakes::within(
        "the fetch path's answer",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || fetch.answer_fetch(),
    );
    assert_eq!(fixture.server.requests().len(), 2, "no fetch started");
    assert_eq!(notice_at, 80.0);
    assert_eq!(
        rows,
        [info(
            "qp",
            "work",
            QuotaEntry::Reported(Quota::Windows(vec![Window {
                name: "primary".to_owned(),
                used: WindowUse::Percent {
                    used: 11.0,
                    resets_at: None,
                },
            }])),
        )
        .unwrap()]
    );
}

#[test]
fn without_fetch_the_answer_comes_from_cache_with_no_new_fetch() {
    let fixture = http_fixture(vec![fakes::Response::status(200, windows_body())]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    let lua = store.held_instance("qp");
    assert_eq!(
        read_to_end(Arc::clone(&lua), pair_of("work")),
        QuotaEntry::Reported(quota_value())
    );
    assert_eq!(fixture.server.requests().len(), 1);
    let cached = Arc::clone(&store);
    let (rows, notice_at) = fakes::within(
        "the cached answer",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || cached.answer(),
    );
    assert_eq!(fixture.server.requests().len(), 1, "a fetch started");
    assert_eq!(notice_at, 80.0);
    assert_eq!(
        rows,
        [info("qp", "work", QuotaEntry::Reported(quota_value())).unwrap()]
    );
}

#[test]
fn a_failed_fetch_without_a_last_value_answers_unreachable() {
    let fixture = http_fixture(vec![denied(), denied()]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    // Nothing ever fetched, so the failed fetch has no last value: the
    // row is `unreachable` with no value, through the failure mapper.
    let failed_fetch = Arc::clone(&store);
    assert_eq!(
        fakes::within(
            "the failed fetch's answer",
            Deadline::after(MUST_SUCCEED_WITHIN),
            move || failed_fetch.answer_fetch()
        ),
        (vec![failed("qp", "work")], 80.0)
    );
}

#[test]
fn a_failed_fetch_with_a_last_value_answers_unreachable_with_it() {
    let fixture = http_fixture(vec![fakes::Response::status(200, windows_body()), denied()]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    let lua = store.held_instance("qp");
    assert_eq!(
        read_to_end(Arc::clone(&lua), pair_of("work")),
        QuotaEntry::Reported(quota_value())
    );
    // The retry fails, keeping the last value: the row carries it with
    // `unreachable`, through the value mapper.
    let failed_fetch = Arc::clone(&store);
    assert_eq!(
        fakes::within(
            "the failed fetch's answer",
            Deadline::after(MUST_SUCCEED_WITHIN),
            move || failed_fetch.answer_fetch()
        ),
        (
            vec![
                info(
                    "qp",
                    "work",
                    QuotaEntry::Unreachable {
                        last: Some((quota_value(), Duration::from_secs(0))),
                    },
                )
                .unwrap()
            ],
            80.0
        )
    );
}

#[test]
fn fetch_true_waits_for_a_held_loader_then_answers() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    // A test-owned fake loader clock: its parks name the instants the
    // waits below synchronize on.
    let loader_clock = fakes::clock::FakeClock::new();
    let mut now = fixture.switching.reloadable();
    now.loader.clock = loader_clock.clone();
    fixture.switching.reloaded(now);
    let (entered, at_register) = mpsc::channel();
    let (release, released) = mpsc::channel();
    store.pause_at_register(entered, released);
    let (found, got) = mpsc::channel();
    let reader = store.clone();
    std::thread::spawn(move || found.send(reader.answer_fetch()));
    receive(&at_register, "the loader before the quota check");
    // The loader is held before resolving any label: the fetch path parks
    // on the loader wait instead of answering the empty snapshot as
    // complete. The parked thread proves the reader reached its wait, and
    // the held loader proves no answer can arrive before the release:
    // no real-time wait is needed.
    let end = loader_clock
        .now()
        .checked_add(LOADER_BUDGET)
        .expect("the loader budget fits the clock");
    assert!(
        loader_clock.await_parked(end, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the fetch path parked on the loader wait"
    );
    release.send(()).unwrap();
    let (rows, notice_at) = receive(&got, "the fetch path's answer");
    assert_eq!(notice_at, 80.0);
    let weekly = Quota::Windows(vec![Window {
        name: "weekly".to_owned(),
        used: WindowUse::Percent {
            used: 35.0,
            resets_at: None,
        },
    }]);
    assert_eq!(
        rows,
        [
            failed("qp", "broken"),
            info("qp", "home", QuotaEntry::Reported(weekly.clone())).unwrap(),
            info("qp", "work", QuotaEntry::Reported(weekly)).unwrap(),
        ]
    );
}

#[test]
fn fetch_true_answers_unreachable_when_the_loader_budget_expires() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    // A test-owned fake loader clock: its parks name the instants the
    // waits below synchronize on.
    let loader_clock = fakes::clock::FakeClock::new();
    let mut now = fixture.switching.reloadable();
    now.loader.clock = loader_clock.clone();
    fixture.switching.reloaded(now);
    let (entered, at_register) = mpsc::channel();
    let (release, released) = mpsc::channel();
    store.pause_at_register(entered, released);
    let (found, got) = mpsc::channel();
    let reader = store.clone();
    std::thread::spawn(move || found.send(reader.answer_fetch()));
    receive(&at_register, "the loader before the quota check");
    // Past the budget on the loader clock, which the waiter reads: no
    // label resolved, so every startup label answers `unreachable`. The
    // parked thread proves the reader reached its wait, and the advance
    // names that park, never a sleep; reaching the deadline is enough,
    // since the waiter's own check is `now >= end`.
    let end = loader_clock
        .now()
        .checked_add(LOADER_BUDGET)
        .expect("the loader budget fits the clock");
    assert!(
        loader_clock.await_parked(end, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the fetch path parked on the loader wait"
    );
    loader_clock.advance_when_parked(
        Some(end),
        None,
        LOADER_BUDGET,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        receive(&got, "the expired budget's answer"),
        (
            vec![
                failed("qp", "broken"),
                failed("qp", "home"),
                failed("qp", "work"),
            ],
            80.0
        )
    );
    release.send(()).unwrap();
}

#[test]
fn fetch_true_answers_unreachable_when_shutdown_fires_while_unchecked() {
    let fixture = fixture(false, false, &[]);
    let store = fixture.switching.quota_store();
    drop(fixture.switching);
    // The loader finds no session and ends it unchecked: every startup
    // label answers `unreachable` at once, never partial and never empty
    // as complete.
    let unreachable = Arc::clone(&store);
    assert_eq!(
        fakes::within(
            "the shutdown fetch's answer",
            Deadline::after(MUST_SUCCEED_WITHIN),
            move || unreachable.answer_fetch()
        ),
        (
            vec![
                failed("qp", "broken"),
                failed("qp", "home"),
                failed("qp", "work"),
            ],
            80.0
        )
    );
}

#[test]
fn a_fetched_no_quota_stays_absent() {
    let fixture = http_fixture(vec![
        fakes::Response::status(200, b"null".to_vec()),
        fakes::Response::status(200, b"null".to_vec()),
    ]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(matches!(store.entries(), QuotaView::Loading));
    receive(&ended, "the loader's end");
    // The loader ends without waiting for its fetch flight, so the
    // forced fetch below would join that flight or start a second
    // request depending on timing. Waiting for the flight to land first
    // keeps both requests deterministic: one here, one in the fetch.
    let lua = store.held_instance("qp");
    assert_eq!(
        read_to_end(Arc::clone(&lua), pair_of("work")),
        QuotaEntry::NoQuota
    );
    assert_eq!(fixture.server.requests().len(), 1);
    // The forced fetch resolves no quota: the label stays absent, as in
    // the cached path, instead of answering `unreachable`.
    let empty = Arc::clone(&store);
    assert_eq!(
        fakes::within(
            "the empty fetch's answer",
            Deadline::after(MUST_SUCCEED_WITHIN),
            move || { empty.answer_fetch() }
        ),
        (Vec::new(), 80.0)
    );
    assert_eq!(fixture.server.requests().len(), 2);
}
