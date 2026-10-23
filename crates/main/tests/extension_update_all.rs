//! Binary-level tests of `fiber extension update` with no name
//! (`docs/extensions.md`, "Versions"): every installed extension,
//! dependencies included, moves to its newest tag within the accepted
//! major, and a moved dependency records that version as a floor while
//! staying unrequested. Every step runs the built `fiber` against a local
//! git stand-in for `github.com`, never `plan()`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use support::{Deadline, KillGroup, Output, Setup, git, is_status, spawn_watched};

impl Setup {
    /// Runs `fiber` with `args` and the extra environment `env`, stdin
    /// closed, in its own process group under the test's deadline.
    #[track_caller]
    fn fiber_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            !fakes::group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }

    /// Runs `fiber` with `args` and a closed stdin.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Output {
        self.fiber_with_env(args, &[])
    }
}

/// A first-party provider package in the repository's `providers/`.
fn package(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name)
}

const MUSE: &str = "github.com/aakshintala/fiber/providers/muse";
const NEEDS_MUSE: &str = "github.com/aakshintala/fiber/providers/needs-muse";

/// Fiber's own repository as a local one, with `providers/muse` (the
/// dependency), `providers/needs-muse` (depending on it) and
/// `providers/opencode` (unrelated), and the environment that makes
/// `https://github.com/` mean the directory holding it.
struct Github {
    repo: PathBuf,
    deadline: Deadline,
    key: String,
    value: String,
}

impl Github {
    #[track_caller]
    fn new(setup: &Setup) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber");
        std::fs::create_dir_all(&repo).unwrap();
        git(setup.deadline, &repo, &["init", "--quiet"]);
        Self {
            repo,
            deadline: setup.deadline,
            key: format!("url.file://{}/.insteadOf", base.display()),
            value: "https://github.com/".into(),
        }
    }

    fn env(&self) -> [(&str, &str); 3] {
        [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", &self.key),
            ("GIT_CONFIG_VALUE_0", &self.value),
        ]
    }

    fn write_muse(&self, tag: &str) {
        let dir = self.repo.join("providers/muse");
        std::fs::create_dir_all(dir.join("providers")).unwrap();
        for file in ["extension.json", "providers/muse.json"] {
            std::fs::copy(package("muse").join(file), dir.join(file)).unwrap();
        }
        std::fs::write(dir.join("NOTES.md"), format!("notes for {tag}\n")).unwrap();
    }

    fn write_needs_muse(&self, tag: &str, muse_min: &str) {
        let dir = self.repo.join("providers/needs-muse");
        std::fs::create_dir_all(dir.join("providers")).unwrap();
        let manifest = format!(
            r#"{{"name":"{NEEDS_MUSE}","version":"0.0.0","fiber":"0.0.0","api":1,"depends":{{"{MUSE}":"{muse_min}"}}}}"#
        );
        std::fs::write(dir.join("extension.json"), manifest).unwrap();
        std::fs::write(dir.join("providers/.gitkeep"), "").unwrap();
        std::fs::write(dir.join("NOTES.md"), format!("needs-muse {tag}\n")).unwrap();
    }

    fn write_opencode(&self, tag: &str) {
        let dir = self.repo.join("providers/opencode");
        std::fs::create_dir_all(dir.join("providers")).unwrap();
        std::fs::copy(
            package("opencode").join("extension.json"),
            dir.join("extension.json"),
        )
        .unwrap();
        for entry in std::fs::read_dir(package("opencode").join("providers")).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.join("providers").join(entry.file_name())).unwrap();
        }
        std::fs::write(dir.join("NOTES.md"), format!("notes for {tag}\n")).unwrap();
    }

    fn commit(&self, message: &str) {
        git(self.deadline, &self.repo, &["add", "."]);
        git(
            self.deadline,
            &self.repo,
            &["commit", "--quiet", "-m", message],
        );
    }

    fn tag(&self, tag: &str) {
        git(self.deadline, &self.repo, &["tag", tag]);
    }

    /// Commits all three providers and tags each: the one release step
    /// costs one commit instead of three.
    #[track_caller]
    fn release(&self, muse_tag: &str, needs_tag: &str, opencode_tag: &str, muse_min: &str) {
        self.write_muse(muse_tag);
        self.write_needs_muse(needs_tag, muse_min);
        self.write_opencode(opencode_tag);
        self.commit(needs_tag);
        self.tag(muse_tag);
        self.tag(needs_tag);
        self.tag(opencode_tag);
    }

    /// Tags one more release of `muse` alone.
    #[track_caller]
    fn release_muse(&self, tag: &str) {
        self.write_muse(tag);
        self.commit(tag);
        self.tag(tag);
    }
}

