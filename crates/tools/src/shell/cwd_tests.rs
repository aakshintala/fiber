//! Hand rows for the `cd` directory states (`docs/tools.md`, "Shell",
//! "Effects"): each `&&`, `||` and `;` element runs in the directories its
//! join allows, and a lost path refuses every later element.

use super::super::syntax::Join;
use super::{Cwd, MAX_DIRECTORIES};

/// The directories each element of a call runs in: `enter` for the join,
/// then `leave` for the `cd` target (`None` for any other element).
fn entered(workdir: &str, steps: &[(Join, Option<&str>)]) -> Vec<Vec<String>> {
    let mut cwd = Cwd::new(workdir);
    let mut seen = Vec::new();
    for (join, target) in steps {
        seen.push(cwd.enter(*join).expect("the path holds"));
        cwd.leave(*target);
    }
    seen
}

#[test]
fn the_cap_counts_distinct_directories() {
    assert_eq!(MAX_DIRECTORIES, 32);
}

#[test]
fn cd_and_runs_ls_in_the_new_directory() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x")), (Join::And, None)]),
        vec![vec!["/work".to_owned()], vec!["/work/x".to_owned()]],
    );
}

#[test]
fn cd_semicolon_runs_ls_in_both_directories() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x")), (Join::Seq, None)]),
        vec![
            vec!["/work".to_owned()],
            vec!["/work".to_owned(), "/work/x".to_owned()],
        ],
    );
}

#[test]
fn cd_pipe_runs_ls_in_both_directories() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x")), (Join::Or, None)]),
        vec![
            vec!["/work".to_owned()],
            vec!["/work".to_owned(), "/work/x".to_owned()],
        ],
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
        vec![
            vec!["/work".to_owned()],
            vec!["/work".to_owned()],
            vec!["/work".to_owned(), "/work/x".to_owned()],
        ],
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
        vec![
            vec!["/work".to_owned()],
            vec!["/work/x".to_owned()],
            vec!["/work/x/y".to_owned()],
        ],
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
            vec!["/work".to_owned()],
            vec!["/work/x".to_owned()],
            vec!["/work".to_owned(), "/work/x".to_owned()],
        ],
    );
}

#[test]
fn an_absolute_cd_replaces_the_directory() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("/abs")), (Join::Seq, None)],),
        vec![
            vec!["/work".to_owned()],
            vec!["/abs".to_owned(), "/work".to_owned()],
        ],
    );
}

#[test]
fn dotdot_pops_one_component() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("..")), (Join::And, None)]),
        vec![vec!["/work".to_owned()], vec!["/".to_owned()]],
    );
}

#[test]
fn dotdot_past_the_root_stays_literal() {
    assert_eq!(
        entered("/", &[(Join::First, Some("..")), (Join::And, None)],),
        vec![vec!["/".to_owned()], vec!["/..".to_owned()]],
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
        vec![
            vec!["/work".to_owned()],
            vec!["/work/a".to_owned()],
            vec!["/work".to_owned()],
        ],
    );
}

#[test]
fn a_cd_in_a_pipeline_moves_nothing() {
    assert_eq!(
        entered("/work", &[(Join::First, None), (Join::Seq, None)]),
        vec![vec!["/work".to_owned()], vec!["/work".to_owned()]],
    );
}

#[test]
fn a_bare_cd_alone_runs_in_the_workdir() {
    assert_eq!(
        entered("/work", &[(Join::First, Some("x"))]),
        vec![vec!["/work".to_owned()]],
    );
}

#[test]
fn thirty_absolute_cds_then_ls_hold_the_path() {
    let mut cwd = Cwd::new("/work");
    let mut targets = Vec::new();
    for n in 1..=30 {
        targets.push(format!("/d{n}"));
    }
    for (index, target) in targets.iter().enumerate() {
        let join = if index == 0 { Join::First } else { Join::Seq };
        let dirs = cwd.enter(join).expect("the path holds");
        assert!(dirs.len() <= MAX_DIRECTORIES, "{dirs:?}");
        cwd.leave(Some(target));
    }
    assert_eq!(cwd.enter(Join::Seq).expect("the path holds").len(), 31);
}

#[test]
fn thirty_one_absolute_cds_then_ls_hold_the_path() {
    let mut cwd = Cwd::new("/work");
    let mut targets = Vec::new();
    for n in 1..=31 {
        targets.push(format!("/d{n}"));
    }
    for (index, target) in targets.iter().enumerate() {
        let join = if index == 0 { Join::First } else { Join::Seq };
        cwd.enter(join).expect("the path holds");
        cwd.leave(Some(target));
    }
    assert_eq!(cwd.enter(Join::Seq).expect("the path holds").len(), 32);
}

#[test]
fn thirty_two_absolute_cds_lose_the_path() {
    let mut cwd = Cwd::new("/work");
    let mut targets = Vec::new();
    for n in 1..=32 {
        targets.push(format!("/d{n}"));
    }
    for (index, target) in targets.iter().enumerate() {
        let join = if index == 0 { Join::First } else { Join::Seq };
        cwd.enter(join).expect("the path holds");
        cwd.leave(Some(target));
    }
    assert!(cwd.enter(Join::Seq).is_none());
}
