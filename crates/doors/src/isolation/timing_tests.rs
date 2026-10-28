//! The worktree create and remove stamps (`docs/state.md`, "Diagnostic
//! logs"): the two unconditional clock reads around the one
//! `worktree::create` or `worktree::recreate` call, the opener that emits
//! the one `worktree.create` line, and the `end` scope around the one
//! real removal. Time comes from the injected clock only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::Clock;
use contract::diag::{Op, Timing};
use contract::events::{Event, SessionStarted, Variables, VariablesSource};
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;

use super::{isolate, recreate};
use worktree::project;

/// One budget for the whole wait: setup, the code under test and the
/// assertions run on one thread under one deadline (`docs/testing.md`,
/// "Waits and timeouts").
const BUDGET: Duration = Duration::from_secs(40);

fn direct(command: &mut Command) -> io::Result<Output> {
    command.output()
}

fn repo(prefix: &str) -> fakes::TempDir {
    let home = fakes::TempDir::new(prefix);
    fakes::git::git(home.path(), &["init", "--quiet"]);
    fs::write(home.path().join("file.txt"), "x").unwrap();
    fakes::git::git(home.path(), &["add", "."]);
    fakes::git::git(home.path(), &["commit", "--quiet", "-m", "first"]);
    home
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn key_of(repo: &Path) -> String {
    log::project_key(&project(repo))
}

fn held(sessions: &Path, id: &str, workspace: &str) -> Log {
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let log = Log::create(sessions, SessionId(id.into()), Arc::clone(&clock)).unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: workspace.to_owned(),
            variables: Variables {
                path: "/usr/bin:/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    log
}

fn warn_line(home: &Path, id: &str) -> serde_json::Value {
    let text = fs::read_to_string(home.join("logs").join(format!("session-{id}.log"))).unwrap();
    assert_eq!(text.lines().count(), 1, "one diagnostic line");
    serde_json::from_str(text.lines().next().unwrap()).unwrap()
}

/// One closed scope: the operation, the tool, whether its start was
/// stamped, and the whole milliseconds its guard held.
struct Record {
    op: Op,
    tool: Option<String>,
    stamped: bool,
    ms: u64,
}

/// A recording `Timing` on the fake clock: each call counts itself and
/// stamps the call's start, each guard closes one record from the stamp
/// to the drop's `now`.
struct Capture {
    clock: Arc<FakeClock>,
    calls: AtomicUsize,
    records: Mutex<Vec<Record>>,
}

impl Capture {
    fn new(clock: Arc<FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            calls: AtomicUsize::new(0),
            records: Mutex::new(Vec::new()),
        })
    }

    fn timing(self: &Arc<Self>) -> Timing {
        let me = Arc::clone(self);
        Arc::new(
            move |op: Op, tool: Option<&str>, started: Option<Instant>| {
                me.calls.fetch_add(1, Ordering::SeqCst);
                let stamped = started.is_some();
                let start = started.unwrap_or_else(|| me.clock.now());
                let scope = Scope {
                    capture: Arc::clone(&me),
                    op,
                    tool: tool.map(str::to_owned),
                    stamped,
                    start,
                };
                Some(Box::new(scope) as Box<dyn Send>)
            },
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn records(&self) -> Vec<(Op, Option<String>, bool, u64)> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .map(|record| (record.op, record.tool.clone(), record.stamped, record.ms))
            .collect()
    }

    fn ops(&self) -> Vec<Op> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.op)
            .collect()
    }
}

struct Scope {
    capture: Arc<Capture>,
    op: Op,
    tool: Option<String>,
    stamped: bool,
    start: Instant,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let end = self.capture.clock.now();
        let ms = u64::try_from(
            end.checked_duration_since(self.start)
                .unwrap_or(Duration::ZERO)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        self.capture.records.lock().unwrap().push(Record {
            op: self.op,
            tool: self.tool.clone(),
            stamped: self.stamped,
            ms,
        });
    }
}

