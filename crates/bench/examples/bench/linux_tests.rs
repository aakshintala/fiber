use std::collections::BTreeMap;

use serde_json::json;

use super::{
    Counts, RETAINED_SETTLE, comm, hub_connected, idle_switches, retained_anon_kib, rss_anon_kib,
    rss_split_kib, switches, unsettled, vm_hwm_kib,
};

const STATUS: &str = "Name:\tfiber\nState:\tS (sleeping)\nVmHWM:\t   10840 kB\nVmRSS:\t   10812 kB\nRssAnon:\t    4100 kB\nRssFile:\t    4800 kB\nThreads:\t5\nvoluntary_ctxt_switches:\t12\nnonvoluntary_ctxt_switches:\t3\n";

#[test]
fn vm_hwm_is_read_in_kib() {
    assert_eq!(vm_hwm_kib(STATUS), Ok(10840));
}

#[test]
fn the_rss_split_is_anon_and_file_in_kib() {
    assert_eq!(rss_split_kib(STATUS), Ok((4100, 4800)));
}

#[test]
fn a_status_without_the_split_errors_naming_the_missing_field() {
    let without_anon = STATUS.replace("RssAnon:\t    4100 kB\n", "");
    assert!(
        rss_split_kib(&without_anon)
            .unwrap_err()
            .contains("RssAnon"),
        "{without_anon:?}"
    );
    let without_file = STATUS.replace("RssFile:\t    4800 kB\n", "");
    assert!(
        rss_split_kib(&without_file)
            .unwrap_err()
            .contains("RssFile"),
        "{without_file:?}"
    );
}

#[test]
fn rss_anon_is_the_anonymous_part_in_kib() {
    assert_eq!(rss_anon_kib(STATUS), Ok(4100));
    let without_anon = STATUS.replace("RssAnon:\t    4100 kB\n", "");
    assert!(
        rss_anon_kib(&without_anon)
            .unwrap_err()
            .contains("RssAnon"),
        "{without_anon:?}"
    );
    // A file-backed split without the anonymous part is still an error,
    // never the file half: the retained budget gates the anon half.
    let anon_as_file = STATUS.replace("RssAnon:", "RssAnonOld:");
    assert!(rss_anon_kib(&anon_as_file).is_err());
}

/// The settle waits `sleeps` records, and the order `events` saw.
#[derive(Default)]
struct RetainedRecorder {
    sleeps: Vec<std::time::Duration>,
    events: Vec<&'static str>,
}

#[test]
fn the_retained_reading_waits_the_settle_then_reads_rss_anon() {
    let recorder = std::cell::RefCell::new(RetainedRecorder::default());
    let anon = retained_anon_kib(
        || {
            recorder.borrow_mut().events.push("read");
            Ok(STATUS.to_owned())
        },
        |wait| {
            let mut recorder = recorder.borrow_mut();
            recorder.sleeps.push(wait);
            recorder.events.push("sleep");
        },
    );
    assert_eq!(anon, Ok(4100));
    let recorder = recorder.borrow();
    // Exactly one settle wait, of ten seconds.
    assert_eq!(recorder.sleeps, vec![RETAINED_SETTLE]);
    assert_eq!(RETAINED_SETTLE, std::time::Duration::from_secs(10));
    // The read follows the wait, so the settled memory is read.
    assert_eq!(recorder.events, vec!["sleep", "read"]);
}

#[test]
fn a_retained_reading_without_rss_anon_is_an_error_not_zero() {
    let without_anon = STATUS.replace("RssAnon:\t    4100 kB\n", "");
    let mut sleeps = 0;
    let anon = retained_anon_kib(
        || Ok(without_anon.clone()),
        |_| {
            sleeps += 1;
        },
    );
    let err = anon.unwrap_err();
    assert!(err.contains("RssAnon"), "{err}");
    assert_eq!(sleeps, 1);
}

#[test]
fn a_process_gone_during_the_settle_wait_fails_the_reading() {
    let mut sleeps = 0;
    let anon = retained_anon_kib(
        || Err("reading /proc/4101/status: no such file".to_owned()),
        |_| {
            sleeps += 1;
        },
    );
    assert!(anon.is_err());
    // The wait still ran: the process lived through the settle.
    assert_eq!(sleeps, 1);
}

#[test]
fn switches_are_the_voluntary_and_involuntary_counts() {
    assert_eq!(
        switches(STATUS),
        Ok(Counts {
            voluntary: 12,
            involuntary: 3,
            name: None,
            state: 'S',
        })
    );
}

