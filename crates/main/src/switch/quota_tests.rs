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
fn provider_start_and_registration_failures_are_unreachable_for_every_label() {
    for start_failure in [true, false] {
        let fixture = fixture(false, false, &[]);
        let dir = fixture
            .switching
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
fn mapping_keeps_values_but_no_age_and_omits_empty_caches() {
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
    CredentialPair {
        credential: "qp".to_owned(),
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
    assert!(store.entries().loading);
    receive(&ended, "the loader's end");
    // The fetch is still held open: no last value, still pending.
    let held = store.entries();
    assert!(!held.loading);
    assert_eq!(
        held.labels,
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
    let failed = store.entries();
    assert!(!failed.loading);
    assert_eq!(
        failed.labels,
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
    assert!(store.entries().loading);
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
        read_to_end(zq, pair_of("work")),
        QuotaEntry::Reported(weekly.clone())
    );
    let view = store.entries();
    assert!(!view.loading);
    let reported = |label: &str| LabelQuota {
        provider: "qp".to_owned(),
        credential: label.to_owned(),
        entry: QuotaEntry::Reported(weekly.clone()),
        pending: false,
    };
    assert_eq!(
        view.labels,
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
    assert!(store.entries().loading);
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
    assert!(!view.loading);
    assert_eq!(
        view.labels,
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
        [failed("zq", "work"), failed("qp", "work"), failed("qp", "home")]
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
fn the_model_view_never_reads_a_key_for_a_provider_without_quota() {
    let fixture = fixture(true, false, &[]);
    let store = fixture.switching.quota_store();
    let (done, ended) = mpsc::channel();
    store.note_loaded(done);
    assert!(store.entries().loading);
    receive(&ended, "the loader's end");
    assert!(
        !fixture.marker.exists(),
        "listing labels read a key for the provider without quota"
    );
    let view = store.entries();
    assert!(!view.loading);
    assert!(
        view.labels.iter().any(|row| row.provider == "nq"
            && row.credential == "work"
            && row.entry == QuotaEntry::NoQuota
            && !row.pending),
        "the silent label is listed without a key read: {:?}",
        view.labels
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
    assert!(first.loading);
    assert!(first.labels.is_empty());
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
    let second = store.entries();
    assert!(!second.loading);
    assert_eq!(second.labels.len(), 3);
    assert!(
        second.labels.iter().any(|row| row.provider == "qp"
            && row.credential == "work"
            && row.entry == QuotaEntry::Reported(weekly.clone())
            && !row.pending),
        "the loaded view lists the label's entry: {:?}",
        second.labels
    );
}

/// Reads the model view until the loader has checked every provider,
/// failing at the suite's deadline instead of spinning past it.
fn read_loaded(model: &ModelQuotaRead) -> QuotaView {
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    loop {
        let view = model();
        if !view.loading {
            return view;
        }
        assert!(
            !deadline.left().is_zero(),
            "the loader never checked every provider"
        );
    }
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
    assert!(model().loading);
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
    // `quota_readers` hands out the same pair: its model reader loads the
    // session's labels, and its doors reader lists them once the shared
    // fetches land.
    let (doors, model) = fixture.switching.quota_readers();
    let view = read_loaded(&model);
    assert_eq!(view.labels.len(), 3);
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
    let (_, model) = fixture.switching.quota_readers();
    drop(fixture.switching);
    // The loader thread runs, finds no session and ends it: no provider
    // ever starts, so the view stays loading and the counter never moves.
    let view = model();
    assert!(view.loading);
    assert!(view.labels.is_empty());
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
    assert!(store.entries().loading);
    receive(&ended, "the retried loader's end");
    assert!(store.state.lock().unwrap().started);
}