#[test]
fn create_opens_one_stamped_line_and_a_second_open_writes_nothing() {
    fakes::within(
        "create_opens_one_stamped_line_and_a_second_open_writes_nothing",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-timing-create-home");
            let repo = repo("doors-timing-create");
            let clock = FakeClock::new();
            let id = SessionId("s_0123456789abcdef".into());
            let clock_add = Arc::clone(&clock);
            let add = move |command: &mut Command| -> io::Result<Output> {
                let out = command.output()?;
                // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
                #[allow(
                    clippy::disallowed_methods,
                    reason = "no code under test waits on the clock (ruling 6179406086)"
                )]
                clock_add.advance(Duration::from_millis(7));
                Ok(out)
            };
            let isolation = isolate(
                repo.path(),
                home.path(),
                &id,
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                &add,
            )
            .unwrap();
            let capture = Capture::new(Arc::clone(&clock));
            let timing = capture.timing();
            assert_eq!(capture.calls(), 0, "nothing before the open");
            let opener = isolation.opener();
            opener(&timing);
            assert_eq!(capture.calls(), 1, "one create line");
            let records = capture.records();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].0, Op::WorktreeCreate);
            assert_eq!(records[0].1, None, "no tool name on a create line");
            assert!(records[0].2, "the create guard starts stamped");
            assert_eq!(records[0].3, 7, "the stamped add duration");
            // Setting the slot once means a second open adds nothing: a
            // mutant that emits on the set-fails path fails here.
            opener(&timing);
            assert_eq!(capture.calls(), 1, "a second open writes nothing");
            let path = isolation.path().to_path_buf();
            isolation.end();
            assert!(!exists(&path));
        },
    );
}

#[test]
fn recreation_stamp_excludes_the_lock_claim_and_the_downgrade() {
    fakes::within(
        "recreation_stamp_excludes_the_lock_claim_and_the_downgrade",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-timing-recreate-home");
            let repo = repo("doors-timing-recreate");
            let id = SessionId("s_0123456789abcdef".into());
            let first_clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
            let isolation = isolate(repo.path(), home.path(), &id, first_clock, &direct).unwrap();
            let recorded = isolation.worktree();
            let path = PathBuf::from(&recorded.path);
            let branch = recorded.branch.clone();
            let repository = PathBuf::from(recorded.repository.clone().unwrap());
            isolation.end();
            assert!(!exists(&path));

            let clock = FakeClock::new();
            let clock_add = Arc::clone(&clock);
            let add = move |command: &mut Command| -> io::Result<Output> {
                let out = command.output()?;
                // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
                #[allow(
                    clippy::disallowed_methods,
                    reason = "no code under test waits on the clock (ruling 6179406086)"
                )]
                clock_add.advance(Duration::from_millis(4));
                Ok(out)
            };
            let clock_downgrade = Arc::clone(&clock);
            let made = match super::recreate_with(
                &repository,
                &path,
                &branch,
                home.path(),
                &id,
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                &add,
                &|| {},
                &|creating| {
                    // The downgrade stays outside the stamp: advancing
                    // here must not change the recorded duration.
                    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
                    #[allow(
                        clippy::disallowed_methods,
                        reason = "no code under test waits on the clock (ruling 6179406086)"
                    )]
                    clock_downgrade.advance(Duration::from_millis(9));
                    super::shared::Creating::downgrade(creating)
                },
            )
            .unwrap()
            {
                super::Recreated::Made(made) => made,
                super::Recreated::Present(_) => panic!("the path is absent"),
            };
            let capture = Capture::new(Arc::clone(&clock));
            let timing = capture.timing();
            made.opener()(&timing);
            let records = capture.records();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].0, Op::WorktreeCreate);
            assert_eq!(records[0].3, 4, "only the add, not the downgrade");
            let path = made.path().to_path_buf();
            made.end();
            assert!(!exists(&path));
        },
    );
}

#[test]
fn a_failed_add_makes_no_isolation_and_no_opener() {
    fakes::within(
        "a_failed_add_makes_no_isolation_and_no_opener",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-timing-failed-home");
            let repo = repo("doors-timing-failed");
            let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
            let failing = |command: &mut Command| -> io::Result<Output> {
                let _ = command;
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            };
            match isolate(
                repo.path(),
                home.path(),
                &SessionId("s_0123456789abcdef".into()),
                clock,
                &failing,
            ) {
                Err(_) => {}
                Ok(_) => panic!("a failing add makes nothing"),
            }
            // A failed recreation likewise keeps no isolation, so there
            // is no opener to call.
            let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
            let error = match recreate(
                &repo.path().join(".git"),
                &home.path().join("wt"),
                "fiber/x",
                home.path(),
                &SessionId("s_0123456789abcdef".into()),
                clock,
                &failing,
            ) {
                Err(error) => error,
                Ok(_) => panic!("a failing recreate makes nothing"),
            };
            assert!(error.isolation.is_none(), "no opener exists");
        },
    );
}