#[test]
fn the_state_is_the_first_character_of_its_value() {
    for (value, state) in [("R (running)", 'R'), ("D (disk sleep)", 'D')] {
        let status = STATUS.replace("S (sleeping)", value);
        assert_eq!(switches(&status).map(|counts| counts.state), Ok(state));
    }
}

#[test]
fn a_missing_or_empty_state_is_an_error() {
    let without = STATUS.replace("State:\tS (sleeping)\n", "");
    assert!(switches(&without).unwrap_err().contains("State"));
    let empty = STATUS.replace("State:\tS (sleeping)\n", "State:\t\n");
    assert!(switches(&empty).unwrap_err().contains("State"));
}

#[test]
fn a_missing_field_is_an_error_not_zero() {
    let without = STATUS.replace("VmHWM:\t   10840 kB\n", "");
    assert!(vm_hwm_kib(&without).unwrap_err().contains("VmHWM"));
    let without = STATUS.replace("nonvoluntary_ctxt_switches:\t3\n", "");
    assert!(
        switches(&without)
            .unwrap_err()
            .contains("nonvoluntary_ctxt_switches")
    );
    let without = STATUS.replace("voluntary_ctxt_switches:\t12\n", "");
    assert!(
        switches(&without)
            .unwrap_err()
            .contains("voluntary_ctxt_switches")
    );
}

#[test]
fn a_field_that_is_not_a_number_is_an_error() {
    let bad = STATUS.replace("10840 kB", "lots kB");
    assert!(vm_hwm_kib(&bad).is_err());
}

#[test]
fn a_field_name_matches_whole_not_as_a_suffix() {
    // `voluntary_ctxt_switches` is a suffix of `nonvoluntary_ctxt_switches`.
    let only_non = "nonvoluntary_ctxt_switches:\t3\n";
    assert!(switches(only_non).is_err());
}

fn counts(voluntary: u64, involuntary: u64) -> Counts {
    Counts {
        voluntary,
        involuntary,
        name: None,
        state: 'S',
    }
}

fn named(voluntary: u64, involuntary: u64, name: &str) -> Counts {
    Counts {
        name: Some(name.to_owned()),
        ..counts(voluntary, involuntary)
    }
}

#[test]
fn a_thread_name_is_its_comm_line() {
    assert_eq!(comm("status\n"), Ok("status".to_owned()));
    assert_eq!(comm("tokio-runtime-w"), Ok("tokio-runtime-w".to_owned()));
    assert!(comm("\n").is_err());
    assert!(comm("").is_err());
    assert!(comm("a\nb\n").is_err());
}

#[test]
fn a_named_thread_carries_its_name_in_its_entry() {
    let before = BTreeMap::from([(4101, named(1, 0, "fiber")), (4102, counts(0, 0))]);
    let after = BTreeMap::from([(4101, named(2, 0, "fiber")), (4102, named(0, 0, "status"))]);
    let mut notes = Vec::new();
    assert_eq!(
        idle_switches(&before, &after, &mut notes),
        vec![
            json!({"tid": 4101, "voluntary": 1, "involuntary": 0, "name": "fiber"}),
            json!({"tid": 4102, "voluntary": 0, "involuntary": 0, "name": "status"}),
        ]
    );
    let after = BTreeMap::from([(4101, counts(1, 0)), (4102, counts(0, 0))]);
    assert_eq!(
        idle_switches(&before, &after, &mut notes),
        vec![
            json!({"tid": 4101, "voluntary": 0, "involuntary": 0, "name": "fiber"}),
            json!({"tid": 4102, "voluntary": 0, "involuntary": 0}),
        ]
    );
    assert!(notes.is_empty());
}

#[test]
fn idle_switches_are_the_per_thread_deltas() {
    let before = BTreeMap::from([(4101, counts(10, 2)), (4102, counts(5, 0))]);
    let after = BTreeMap::from([(4101, counts(10, 2)), (4102, counts(6, 1))]);
    let mut notes = Vec::new();
    let deltas = idle_switches(&before, &after, &mut notes);
    assert_eq!(
        deltas,
        vec![
            json!({"tid": 4101, "voluntary": 0, "involuntary": 0}),
            json!({"tid": 4102, "voluntary": 1, "involuntary": 1}),
        ]
    );
    assert!(notes.is_empty());
}

