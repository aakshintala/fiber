//! Tests for file search: the ranking, the listing and the worker.

use super::{CHECK_EVERY, KEPT, Limits, Query, Roots, Search, entries, find, list, rank, resolve, split};
use crate::Input;
use fakes::Deadline;
use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
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
    assert_eq!(split("/"), Path { base_text: "/", query: "" });
    assert_eq!(split("~/"), Path { base_text: "~/", query: "" });
    assert_eq!(split("../"), Path { base_text: "../", query: "" });
    assert_eq!(split("/etc"), Path { base_text: "/", query: "etc" });
    assert_eq!(
        split("../x"),
        Path { base_text: "../", query: "x" }
    );
    assert_eq!(
        split("/a/b/c"),
        Path { base_text: "/a/b/", query: "c" }
    );
}

#[test]
fn a_base_resolves_against_home_workspace_or_root() {
    let roots = Roots::new(
        PathBuf::from("/w"),
        Some(OsString::from("/home/u")),
    );
    assert_eq!(resolve("~/proj/", &roots), Ok(PathBuf::from("/home/u/proj")));
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
        assert_eq!(
            resolve("~/", &roots),
            Err("no home directory".to_owned())
        );
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
fn listed(dir: &Path, base_text: &str, query: &str, limits: &Limits) -> Option<Result<Vec<String>, String>> {
    entries(dir, base_text, query, limits, &mut 0, || false)
}

fn search_limits() -> Limits {
    Limits {
        kept: Limits::SEARCH.kept,
        visits: Limits::SEARCH.visits,
        check_every: Limits::SEARCH.check_every,
    }
}

#[test]
fn directories_come_first_then_by_name() {
    let dir = seeded(&["mango/", "Apple/", "apricot", "Zebra", "apple-pie"]);
    assert_eq!(
        listed(dir.path(), "~/proj/", "", &search_limits()),
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
        listed(dir.path(), "~/proj/", "rea", &search_limits()),
        Some(Ok(owned(&["~/proj/readme-notes.md", "~/proj/README.md"])))
    );
    assert_eq!(
        listed(dir.path(), "~/proj/", "REA", &search_limits()),
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
    std::os::unix::fs::symlink(dir.path().join("real/inner.txt"), dir.path().join("file-link"))
        .unwrap_or_else(|err| panic!("symlink: {err}"));
    let rows = listed(dir.path(), "~/proj/", "link", &search_limits())
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
    let rows = listed(dir.path(), "~/proj/", "f", &search_limits())
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/f\u{FFFD}o"]));
}

#[test]
#[cfg(unix)]
fn a_name_with_a_control_character_is_not_listed() {
    let dir = seeded(&["li\nne", "fine"]);
    let mut examined = 0;
    let rows = entries(dir.path(), "~/proj/", "", &search_limits(), &mut examined, || false)
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/fine"]));
    assert_eq!(examined, 2);
}

/// Runs `find` on `dir`'s entries matching `query` as `@` rows typed as
/// `base_text`, with `limits` and a worker that is never cancelled.
#[track_caller]
fn found(dir: &Path, base_text: &str, query: &str, limits: &Limits) -> Option<Result<Vec<String>, String>> {
    find(dir, base_text, query, limits, || false)
}

#[test]
fn the_walk_honours_gitignore_and_skips_dot_git() {
    let dir = seeded(&[
        ".gitignore",
        ".git/HEAD",
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
    let rows = found(dir.path(), "~/proj/", "txt", &search_limits())
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/top.txt", "~/proj/deep/kept.txt"]));
}

#[test]
fn walk_rows_are_relative_to_the_base_as_typed() {
    let dir = seeded(&["sub/inner.txt", "sub/README.md"]);
    let rows = found(dir.path(), "~/proj/", "rea", &search_limits())
        .unwrap_or_else(|| panic!("cancelled"))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(rows, owned(&["~/proj/sub/README.md"]));
    let rows = found(dir.path(), "/etc/", "inner", &search_limits())
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
        "a1", "a2", "sub/", "sub/b1", "sub/b2", "sub/deep/", "sub/deep/c1",
        "sub/deep/c2", "sub/deep/c3", "sub/deep/c4",
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
    // deep `sub/zzz/m2`, examined last in the walk's path order.
    let names = [
        "m1", "sub/", "sub/n1", "sub/n2", "sub/n3", "sub/n4", "sub/zzz/", "sub/zzz/m2",
    ];
    for (visits, want) in [(7, ["~/proj/m1"].as_slice()), (8, ["~/proj/m1", "~/proj/sub/zzz/m2"].as_slice()), (9, ["~/proj/m1", "~/proj/sub/zzz/m2"].as_slice())] {
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