#[test]
fn a_clean_removal_times_the_one_remove_call() {
    fakes::within(
        "a_clean_removal_times_the_one_remove_call",
        Deadline::after(BUDGET),
        || {
            let home = fakes::TempDir::new("doors-timing-remove-home");
            let repo = repo("doors-timing-remove");
            let clock = FakeClock::new();
            let id = SessionId("s_0123456789abcdef".into());
            let isolation = isolate(
                repo.path(),
                home.path(),
                &id,
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                &direct,
            )
            .unwrap();
            let path = isolation.path().to_path_buf();
            // The scope covers the one real removal: present at the
            // call, gone at the guard's drop. A stamp moved later fails
            // the first assert, an early drop the second.
            let path_call = path.clone();
            let path_drop = path.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let calls_in = Arc::clone(&calls);
            let timing: Timing = Arc::new(move |op, tool, started| {
                if op == Op::WorktreeCreate {
                    assert_eq!(tool, None);
                    assert!(started.is_some(), "the create guard starts stamped");
                    let guard = CreateEnd;
                    return Some(Box::new(guard) as Box<dyn Send>);
                }
                assert_eq!(op, Op::WorktreeRemove);
                assert_eq!(tool, None);
                assert!(started.is_none(), "the remove guard starts unstamped");
                assert!(exists(&path_call), "the worktree stands at the call");
                calls_in.fetch_add(1, Ordering::SeqCst);
                let guard = Presence {
                    path: path_drop.clone(),
                };
                Some(Box::new(guard) as Box<dyn Send>)
            });
            isolation.opener()(&timing);
            // One create call so far; the removal has not run yet.
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            isolation.end();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one remove scope");
            assert!(!exists(&path), "the path is gone");
            assert!(
                !branch_exists(repo.path(), &format!("fiber/{}", id.0)),
                "the branch is gone"
            );
        },
    );
}

/// The create probe's guard: held for the stamped scope, dropped at its end.
struct CreateEnd;

/// The removal probe's guard: dropping it proves the worktree is gone.
struct Presence {
    path: PathBuf,
}

impl Drop for Presence {
    fn drop(&mut self) {
        assert!(!exists(&self.path), "the worktree is gone at the drop");
    }
}

#[test]
fn failed_and_branch_kept_removals_write_one_line_with_the_warn_unchanged() {
    fakes::within(
        "failed_and_branch_kept_removals_write_one_line_with_the_warn_unchanged",
        Deadline::after(BUDGET),
        || {
            // A locked worktree fails its removal: one remove line, and
            // the warn line reads exactly as before.
            let home = fakes::TempDir::new("doors-timing-locked-home");
            let repo_dir = repo("doors-timing-locked");
            let clock = FakeClock::new();
            let id = "s_0123456789abcdef";
            let isolation = isolate(
                repo_dir.path(),
                home.path(),
                &SessionId(id.into()),
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                &direct,
            )
            .unwrap();
            let path = isolation.path().to_path_buf();
            run_git(
                repo_dir.path(),
                &["worktree", "lock", path.to_str().unwrap()],
            );
            let capture = Capture::new(Arc::clone(&clock));
            isolation.opener()(&capture.timing());
            assert_eq!(capture.calls(), 1);
            isolation.end();
            assert!(exists(&path), "the worktree stays");
            assert_eq!(capture.calls(), 2, "one create and one remove");
            assert_eq!(
                capture.ops(),
                vec![Op::WorktreeCreate, Op::WorktreeRemove],
                "exactly one scope per operation"
            );
            let line = warn_line(home.path(), id);
            assert_eq!(line["level"], "warn");
            assert_eq!(line["code"], "io_failed");
            assert_eq!(
                line["message"],
                format!(
                    "git worktree remove failed for the worktree {}.",
                    path.display()
                )
            );

            // A branch that cannot be deleted: the directory goes, the
            // branch stays, one remove line and one warn line.
            let home = fakes::TempDir::new("doors-timing-branchkept-home");
            let repo = repo("doors-timing-branchkept");
            let clock = FakeClock::new();
            let branch = format!("fiber/{id}");
            let isolation = isolate(
                repo.path(),
                home.path(),
                &SessionId(id.into()),
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                &direct,
            )
            .unwrap();
            let path = isolation.path().to_path_buf();
            let common_out = std::process::Command::new("git")
                .args([
                    "-C",
                    repo.path().to_str().unwrap(),
                    "rev-parse",
                    "--absolute-git-dir",
                ])
                .output()
                .unwrap();
            let common = String::from_utf8_lossy(&common_out.stdout)
                .trim()
                .to_owned();
            fs::write(
                Path::new(&common).join(format!("refs/heads/{branch}.lock")),
                "",
            )
            .unwrap();
            let capture = Capture::new(Arc::clone(&clock));
            isolation.opener()(&capture.timing());
            isolation.end();
            assert!(!exists(&path), "the worktree goes");
            assert!(branch_exists(repo.path(), &branch), "but the branch stays");
            assert_eq!(capture.calls(), 2, "one create and one remove");
            assert_eq!(capture.ops(), vec![Op::WorktreeCreate, Op::WorktreeRemove]);
            let line = warn_line(home.path(), id);
            assert_eq!(line["level"], "warn");
            assert_eq!(line["code"], "io_failed");
        },
    );
}

