use std::collections::BTreeMap;

use serde_json::json;

use super::{Counts, comm, hub_connected, idle_switches, switches, vm_hwm_kib};

const STATUS: &str = "Name:\tfiber\nState:\tS (sleeping)\nVmHWM:\t   10840 kB\nVmRSS:\t   10812 kB\nThreads:\t5\nvoluntary_ctxt_switches:\t12\nnonvoluntary_ctxt_switches:\t3\n";

#[test]
fn vm_hwm_is_read_in_kib() {
    assert_eq!(vm_hwm_kib(STATUS), Ok(10840));
}

#[test]
fn switches_are_the_voluntary_and_involuntary_counts() {
    assert_eq!(
        switches(STATUS),
        Ok(Counts {
            voluntary: 12,
            involuntary: 3,
            name: None,
        })
    );
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
