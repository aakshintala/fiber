//! Hand rows for the `cd` directory states (`docs/tools.md`, "Shell",
//! "Effects"): each `&&`, `||` and `;` element runs in the directories its
//! join allows, and a lost path refuses every later element.

use std::collections::BTreeSet;

use super::super::syntax::Join;
use super::Cwd;

/// The membership of a directory list: the doc promises which directories
/// each element runs in, not their order.
fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// The directory memberships each element of a call runs in: `enter` for
/// the join, then `leave` for the `cd` target (`None` for any other
/// element).
fn entered(workdir: &str, steps: &[(Join, Option<&str>)]) -> Vec<BTreeSet<String>> {
    let mut cwd = Cwd::new(workdir);
    let mut seen = Vec::new();
    for (join, target) in steps {
        seen.push(
            cwd.enter(*join)
                .expect("the path holds")
                .into_iter()
                .collect(),
        );
        cwd.leave(*join, *target);
    }
    seen
}

#[test]
fn cd_and_runs_ls_in_the_new_directory() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x")), (Join::And, None)]),
        vec![set(&["/work"]), set(&["/work/x"])],
    );
}

#[test]
fn cd_semicolon_runs_ls_in_both_directories() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x")), (Join::Seq, None)]),
        vec![set(&["/work"]), set(&["/work", "/work/x"])],
    );
}

#[test]
fn cd_pipe_runs_ls_in_both_directories() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x")), (Join::Or, None)]),
        vec![set(&["/work"]), set(&["/work", "/work/x"])],
    );
}

#[test]
fn or_keeps_the_old_directory_for_a_later_and() {
    assert_eq!(
        entered(
            "/work",
            &[
                (Join::First, None),
                (Join::Or, Some("x")),
                (Join::And, None),
            ],
        ),
        vec![set(&["/work"]), set(&["/work"]), set(&["/work", "/work/x"]),],
    );
}

#[test]
fn repeated_cd_tracks_each_directory_in_order() {
    assert_eq!(
        entered(
            "/work",
            &[
                (Join::First, Some("x")),
                (Join::And, Some("y")),
                (Join::And, None),
            ],
        ),
        vec![set(&["/work"]), set(&["/work/x"]), set(&["/work/x/y"]),],
    );
}

#[test]
fn and_keeps_the_failed_directory_for_a_later_semicolon() {
    assert_eq!(
        entered(
            "/work",
            &[
                (Join::First, Some("x")),
                (Join::And, None),
                (Join::Seq, None),
            ],
        ),
        vec![
            set(&["/work"]),
            set(&["/work/x"]),
            set(&["/work", "/work/x"]),
        ],
    );
}

#[test]
fn an_absolute_cd_replaces_the_directory() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("/abs")), (Join::Seq, None)],),
        vec![set(&["/work"]), set(&["/abs", "/work"])],
    );
}

#[test]
fn dotdot_pops_one_component() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("..")), (Join::And, None)]),
        vec![set(&["/work"]), set(&["/"])],
    );
}

#[test]
fn dotdot_past_the_root_stays_literal() {
    assert_eq!(
        entered("/", &[(Join::First, Some("..")), (Join::And, None)],),
        vec![set(&["/"]), set(&["/.."])],
    );
}

#[test]
fn cd_dot_returns_to_the_workdir() {
    assert_eq!(
        entered(
            "/work",
            &[
                (Join::First, Some("a")),
                (Join::And, Some("..")),
                (Join::And, None),
            ],
        ),
        vec![set(&["/work"]), set(&["/work/a"]), set(&["/work"])],
    );
}

#[test]
fn a_cd_in_a_pipeline_moves_nothing() {
    assert_eq!(
        entered("/work", &[(Join::First, None), (Join::Seq, None)]),
        vec![set(&["/work"]), set(&["/work"])],
    );
}

#[test]
fn a_bare_cd_alone_runs_in_the_workdir() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x"))]),
        vec![set(&["/work"])],
    );
}

#[test]
fn the_path_holds_through_thirty_two_directories() {
    // Thirty `cd`s hold 31 directories and read, 31 hold 32 and read,
    // 32 hold 33 and lose the path.
    for (count, reads) in [(30_usize, true), (31, true), (32, false)] {
        let mut cwd = Cwd::new("/work");
        for n in 1..=count {
            let join = if n == 1 { Join::First } else { Join::Seq };
            cwd.enter(join).expect("the path holds");
            cwd.leave(join, Some(&format!("/d{n}")));
        }
        if reads {
            let dirs = cwd.enter(Join::Seq).expect("the path holds");
            assert_eq!(dirs.len(), count + 1, "{count} cds");
        } else {
            assert!(cwd.enter(Join::Seq).is_none(), "{count} cds");
        }
    }
}
