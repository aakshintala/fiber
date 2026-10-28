//! The shared scope-switch mechanics' promises: the `at` mapping, the
//! focus mark, the focus moves, and the write-then-reread ordering.

use std::cell::RefCell;

use contract::ErrorCode;

use super::*;
use crate::configure::ConfigureError;

/// A `write_then_reread` state: the value `is_on` reports, the callbacks
/// in call order, and the message the last callback left.
#[derive(Debug, Default)]
struct Probe {
    /// What `is_on` reports for any scope.
    on: bool,
    /// The callbacks in the order they ran.
    calls: RefCell<Vec<&'static str>>,
    /// The `on` the write ran with.
    written: RefCell<Option<bool>>,
    /// Whether the write landed before it answered.
    landed: RefCell<bool>,
    /// When set, `write` lands, then fails with this message.
    fail: Option<String>,
    /// What the last callback said.
    said: Vec<String>,
}

/// A failure the write answers with.
fn error(message: &str) -> ConfigureError {
    ConfigureError {
        code: ErrorCode::IoFailed,
        message: message.to_owned(),
    }
}

/// Runs the shared step over `probe` at `scope`, logging each callback.
fn run(probe: &mut Probe, scope: SwitchScope) {
    ScopeSwitches::write_then_reread(
        scope,
        probe,
        |state: &Probe, _| {
            state.calls.borrow_mut().push("is_on");
            state.on
        },
        |state: &mut Probe, _, on| {
            state.calls.borrow_mut().push("write");
            *state.written.borrow_mut() = Some(on);
            *state.landed.borrow_mut() = true;
            match state.fail.clone() {
                Some(message) => Err(error(&message)),
                None => Ok(()),
            }
        },
        |state: &mut Probe| {
            state.calls.borrow_mut().push("reread");
        },
        |state: &mut Probe| {
            state.calls.borrow_mut().push("on_ok");
            state.said.clear();
        },
        |state: &mut Probe, message: String| {
            state.calls.borrow_mut().push("on_err");
            state.said = vec![message];
        },
    );
}

#[test]
fn at_zero_maps_to_this_project() {
    assert_eq!(ScopeSwitches::scope_for_at(0), SwitchScope::Project);
}

#[test]
fn above_zero_maps_to_everywhere() {
    for at in [1, 2, 99] {
        assert_eq!(ScopeSwitches::scope_for_at(at), SwitchScope::Everywhere);
    }
}

#[test]
fn the_focused_switch_of_the_selected_row_reads_as_a_chevron() {
    assert_eq!(
        ScopeSwitches::mark(true, true),
        ("›".to_owned(), Ink::Heading)
    );
}

#[test]
fn any_other_cell_reads_as_a_space() {
    for (selected, focused) in [(true, false), (false, true), (false, false)] {
        assert_eq!(
            ScopeSwitches::mark(selected, focused),
            (" ".to_owned(), Ink::Plain),
            "{selected} {focused}"
        );
    }
}

#[test]
fn focused_returns_the_switch_space_flips() {
    let switches = ScopeSwitches {
        focus: SwitchScope::Project,
    };
    assert_eq!(switches.focused(), SwitchScope::Project);
}

#[test]
fn left_and_right_move_the_focus_and_stop_at_each_end() {
    let mut switches = ScopeSwitches {
        focus: SwitchScope::Project,
    };
    switches.edit_key(&Edit::Right);
    assert_eq!(switches.focused(), SwitchScope::Everywhere);
    switches.edit_key(&Edit::Right);
    assert_eq!(switches.focused(), SwitchScope::Everywhere);
    switches.edit_key(&Edit::Left);
    assert_eq!(switches.focused(), SwitchScope::Project);
    switches.edit_key(&Edit::Left);
    assert_eq!(switches.focused(), SwitchScope::Project);
}

#[test]
fn other_edits_leave_the_focus_where_it_was() {
    for edit in [
        Edit::ShiftEnter,
        Edit::CtrlJ,
        Edit::WordLeft,
        Edit::WordRight,
        Edit::DeleteWord,
        Edit::DeleteToLineStart,
        Edit::LineStart,
        Edit::LineEnd,
        Edit::Delete,
        Edit::Paste("x".to_owned()),
    ] {
        for scope in [SwitchScope::Project, SwitchScope::Everywhere] {
            let mut switches = ScopeSwitches { focus: scope };
            switches.edit_key(&edit);
            assert_eq!(switches.focused(), scope, "{edit:?}");
        }
    }
}

#[test]
fn a_write_turns_the_switch_off_then_rereads_then_clears() {
    let mut probe = Probe {
        on: true,
        said: vec!["stale".to_owned()],
        ..Probe::default()
    };
    run(&mut probe, SwitchScope::Project);
    assert_eq!(*probe.written.borrow(), Some(false));
    assert_eq!(
        *probe.calls.borrow(),
        vec!["is_on", "write", "reread", "on_ok"]
    );
    assert!(probe.said.is_empty());
}

#[test]
fn a_write_turns_the_switch_on_when_it_was_off() {
    let mut probe = Probe {
        on: false,
        ..Probe::default()
    };
    run(&mut probe, SwitchScope::Everywhere);
    assert_eq!(*probe.written.borrow(), Some(true));
    assert_eq!(
        *probe.calls.borrow(),
        vec!["is_on", "write", "reread", "on_ok"]
    );
}

#[test]
fn a_write_that_landed_then_failed_rereads_before_it_says_why() {
    let mut probe = Probe {
        on: true,
        said: vec!["stale".to_owned()],
        fail: Some("the write landed, then syncing failed".to_owned()),
        ..Probe::default()
    };
    run(&mut probe, SwitchScope::Project);
    assert!(*probe.landed.borrow());
    assert_eq!(
        *probe.calls.borrow(),
        vec!["is_on", "write", "reread", "on_err"]
    );
    assert_eq!(
        probe.said,
        vec!["the write landed, then syncing failed".to_owned()]
    );
}