#[test]
fn kept_paths_write_no_remove_line() {
    fakes::within(
        "kept_paths_write_no_remove_line",
        Deadline::after(BUDGET),
        || {
            // An untracked file keeps the worktree.
            {
                let home = fakes::TempDir::new("doors-timing-kept-untracked-home");
                let repo = repo("doors-timing-kept-untracked");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                fs::write(path.join("notes.txt"), "scratch").unwrap();
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
                assert!(!home.path().join("logs").exists(), "no diagnostic line");
            }
            // A modified tracked file keeps the worktree.
            {
                let home = fakes::TempDir::new("doors-timing-kept-modified-home");
                let repo = repo("doors-timing-kept-modified");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                fs::write(path.join("file.txt"), "precious change").unwrap();
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
            }
            // A commit beyond the base keeps the worktree.
            {
                let home = fakes::TempDir::new("doors-timing-kept-commit-home");
                let repo = repo("doors-timing-kept-commit");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                fs::write(path.join("more.txt"), "y").unwrap();
                run_git(&path, &["add", "."]);
                run_git(&path, &["commit", "--quiet", "-m", "second"]);
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
            }
            // A held share keeps the worktree with no line.
            {
                let home = fakes::TempDir::new("doors-timing-kept-share-home");
                let repo = repo("doors-timing-kept-share");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                let _shared = super::share(&path).unwrap();
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
            }
            // A busy session log keeps the worktree.
            {
                let home = fakes::TempDir::new("doors-timing-kept-busy-home");
                let repo = repo("doors-timing-kept-busy");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                let sessions = home
                    .path()
                    .join("projects")
                    .join(key_of(repo.path()))
                    .join("sessions");
                let _holder = held(&sessions, id, path.to_str().unwrap());
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
            }
            // A switched branch keeps the worktree.
            {
                let home = fakes::TempDir::new("doors-timing-kept-switched-home");
                let repo = repo("doors-timing-kept-switched");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                run_git(&path, &["switch", "--quiet", "-c", "other"]);
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
            }
            // A rewound creator keeps the worktree.
            {
                let home = fakes::TempDir::new("doors-timing-kept-rewound-home");
                let repo = repo("doors-timing-kept-rewound");
                let clock = FakeClock::new();
                let id = "s_0123456789abcdef";
                let isolation = isolate(
                    repo.path(),
                    home.path(),
                    &SessionId(id.into()),
                    Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                    &direct,
                )
                .unwrap();
                let path = isolation.path().to_path_buf();
                let sessions = home
                    .path()
                    .join("projects")
                    .join(key_of(repo.path()))
                    .join("sessions");
                let holder = held(&sessions, id, path.to_str().unwrap());
                holder
                    .append(
                        &Event::Rewound(contract::events::Rewound {
                            new_session_id: SessionId("s_ffffffffffffffff".into()),
                            seq: contract::Seq(1),
                            from_session_id: None,
                            summary: None,
                        }),
                        None,
                        None,
                    )
                    .unwrap();
                drop(holder);
                let capture = Capture::new(Arc::clone(&clock));
                isolation.opener()(&capture.timing());
                isolation.end();
                assert!(exists(&path));
                assert_eq!(capture.calls(), 1, "create only, no remove");
            }
        },
    );
}
