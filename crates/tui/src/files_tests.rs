//! Tests for file search: the ranking, the listing and the worker.

use super::{
    CHECK_EVERY, KEPT, Limits, Paths, Query, Roots, Search, entries, find, list, rank, resolve,
    split,
};
use crate::Input;
use fakes::Deadline;
use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
fn a_path_query_splits_at_its_last_slash() {
    use Query::{Path, Workspace};
    for typed in ["foo", "~", "..", "./x", "@~", "~foo", "..foo", ""] {
        assert_eq!(split(typed), Workspace(typed), "{typed}");
    }
    assert_eq!(
        split("~/proj/rea"),
        Path {
            base_text: "~/proj/",
            query: "rea"
        }
    );
    assert_eq!(
        split("../../a/b"),
        Path {
            base_text: "../../a/",
            query: "b"
        }
    );
    assert_eq!(
        split("/"),
        Path {
            base_text: "/",
            query: ""
        }
    );
    assert_eq!(
        split("~/"),
        Path {
            base_text: "~/",
            query: ""
        }
    );
    assert_eq!(
        split("../"),
        Path {
            base_text: "../",
            query: ""
        }
    );
    assert_eq!(
        split("/etc"),
        Path {
            base_text: "/",
            query: "etc"
        }
    );
    assert_eq!(
        split("../x"),
        Path {
            base_text: "../",
            query: "x"
        }
    );
    assert_eq!(
        split("/a/b/c"),
        Path {
            base_text: "/a/b/",
            query: "c"
        }
    );
}

#[test]
fn a_base_resolves_against_home_workspace_or_root() {
    let roots = Roots::new(PathBuf::from("/w"), Some(OsString::from("/home/u")));
    assert_eq!(
        resolve("~/proj/", &roots),
        Ok(PathBuf::from("/home/u/proj"))
    );
    assert_eq!(resolve("/etc/", &roots), Ok(PathBuf::from("/etc")));
    assert_eq!(resolve("../x/", &roots), Ok(PathBuf::from("/w/../x")));
    let no_home = Roots::new(PathBuf::from("/w"), None);
    assert_eq!(
        resolve("~/proj/", &no_home),
        Err("no home directory".to_owned())
    );
}

#[test]
fn an_empty_or_relative_home_is_no_home() {
    for home in [None, Some(OsString::from("")), Some(OsString::from("rel"))] {
        let roots = Roots::new(PathBuf::from("/w"), home);
        assert_eq!(resolve("~/", &roots), Err("no home directory".to_owned()));
    }
    let roots = Roots::new(PathBuf::from("/w"), Some(OsString::from("/home/u")));
    assert_eq!(resolve("~/", &roots), Ok(PathBuf::from("/home/u")));
}

/// A directory holding `names`: a trailing `/` makes a directory.
#[track_caller]
fn seeded(names: &[&str]) -> fakes::TempDir {
    let dir = fakes::TempDir::new("tui-files-base");
    for name in names {
        let path = dir.path().join(name);
        if name.ends_with('/') {
            std::fs::create_dir_all(&path).unwrap_or_else(|err| panic!("{name}: {err}"));
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap_or_else(|err| panic!("{name}: {err}"));
            }
            std::fs::write(&path, "x").unwrap_or_else(|err| panic!("{name}: {err}"));
        }
    }
    dir
}

/// The base listing of `dir`'s entries matching `query` as `@` rows
/// typed as `base_text`, with `limits` and a fresh budget.
#[track_caller]
fn listed(
    dir: &Path,
    base_text: &str,
    query: &str,
    limits: &Limits,
) -> Option<Result<Vec<String>, String>> {
    entries(dir, base_text, query, limits, &mut 0, || false)
}

#[test]
fn directories_come_first_then_by_name() {
    let dir = seeded(&["mango/", "Apple/", "apricot", "Zebra", "apple-pie"]);
    assert_eq!(
        listed(dir.path(), "~/proj/", "", &Limits::SEARCH),
        Some(Ok(owned(&[
            "~/proj/Apple/",
            "~/proj/mango/",
            "~/proj/apple-pie",
            "~/proj/apricot",
            "~/proj/Zebra",
        ])))
    );
}

