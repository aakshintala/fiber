//! Tests for file search: the ranking, the listing and the worker.

use super::{CHECK_EVERY, KEPT, Search, list, rank};
use crate::Input;
use fakes::Deadline;
use std::cell::Cell;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

fn owned(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

/// Runs `work` on a thread and returns its result within [`DEADLINE`].
#[track_caller]
fn within<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("files-within".to_owned())
        .spawn(move || done.send(work()).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(result) => result,
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

/// Runs git with `args` in `dir`, which must succeed.
#[track_caller]
fn git(dir: &Path, args: &[&str]) {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let what = format!("git {}", args.join(" "));
    let status = within(&what, move || {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .unwrap_or_else(|err| panic!("{what}: {err}"));
    assert!(status.success(), "{what}: {status}");
}

/// Lists `dir` within [`DEADLINE`].
#[track_caller]
fn list_within(dir: &Path) -> Result<Vec<String>, String> {
    let dir = dir.to_path_buf();
    within("the listing", move || list(&dir))
}

/// A repository with `a.txt` and `sub/b.rs` tracked and `c.txt` not.
#[track_caller]
fn repository() -> fakes::TempDir {
    let dir = fakes::TempDir::new("tui-files");
    let root = dir.path();
    std::fs::create_dir(root.join("sub")).unwrap_or_else(|err| panic!("mkdir: {err}"));
    for file in ["a.txt", "sub/b.rs", "c.txt"] {
        std::fs::write(root.join(file), "x").unwrap_or_else(|err| panic!("{file}: {err}"));
    }
    git(root, &["init", "-q"]);
    git(root, &["add", "a.txt", "sub/b.rs"]);
    dir
}

/// The next result the worker posts, within [`DEADLINE`].
#[track_caller]
fn next_result(
    rx: &Receiver<Input>,
    what: &str,
    wait: &Deadline,
) -> (u64, Result<Vec<String>, String>) {
    match wait.recv(rx) {
        Ok(Input::Files { generation, result }) => (generation, result),
        Ok(_) => panic!("{what}: not a file search result"),
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

#[test]
fn file_name_matches_come_first_then_shorter_then_by_path() {
    let paths = owned(&[
        "src/main.rs",
        "docs/src.md",
        "src/a/lib.rs",
        "lib/srcfile.rs",
        "Src/b.rs",
        "README",
    ]);
    let found = rank(&paths, "SRC", || false);
    assert_eq!(
        found,
        Some(owned(&[
            "docs/src.md",
            "lib/srcfile.rs",
            "Src/b.rs",
            "src/main.rs",
            "src/a/lib.rs",
        ]))
    );
}

#[test]
fn an_empty_query_matches_every_path_shortest_first() {
    let paths = owned(&["bb", "a", "ab"]);
    assert_eq!(rank(&paths, "", || false), Some(owned(&["a", "ab", "bb"])));
}

#[test]
fn only_the_first_fifty_are_kept() {
    let paths: Vec<String> = (0..120).rev().map(|at| format!("f{at:03}")).collect();
    let found = rank(&paths, "f", || false).unwrap_or_default();
    assert_eq!(found.len(), KEPT);
    assert_eq!(found.first().map(String::as_str), Some("f000"));
    assert_eq!(found.last().map(String::as_str), Some("f049"));
}

#[test]
fn exactly_fifty_matches_are_all_kept_and_one_more_is_cut() {
    for count in [KEPT, KEPT + 1] {
        let paths: Vec<String> = (0..count).rev().map(|at| format!("g{at:03}")).collect();
        let found = rank(&paths, "g", || false).unwrap_or_default();
        assert_eq!(found.len(), KEPT, "{count} matches");
        assert_eq!(found.first().map(String::as_str), Some("g000"), "{count}");
        assert_eq!(found.last().map(String::as_str), Some("g049"), "{count}");
    }
}

#[test]
fn cancellation_is_checked_every_1024_paths() {
    assert_eq!(CHECK_EVERY, 1024);
    let paths: Vec<String> = (0..3000).map(|at| format!("p{at}")).collect();
    let asked = Cell::new(0usize);
    let found = rank(&paths, "zz", || {
        asked.set(asked.get() + 1);
        false
    });
    assert_eq!(found, Some(Vec::new()));
    assert_eq!(asked.get(), 3);
    // Cancelled on the second look: nothing comes back.
    let asked = Cell::new(0usize);
    let found = rank(&paths, "p", || {
        asked.set(asked.get() + 1);
        asked.get() == 2
    });
    assert_eq!(found, None);
    assert_eq!(asked.get(), 2);
}

#[test]
fn the_listing_is_the_tracked_files_from_the_repository_root() {
    let dir = repository();
    let mut found = list_within(&dir.path().join("sub")).unwrap_or_else(|err| panic!("{err}"));
    found.sort();
    assert_eq!(found, owned(&["a.txt", "sub/b.rs"]));
}

#[test]
fn a_directory_outside_a_repository_gives_the_error() {
    let dir = fakes::TempDir::new("tui-files-bare");
    let error = list_within(dir.path()).err().unwrap_or_default();
    assert!(error.contains("not a git repository"), "{error}");
    assert!(!error.contains('\n'), "{error}");
}

#[test]
fn the_worker_skips_a_search_superseded_before_it_ran() {
    let (out, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    // The listing waits at a pause point until both searches are asked.
    let search = Search::spawn(
        move || {
            gate.recv().map_err(|err| err.to_string())?;
            Ok(owned(&["a.rs", "b.rs"]))
        },
        out,
    );
    search.search(1, "a".to_owned());
    search.search(2, "b".to_owned());
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    assert_eq!(
        next_result(&rx, "the newest search", &Deadline::after(DEADLINE)),
        (2, Ok(owned(&["b.rs"])))
    );
    // Generation 1 never ran: the next result is the next search's.
    search.search(3, String::new());
    assert_eq!(
        next_result(&rx, "the next search", &Deadline::after(DEADLINE)),
        (3, Ok(owned(&["a.rs", "b.rs"])))
    );
}

#[test]
fn a_failed_listing_answers_every_search_with_its_error() {
    let (out, rx) = mpsc::channel();
    let search = Search::spawn(|| Err("no git".to_owned()), out);
    search.search(4, "x".to_owned());
    assert_eq!(
        next_result(&rx, "the error", &Deadline::after(DEADLINE)),
        (4, Err("no git".to_owned()))
    );
}

#[test]
fn a_worker_that_never_started_answers_every_search_with_why() {
    let (out, rx) = mpsc::channel();
    let search = Search::unstarted("no threads".to_owned(), out);
    search.search(5, "x".to_owned());
    assert_eq!(
        next_result(&rx, "the error", &Deadline::after(DEADLINE)),
        (5, Err("no threads".to_owned()))
    );
    search.search(6, String::new());
    assert_eq!(
        next_result(&rx, "the next error", &Deadline::after(DEADLINE)),
        (6, Err("no threads".to_owned()))
    );
}

#[test]
fn dropping_the_worker_abandons_the_search_it_was_asked_for() {
    let (out, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let search = Search::spawn(
        move || {
            gate.recv().map_err(|err| err.to_string())?;
            Ok(owned(&["a.rs"]))
        },
        out,
    );
    search.search(1, "a".to_owned());
    drop(search);
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    // The worker ends without posting: its sender goes with it.
    match Deadline::after(DEADLINE).recv(&rx) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("waited {DEADLINE:?} for the worker to end"),
        Ok(_) => panic!("a dropped worker posted a result"),
    }
}