#[test]
fn a_thread_that_appears_or_vanishes_in_the_window_is_a_self_check_failure() {
    let before = BTreeMap::from([(4101, counts(1, 0)), (4102, counts(1, 0))]);
    let after = BTreeMap::from([(4101, counts(1, 0)), (4103, counts(0, 0))]);
    let mut notes = Vec::new();
    let deltas = idle_switches(&before, &after, &mut notes);
    assert_eq!(
        deltas,
        vec![json!({"tid": 4101, "voluntary": 0, "involuntary": 0})]
    );
    assert_eq!(
        notes,
        vec![
            "thread 4102 exited during the idle window".to_owned(),
            "thread 4103 started during the idle window".to_owned(),
        ]
    );
}

#[test]
fn a_counter_that_goes_backwards_is_a_self_check_failure() {
    let before = BTreeMap::from([(4101, counts(5, 0))]);
    let after = BTreeMap::from([(4101, counts(4, 0))]);
    let mut notes = Vec::new();
    let deltas = idle_switches(&before, &after, &mut notes);
    assert!(deltas.is_empty());
    assert_eq!(
        notes,
        vec!["thread 4101's switch counters went backwards".to_owned()]
    );
}

const NET_UNIX: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n\
0000000000000000: 00000002 00000000 00010000 0001 01 2001 /tmp/fb-1/h/run/hub\n\
0000000000000000: 00000003 00000000 00000000 0001 03 2002\n\
0000000000000000: 00000002 00000000 00000000 0001 03 2003 /tmp/fb-1/h/run/hubx\n";

#[test]
fn the_hub_is_connected_once_a_connected_socket_carries_its_path() {
    let path = std::path::Path::new("/tmp/fb-1/h/run/hub");
    // Listening only: state 01.
    assert!(!hub_connected(NET_UNIX, path));
    let accepted = format!(
        "{NET_UNIX}0000000000000000: 00000003 00000000 00000000 0001 03 2004 /tmp/fb-1/h/run/hub\n"
    );
    assert!(hub_connected(&accepted, path));
}

fn in_state(state: char, counts: Counts) -> Counts {
    Counts { state, ..counts }
}

#[test]
fn equal_sleeping_readings_are_settled() {
    let reading = BTreeMap::from([(4101, named(3, 1, "fiber")), (4102, counts(0, 0))]);
    assert!(unsettled(&reading, &reading.clone()).is_empty());
}

#[test]
fn each_unsettled_thread_gets_one_entry() {
    let first = BTreeMap::from([(4101, named(3, 1, "fiber"))]);
    let cases = [
        (
            BTreeMap::from([(4101, in_state('R', named(3, 1, "fiber")))]),
            "4101 (fiber) is R",
        ),
        (
            BTreeMap::from([(4101, named(4, 1, "fiber"))]),
            "4101 (fiber) switched",
        ),
        (
            BTreeMap::from([(4101, named(3, 2, "fiber"))]),
            "4101 (fiber) switched",
        ),
    ];
    for (second, entry) in cases {
        assert_eq!(unsettled(&first, &second), vec![entry.to_owned()]);
    }
    let unnamed = BTreeMap::from([(4102, counts(0, 0))]);
    let disk = BTreeMap::from([(4102, in_state('D', counts(0, 0)))]);
    assert_eq!(unsettled(&unnamed, &disk), vec!["4102 is D".to_owned()]);
    assert_eq!(
        unsettled(&first, &BTreeMap::new()),
        vec!["4101 exited".to_owned()]
    );
    assert_eq!(
        unsettled(&BTreeMap::new(), &first),
        vec!["4101 started".to_owned()]
    );
}

#[test]
fn unsettled_entries_come_in_tid_order() {
    let first = BTreeMap::from([
        (4101, named(1, 0, "fiber")),
        (4102, counts(0, 0)),
        (4104, named(5, 5, "tui-hub")),
        (4105, named(2, 0, "tui-input")),
    ]);
    let second = BTreeMap::from([
        (4101, in_state('R', named(1, 0, "fiber"))),
        (4103, counts(0, 0)),
        (4104, named(5, 6, "tui-hub")),
        (4105, named(2, 0, "tui-input")),
    ]);
    assert_eq!(
        unsettled(&first, &second),
        vec![
            "4101 (fiber) is R".to_owned(),
            "4102 exited".to_owned(),
            "4103 started".to_owned(),
            "4104 (tui-hub) switched".to_owned(),
        ]
    );
}