#[test]
fn the_base_listing_matches_names_ignoring_case() {
    let dir = seeded(&["README.md", "readme-notes.md", "other.txt", "sub/"]);
    assert_eq!(
        listed(dir.path(), "~/proj/", "rea", &Limits::SEARCH),
        Some(Ok(owned(&["~/proj/readme-notes.md", "~/proj/README.md"])))
    );
    assert_eq!(
        listed(dir.path(), "~/proj/", "REA", &Limits::SEARCH),
        Some(Ok(owned(&["~/proj/readme-notes.md", "~/proj/README.md"])))
    );
}

#[test]
fn the_base_listing_cuts_to_kept() {
    for count in [2, 3, 4] {
        let names: Vec<String> = (0..count).map(|at| format!("f{at:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let dir = seeded(&refs);
        let limits = Limits {
            kept: 3,
            visits: Limits::SEARCH.visits,
            check_every: Limits::SEARCH.check_every,
        };
        let rows = listed(dir.path(), "/b/", "f", &limits)
            .unwrap_or_else(|| panic!("{count}: cancelled"))
            .unwrap_or_else(|err| panic!("{count}: {err}"));
        assert_eq!(rows.len(), count.min(3), "{count} entries");
        assert_eq!(rows.first().map(String::as_str), Some("/b/f00"), "{count}");
    }
}

#[test]
#[cfg(unix)]
fn a_symlink_to_a_directory_is_a_directory_row() {
    let dir = seeded(&["real/inner.txt"]);
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link"))
        .unwrap_or_else(|err| panic!("symlink: {err}"));
    std::os::unix::fs::symlink(
        dir.path().join("real/inner.txt"),
        dir.path().join("file-link"),
    )
    .unwrap_or_else(|err| panic!("symlink: {err}"));
    let rows = listed(dir.path(), "~/proj/", "link", &Limits::SEARCH)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/link/", "~/proj/file-link"]));
}

#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_name_is_listed_lossily() {
    use std::os::unix::ffi::OsStringExt;
    let dir = fakes::TempDir::new("tui-files-lossy");
    let raw = vec![0x66, 0xFF, 0x6F]; // f, an invalid byte, o
    std::fs::write(dir.path().join(std::ffi::OsString::from_vec(raw)), "x")
        .unwrap_or_else(|err| panic!("{err}"));
    let rows = listed(dir.path(), "~/proj/", "f", &Limits::SEARCH)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/f\u{FFFD}o"]));
}

#[test]
#[cfg(unix)]
fn a_name_with_a_control_character_is_not_listed() {
    let dir = seeded(&["li\nne", "fine"]);
    let mut examined = 0;
    let rows = entries(
        dir.path(),
        "~/proj/",
        "",
        &Limits::SEARCH,
        &mut examined,
        || false,
    )
    .unwrap_or_else(|| panic!("cancelled"))
    .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/fine"]));
    assert_eq!(examined, 2);
}

/// Runs `find` on `dir`'s entries matching `query` as `@` rows typed as
/// `base_text`, with `limits` and a worker that is never cancelled.
#[track_caller]
fn found(
    dir: &Path,
    base_text: &str,
    query: &str,
    limits: &Limits,
) -> Option<Result<Vec<String>, String>> {
    find(dir, base_text, query, limits, || false)
}

#[test]
fn the_walk_honours_gitignore_and_skips_dot_git() {
    // `.git/notes.txt` matches the query below: it stays out of the
    // rows only because the walk skips `.git`, so loosening that
    // filter to depth 1 and below lists it and fails this test.
    let dir = seeded(&[
        ".gitignore",
        ".git/HEAD",
        ".git/notes.txt",
        ".git/objects/pack",
        "vendor/ignored.txt",
        "deep/ignored-too.txt",
        "deep/kept.txt",
        "top.txt",
    ]);
    std::fs::write(
        dir.path().join(".gitignore"),
        "vendor/\ndeep/ignored-too.txt\n",
    )
    .unwrap_or_else(|err| panic!(".gitignore: {err}"));
    let rows = found(dir.path(), "~/proj/", "txt", &Limits::SEARCH)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/top.txt", "~/proj/deep/kept.txt"]));
}

#[test]
fn walk_rows_are_relative_to_the_base_as_typed() {
    let dir = seeded(&["sub/inner.txt", "sub/README.md"]);
    let rows = found(dir.path(), "~/proj/", "rea", &Limits::SEARCH)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/sub/README.md"]));
    let rows = found(dir.path(), "/etc/", "inner", &Limits::SEARCH)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["/etc/sub/inner.txt"]));
}