/// The install record of the `muse` directory, as JSON.
fn muse_record(setup: &Setup) -> serde_json::Value {
    let text = std::fs::read_to_string(setup.home().join("extensions/muse/.fiber.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The `muse` line of `fiber extension list`, if it is installed.
fn muse_line(setup: &Setup) -> Option<String> {
    setup
        .run(&["extension", "list"])
        .stdout
        .lines()
        .filter(|line| !is_status(line))
        .find(|line| line.contains("providers/muse "))
        .map(str::to_owned)
}

#[test]
fn update_all_moves_a_dependency_to_its_newest_minor_and_floors_it() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v1.2.0", "v1.0.0", "v0.9.0", "1.2");
    let run = setup.fiber_with_env(&["extension", "install", NEEDS_MUSE], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    gh.release_muse("v1.5.0");
    let run = setup.fiber_with_env(&["extension", "update"], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    // The dependency moved without becoming a person request, and the
    // floor, not just the installed version, is recorded.
    let line = muse_line(&setup).expect("muse is listed");
    assert!(line.contains(" v1.5.0 "), "{line}");
    let record = muse_record(&setup);
    assert_eq!(record["floor"], serde_json::json!("v1.5.0"), "{record}");
    assert_eq!(record["requested"], serde_json::json!(false), "{record}");
}

#[test]
fn an_unrelated_install_keeps_the_floor() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v1.2.0", "v1.0.0", "v0.9.0", "1.2");
    let run = setup.fiber_with_env(&["extension", "install", NEEDS_MUSE], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    gh.release_muse("v1.5.0");
    let run = setup.fiber_with_env(&["extension", "update"], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let run = setup.fiber_with_env(&["extension", "install", "opencode"], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let line = muse_line(&setup).expect("muse is listed");
    assert!(line.contains(" v1.5.0 "), "{line}");
    let record = muse_record(&setup);
    assert_eq!(record["floor"], serde_json::json!("v1.5.0"), "{record}");
    assert_eq!(record["requested"], serde_json::json!(false), "{record}");
}

#[test]
fn removing_the_last_dependent_removes_the_floor_with_it() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v1.2.0", "v1.0.0", "v0.9.0", "1.2");
    let run = setup.fiber_with_env(&["extension", "install", NEEDS_MUSE], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    gh.release_muse("v1.5.0");
    let run = setup.fiber_with_env(&["extension", "update"], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let removed = setup.run(&["extension", "remove", NEEDS_MUSE]);
    assert_eq!(removed.code, Some(0), "{}", removed.stderr);
    assert_eq!(
        removed.stderr,
        format!("fiber: removed {NEEDS_MUSE}\nfiber: removed {MUSE}\n")
    );
    assert!(!setup.home().join("extensions/muse").exists());
    assert!(!setup.home().join("extensions/muse/.fiber.json").exists());
    // With the floor gone, a fresh install takes the lowest tag again,
    // with no floor.
    let run = setup.fiber_with_env(&["extension", "install", NEEDS_MUSE], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let line = muse_line(&setup).expect("muse is listed");
    assert!(line.contains(" v1.2.0 "), "{line}");
    let record = muse_record(&setup);
    assert_eq!(record.get("floor"), None, "{record}");
}

#[test]
fn update_all_stays_within_the_major_a_dependent_accepts() {
    let setup = Setup::new();
    let gh = Github::new(&setup);
    gh.release("v1.2.0", "v1.0.0", "v0.9.0", "1.2");
    gh.release_muse("v1.5.0");
    gh.release_muse("v2.0.0");
    let run = setup.fiber_with_env(&["extension", "install", NEEDS_MUSE], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let line = muse_line(&setup).expect("muse is listed");
    assert!(line.contains(" v1.2.0 "), "{line}");
    let run = setup.fiber_with_env(&["extension", "update"], &gh.env());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    // The dependent's 1.x minimum constrains the update to the newest
    // 1.x tag, floored there and still unrequested.
    let line = muse_line(&setup).expect("muse is listed");
    assert!(line.contains(" v1.5.0 "), "{line}");
    let record = muse_record(&setup);
    assert_eq!(record["floor"], serde_json::json!("v1.5.0"), "{record}");
    assert_eq!(record["requested"], serde_json::json!(false), "{record}");
}