#[test]
fn the_walk_stops_at_the_row_bound() {
    let names: Vec<String> = (0..8).map(|at| format!("sub/f{at:02}.txt")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for kept in [5, 6, 7] {
        let dir = seeded(&refs);
        let limits = Limits {
            kept,
            visits: Limits::SEARCH.visits,
            check_every: Limits::SEARCH.check_every,
        };
        let rows = found(dir.path(), "~/proj/", "f", &limits)
            .unwrap_or_else(|| panic!("kept {kept}: cancelled"))
            .unwrap_or_else(|err| panic!("kept {kept}: {err}"));
        assert_eq!(rows.len(), kept, "kept {kept}");
    }
}

#[test]
fn the_walk_stops_at_the_visit_bound() {
    // Three depth-1 entries and exactly seven deeper ones, all matching:
    // ten examined entries in total.
    let names = [
        "a1",
        "a2",
        "sub/",
        "sub/b1",
        "sub/b2",
        "sub/deep/",
        "sub/deep/c1",
        "sub/deep/c2",
        "sub/deep/c3",
        "sub/deep/c4",
    ];
    for visits in [9, 10, 11] {
        let dir = seeded(&names);
        let limits = Limits {
            kept: Limits::SEARCH.kept,
            visits,
            check_every: Limits::SEARCH.check_every,
        };
        let rows = found(dir.path(), "~/proj/", "", &limits)
            .unwrap_or_else(|| panic!("visits {visits}: cancelled"))
            .unwrap_or_else(|err| panic!("visits {visits}: {err}"));
        assert_eq!(rows.len(), visits.min(10), "visits {visits}");
    }
}

#[test]
fn the_visit_bound_counts_unyielded_and_depth_one_entries() {
    // Eight examined entries, only two matching: the depth-1 `m1` and the
    // deep `sub/zzz/m2`. The walk streams in `read_dir` order, so a cut
    // inside it keeps no fixed rows: the fixed cuts are before the walk
    // yields anything (two visits: the depth-1 entries fill the budget)
    // and past its end.
    let names = [
        "m1",
        "sub/",
        "sub/n1",
        "sub/n2",
        "sub/n3",
        "sub/n4",
        "sub/zzz/",
        "sub/zzz/m2",
    ];
    for (visits, want) in [
        (2, ["~/proj/m1"].as_slice()),
        (8, ["~/proj/m1", "~/proj/sub/zzz/m2"].as_slice()),
        (9, ["~/proj/m1", "~/proj/sub/zzz/m2"].as_slice()),
    ] {
        let dir = seeded(&names);
        let limits = Limits {
            kept: Limits::SEARCH.kept,
            visits,
            check_every: Limits::SEARCH.check_every,
        };
        let rows = found(dir.path(), "~/proj/", "m", &limits)
            .unwrap_or_else(|| panic!("visits {visits}: cancelled"))
            .unwrap_or_else(|err| panic!("visits {visits}: {err}"));
        assert_eq!(rows, owned(want), "visits {visits}");
    }
}

#[test]
fn the_walk_does_not_start_when_the_base_fills_the_rows() {
    let dir = seeded(&["a1", "a2", "sub/deep-match.txt"]);
    let limits = Limits {
        kept: 2,
        visits: Limits::SEARCH.visits,
        check_every: Limits::SEARCH.check_every,
    };
    let rows = found(dir.path(), "~/proj/", "a", &limits)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/a1", "~/proj/a2"]));
}

#[test]
fn the_walk_reaches_its_rows_while_nothing_cancels_it() {
    // With `check_every: 1` every examined entry is a checkpoint, and
    // nothing cancels: loosening the checkpoint from `&&` to `||`
    // abandons the walk at its first countable entry and answers
    // `None` instead of these rows.
    let dir = seeded(&["sub/deep.txt"]);
    let limits = Limits {
        kept: Limits::SEARCH.kept,
        visits: Limits::SEARCH.visits,
        check_every: 1,
    };
    assert_eq!(
        found(dir.path(), "~/proj/", "deep", &limits),
        Some(Ok(owned(&["~/proj/sub/deep.txt"])))
    );
}

#[test]
fn a_cancelled_search_returns_none() {
    let dir = seeded(&["a1", "sub/b1"]);
    let looks = Cell::new(0usize);
    let cancelled = || {
        looks.set(looks.get() + 1);
        looks.get() == 2
    };
    let limits = Limits {
        kept: Limits::SEARCH.kept,
        visits: Limits::SEARCH.visits,
        check_every: 1,
    };
    assert_eq!(find(dir.path(), "~/proj/", "", &limits, cancelled), None);
    assert_eq!(looks.get(), 2);
}

#[test]
fn a_missing_base_is_an_error_row_reason() {
    let missing = Path::new("/no/such/base");
    match found(missing, "/no/such/", "", &Limits::SEARCH) {
        Some(Err(reason)) => assert!(reason.contains("No such file or directory"), "{reason}"),
        other => panic!("a missing base is an error, not {other:?}"),
    }
}

#[test]
fn a_file_as_base_is_an_error() {
    let dir = seeded(&["plain.txt"]);
    match found(
        &dir.path().join("plain.txt"),
        "~/proj/plain.txt/",
        "",
        &Limits::SEARCH,
    ) {
        Some(Err(reason)) => assert!(reason.contains("Not a directory"), "{reason}"),
        other => panic!("a file base is an error, not {other:?}"),
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
    let found = rank(&Paths::new(paths), "SRC", || false);
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
    assert_eq!(
        rank(&Paths::new(paths), "", || false),
        Some(owned(&["a", "ab", "bb"]))
    );
}

#[test]
fn only_the_first_fifty_are_kept() {
    let paths: Vec<String> = (0..120).rev().map(|at| format!("f{at:03}")).collect();
    let found = rank(&Paths::new(paths), "f", || false).unwrap_or_default();
    assert_eq!(found.len(), KEPT);
    assert_eq!(found.first().map(String::as_str), Some("f000"));
    assert_eq!(found.last().map(String::as_str), Some("f049"));
}

#[test]
fn exactly_fifty_matches_are_all_kept_and_one_more_is_cut() {
    for count in [KEPT, KEPT + 1] {
        let paths: Vec<String> = (0..count).rev().map(|at| format!("g{at:03}")).collect();
        let found = rank(&Paths::new(paths), "g", || false).unwrap_or_default();
        assert_eq!(found.len(), KEPT, "{count} matches");
        assert_eq!(found.first().map(String::as_str), Some("g000"), "{count}");
        assert_eq!(found.last().map(String::as_str), Some("g049"), "{count}");
    }
}

#[test]
fn cancellation_is_checked_every_1024_paths() {
    assert_eq!(CHECK_EVERY, 1024);
    let paths = Paths::new((0..3000).map(|at| format!("p{at}")).collect());
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
fn a_path_is_lowercased_once_when_the_listing_arrives_and_never_by_a_search() {
    use super::{Listing, search};
    let paths: Vec<String> = (0..3000).map(|n| format!("Dir{n}/file{n}.rs")).collect();
    let reference = Paths::new(paths.clone());
    let mut listing = Listing::new(move || Ok(paths));
    let roots = nowhere();
    crate::work::take();
    let first = search(&mut listing, &roots, "f", || false);
    assert_eq!(crate::work::take().path_lowercases, 3000);
    assert_eq!(first, Some(Ok(rank(&reference, "f", || false).unwrap())));
    crate::work::take();
    for query in ["fi", "fil", "FILE", "e1"] {
        let found = search(&mut listing, &roots, query, || false);
        assert_eq!(crate::work::take().path_lowercases, 0, "{query}");
        assert_eq!(
            found,
            Some(Ok(rank(&reference, query, || false).unwrap())),
            "{query}"
        );
    }
}

#[test]
fn a_lowercase_path_keeps_no_second_copy() {
    crate::work::take();
    let paths = Paths::new(owned(&["src/lib.rs", "Src/Lib.rs"]));
    assert_eq!(crate::work::take().path_lowercases, 2);
    assert_eq!(paths.paths[0], ("src/lib.rs".to_owned(), None));
    assert_eq!(
        paths.paths[1],
        ("Src/Lib.rs".to_owned(), Some("src/lib.rs".into()))
    );
}

#[test]
fn ranking_sorts_by_the_original_paths_length_when_lowercasing_changes_it() {
    // "İ" lowercases to two code points, so "İa" is 3 bytes and its
    // lowercase form 4: the sort key stays the 3.
    let paths = Paths::new(owned(&["abcd", "İa"]));
    assert_eq!(rank(&paths, "", || false), Some(owned(&["İa", "abcd"])));
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
fn the_search_core_lists_once_and_only_for_workspace_queries() {
    use super::{Listing, search};
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let mut listing = Listing::new(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(owned(&["alpha", "beta"]))
    });
    let home = seeded(&["alpha"]);
    let roots = Roots::new(PathBuf::from("/w"), Some(home.path().into()));
    assert_eq!(
        search(&mut listing, &roots, "~/al", || false),
        Some(Ok(owned(&["~/alpha"])))
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        search(&mut listing, &roots, "al", || false),
        Some(Ok(owned(&["alpha"])))
    );
    assert_eq!(
        search(&mut listing, &roots, "be", || false),
        Some(Ok(owned(&["beta"])))
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn the_search_core_checks_cancellation_before_either_mode() {
    use super::{Listing, search};
    for query in ["", "~/al"] {
        let mut listing = Listing::new(|| panic!("cancelled query must not list"));
        assert_eq!(search(&mut listing, &nowhere(), query, || true), None);
    }
}

#[test]
fn the_search_core_keeps_at_most_fifty_in_both_modes() {
    use super::{Listing, search};
    for (count, kept) in [(49, 49), (50, 50), (51, 50)] {
        let paths: Vec<String> = (0..count).map(|n| format!("f{n:03}")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let home = seeded(&refs);
        let roots = Roots::new(PathBuf::from("/w"), Some(home.path().into()));
        let mut listing = Listing::new(move || Ok(paths));
        for query in ["f", "~/f"] {
            let rows = search(&mut listing, &roots, query, || false)
                .unwrap()
                .unwrap();
            assert_eq!(rows.len(), kept, "{count}: {query}");
            assert!(rows.iter().all(|row| row.contains('f')));
        }
    }
}

/// Roots over an empty workspace and no home: for worker tests that
/// never resolve a path.
fn nowhere() -> Roots {
    Roots::new(PathBuf::from("/w"), None)
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
        nowhere(),
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
    let search = Search::spawn(|| Err("no git".to_owned()), nowhere(), out);
    search.search(4, "x".to_owned());
    assert_eq!(
        next_result(&rx, "the error", &Deadline::after(DEADLINE)),
        (4, Err("no git".to_owned()))
    );
}

#[test]
fn a_path_query_is_answered_while_the_listing_is_blocked() {
    let home = seeded(&["proj/alpha.txt", "proj/beta.txt"]);
    let (out, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let search = Search::spawn(
        move || {
            gate.recv().map_err(|err| err.to_string())?;
            Ok(owned(&["a.rs"]))
        },
        Roots::new(
            PathBuf::from("/w"),
            Some(home.path().as_os_str().to_owned()),
        ),
        out,
    );
    search.search(1, "~/proj/al".to_owned());
    // The rows arrive while the listing is still gated: a path query
    // never runs or waits on it.
    assert_eq!(
        next_result(&rx, "the path search", &Deadline::after(DEADLINE)),
        (1, Ok(owned(&["~/proj/alpha.txt"])))
    );
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    search.search(2, "a".to_owned());
    assert_eq!(
        next_result(&rx, "the workspace search", &Deadline::after(DEADLINE)),
        (2, Ok(owned(&["a.rs"])))
    );
}

#[test]
fn path_searches_use_the_roots_and_ignore_a_failed_listing() {
    let home = seeded(&["proj/README.md"]);
    let (out, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let search = Search::spawn(
        move || {
            gate.recv().map_err(|err| err.to_string())?;
            Err("no git".to_owned())
        },
        Roots::new(
            PathBuf::from("/w"),
            Some(home.path().as_os_str().to_owned()),
        ),
        out,
    );
    search.search(1, "~/proj/rea".to_owned());
    assert_eq!(
        next_result(&rx, "the path search", &Deadline::after(DEADLINE)),
        (1, Ok(owned(&["~/proj/README.md"])))
    );
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    search.search(2, "read".to_owned());
    assert_eq!(
        next_result(&rx, "the failed listing", &Deadline::after(DEADLINE)),
        (2, Err("no git".to_owned()))
    );
    // Without a home, a `~/` search answers why it cannot resolve.
    let (out, rx) = mpsc::channel();
    let search = Search::spawn(|| Ok(owned(&["a.rs"])), nowhere(), out);
    search.search(3, "~/proj/rea".to_owned());
    assert_eq!(
        next_result(&rx, "the homeless search", &Deadline::after(DEADLINE)),
        (3, Err("no home directory".to_owned()))
    );
}

#[test]
fn a_path_search_superseded_before_it_ran_is_skipped() {
    let home = seeded(&["proj/alpha.txt", "proj/beta.txt"]);
    let (out, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let search = Search::spawn(
        move || {
            gate.recv().map_err(|err| err.to_string())?;
            Ok(owned(&["a.rs"]))
        },
        Roots::new(
            PathBuf::from("/w"),
            Some(home.path().as_os_str().to_owned()),
        ),
        out,
    );
    // The workspace job holds the worker in the listing while two path
    // searches queue behind it.
    search.search(1, "a".to_owned());
    search.search(2, "~/proj/al".to_owned());
    search.search(3, "~/proj/be".to_owned());
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    assert_eq!(
        next_result(&rx, "the newest search", &Deadline::after(DEADLINE)),
        (3, Ok(owned(&["~/proj/beta.txt"])))
    );
    // Generation 2 never ran: the next result is the next search's.
    search.search(4, "a".to_owned());
    assert_eq!(
        next_result(&rx, "the next search", &Deadline::after(DEADLINE)),
        (4, Ok(owned(&["a.rs"])))
    );
}

#[test]
fn dropping_the_worker_abandons_a_path_search() {
    let home = seeded(&["proj/alpha.txt"]);
    let (out, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let (entered, listing) = mpsc::channel();
    let search = Search::spawn(
        move || {
            entered.send(()).map_err(|err| err.to_string())?;
            gate.recv().map_err(|err| err.to_string())?;
            Ok(owned(&["a.rs"]))
        },
        Roots::new(
            PathBuf::from("/w"),
            Some(home.path().as_os_str().to_owned()),
        ),
        out,
    );
    search.search(1, "a".to_owned());
    // Hold an in-flight listing, rather than race drop against a queued job.
    Deadline::after(DEADLINE).recv_or_fail(&listing, "workspace listing started");
    search.search(2, "~/proj/al".to_owned());
    drop(search);
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    // The worker ends without posting: the drop tags every generation
    // abandoned, so the queued path search answers nothing.
    match Deadline::after(DEADLINE).recv(&rx) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("waited {DEADLINE:?} for the worker to end"),
        Ok(_) => panic!("a dropped worker posted a result"),
    }
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
    let (started, listing) = mpsc::channel();
    let search = Search::spawn(
        move || {
            started.send(()).map_err(|err| err.to_string())?;
            gate.recv().map_err(|err| err.to_string())?;
            Ok(owned(&["a.rs"]))
        },
        nowhere(),
        out,
    );
    search.search(1, "a".to_owned());
    // Hold an in-flight listing, rather than race drop against a queued job.
    Deadline::after(DEADLINE).recv_or_fail(&listing, "workspace listing started");
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
