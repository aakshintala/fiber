//! Binary-level tests of `fiber approve` (`docs/extensions.md`, "Approving
//! outside a session"; `docs/testing.md`, "Levels"): the built `fiber` runs
//! in a temporary git repository with its own `FIBER_HOME`, and nothing here
//! touches the network. Every run carries a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Watchdog;
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

const HOOKS_FILE: &str = ".fiber/config/hooks.json";

/// Fiber home and a git repository in a temporary directory, removed on
/// drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-approve"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        setup.repository("w");
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// A git repository named `name` beside Fiber home.
    fn repository(&self, name: &str) -> PathBuf {
        let repo = self.root.path().join(name);
        fs::create_dir_all(&repo).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        repo
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// `projects/<key>/approvals` for the repository `name`.
    fn project_approvals(&self, name: &str) -> PathBuf {
        let git = fs::canonicalize(self.root.path().join(name).join(".git")).unwrap();
        self.home()
            .join("projects")
            .join(git.to_string_lossy().replace('/', "-"))
            .join("approvals")
    }

    /// The names of the files in a directory, sorted; none when it is absent.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Runs `fiber approve` with `args` in repository `name`, with `input`
    /// on standard input (none is an empty, closed input).
    fn approve(&self, name: &str, args: &[&str], input: Option<&str>) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .arg("approve")
            .args(args)
            .current_dir(self.root.path().join(name))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.unwrap_or("").as_bytes()).unwrap();
        drop(stdin);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                panic!(
                    "waited {DEADLINE:?} for `fiber approve` to exit (reaped after the kill: {reaped})"
                );
            }
        };
        watchdog.stand_down(DEADLINE);
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

// debt: `spawn_watched` copies `hooks.rs`'s, as the other binary test files
// in this crate each do; move every copy into `fakes` together when a
// change to one has to be made in all.

/// Spawns `command` in a new process group, then a watchdog in its own
/// group, which kills the group if this process dies first.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let watchdog = Watchdog::group(child.id());
    (child, watchdog)
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    /// The `approved <kind> <name> <hash prefix>` lines, as their words.
    fn approvals(&self) -> Vec<Vec<&str>> {
        self.stderr
            .lines()
            .filter(|line| line.starts_with("approved "))
            .map(|line| line.split(' ').collect())
            .collect()
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

fn declare_all(repo: &Path) {
    write(
        &repo.join("pkg/extension.json"),
        &json!({
            "name": "fiber.test/pkg", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
            "install": ["sh", "-c", "echo built > built.txt"],
        })
        .to_string(),
    );
    write(&repo.join("pkg/init.lua"), "-- entry\n");
    write(&repo.join("scripts/fmt.sh"), "echo fmt one\n");
    write(&repo.join("srv/run.js"), "// server\n");
    write(
        &repo.join(".fiber/config.json"),
        &json!({
            "repository_extensions": [{"path": "pkg"}],
            "mcp": {"servers": {"db": {"command": "node", "args": ["srv/run.js"]}}},
        })
        .to_string(),
    );
    write(
        &repo.join(HOOKS_FILE),
        &json!({"hooks": {"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 1000, "on_failure": "non-blocking"}}}).to_string(),
    );
}

/// The copy directories under `pinned/`, sorted; the lock file and the
/// `.ready` markers are not copies.
fn copies(setup: &Setup) -> Vec<String> {
    Setup::names(&setup.home().join("pinned"))
        .into_iter()
        .filter(|n| setup.home().join("pinned").join(n).is_dir())
        .collect()
}

/// The copy under `pinned/` whose name starts with `prefix`.
fn copy(setup: &Setup, prefix: &str) -> PathBuf {
    let names = copies(setup);
    let hits: Vec<_> = names.iter().filter(|n| n.starts_with(prefix)).collect();
    assert_eq!(hits.len(), 1, "{prefix} in {names:?}");
    setup.home().join("pinned").join(hits[0])
}

#[test]
fn yes_approves_an_extension_a_hook_and_a_server() {
    let setup = Setup::new();
    declare_all(&setup.workspace());
    let run = setup.approve("w", &["--yes"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let approvals = run.approvals();
    let kinds: Vec<_> = approvals.iter().map(|w| (w[1], w[2])).collect();
    assert_eq!(
        kinds,
        [
            ("extension", "fiber.test/pkg"),
            ("hook", "fmt"),
            ("mcp_server", "db")
        ]
    );
    assert!(
        approvals.iter().all(|w| w.len() == 4 && w[3].len() == 12),
        "{approvals:?}"
    );
    assert_eq!(run.stderr.lines().count(), 3, "{}", run.stderr);

    // Extension and hook per project, the server per machine.
    let per_project = Setup::names(&setup.project_approvals("w"));
    assert_eq!(per_project.len(), 2, "{per_project:?}");
    let per_machine = Setup::names(&setup.home().join("approvals"));
    assert_eq!(per_machine.len(), 1, "{per_machine:?}");
    for (kind, name, hash) in [
        ("extension", "fiber.test/pkg", &approvals[0][3]),
        ("hook", "fmt", &approvals[1][3]),
        ("mcp_server", "db", &approvals[2][3]),
    ] {
        let dir = if kind == "mcp_server" {
            setup.home().join("approvals")
        } else {
            setup.project_approvals("w")
        };
        let file = Setup::names(&dir)
            .into_iter()
            .find(|n| n.starts_with(hash))
            .unwrap();
        let body: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(&file)).unwrap()).unwrap();
        assert_eq!(body["decision"], "approve");
        assert_eq!(body["kind"], kind);
        assert_eq!(body["name"], name);
        // The copy of the same name.
        assert!(copy(&setup, hash).file_name().unwrap().to_string_lossy() == file);
    }
    // The install step ran in the copy, not in the repository.
    let extension = copy(&setup, approvals[0][3]);
    assert_eq!(
        fs::read_to_string(extension.join("built.txt")).unwrap(),
        "built\n"
    );
    assert!(extension.join("init.lua").is_file());
    assert!(!setup.workspace().join("pkg/built.txt").exists());
    assert_eq!(
        fs::read_to_string(copy(&setup, approvals[1][3]).join("scripts/fmt.sh")).unwrap(),
        "echo fmt one\n"
    );
    assert_eq!(
        fs::read_to_string(copy(&setup, approvals[2][3]).join("srv/run.js")).unwrap(),
        "// server\n"
    );
    // The offer was shown on stdout.
    assert!(
        run.stdout.contains("extension fiber.test/pkg (pkg)\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("hook fmt (.fiber/config/"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("mcp_server db (.fiber/config.json)\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("  loads in this project only\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_second_run_has_nothing_to_approve() {
    let setup = Setup::new();
    declare_all(&setup.workspace());
    assert_eq!(setup.approve("w", &["--yes"], None).code, Some(0));
    let run = setup.approve("w", &["--yes"], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stderr, "nothing to approve\n");
    assert_eq!(run.stdout, "");
}

#[test]
fn a_changed_hook_is_offered_again_with_a_diff_and_the_old_approval_stays() {
    let setup = Setup::new();
    declare_all(&setup.workspace());
    let first = setup.approve("w", &["--yes"], None);
    let old_hash = first.approvals()[1][3].to_owned();
    write(&setup.workspace().join("scripts/fmt.sh"), "echo fmt two\n");
    let run = setup.approve("w", &["--yes"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let approvals = run.approvals();
    assert_eq!(approvals.len(), 1, "{}", run.stderr);
    assert_eq!(&approvals[0][1..3], ["hook", "fmt"]);
    assert_ne!(approvals[0][3], old_hash);
    assert!(
        run.stdout
            .contains("--- a/scripts/fmt.sh\n+++ b/scripts/fmt.sh\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("-echo fmt one\n+echo fmt two\n"),
        "{}",
        run.stdout
    );
    // Both versions are approved and kept.
    assert_eq!(Setup::names(&setup.project_approvals("w")).len(), 3);
    assert_eq!(copies(&setup).len(), 4);
    assert_eq!(
        fs::read_to_string(copy(&setup, &old_hash).join("scripts/fmt.sh")).unwrap(),
        "echo fmt one\n"
    );
    // Switching back to the first version finds it approved.
    write(&setup.workspace().join("scripts/fmt.sh"), "echo fmt one\n");
    assert_eq!(
        setup.approve("w", &["--yes"], None).stderr,
        "nothing to approve\n"
    );
}

#[test]
fn a_piped_y_approves_and_a_piped_n_records_nothing() {
    let setup = Setup::new();
    declare_all(&setup.workspace());
    let no = setup.approve("w", &[], Some("n\n"));
    assert_eq!(no.code, Some(0), "{}", no.stderr);
    assert!(no.stderr.contains("approve all? [y/N]"), "{}", no.stderr);
    assert!(no.stderr.ends_with("nothing approved\n"), "{}", no.stderr);
    assert!(Setup::names(&setup.project_approvals("w")).is_empty());
    assert!(Setup::names(&setup.home().join("approvals")).is_empty());
    assert!(Setup::names(&setup.home().join("pinned")).is_empty());
    assert!(
        no.stdout.contains("hook fmt"),
        "the offer is shown before asking"
    );

    let yes = setup.approve("w", &[], Some("y\n"));
    assert_eq!(yes.code, Some(0), "{}", yes.stderr);
    assert_eq!(yes.approvals().len(), 3, "{}", yes.stderr);
}

#[test]
fn no_yes_and_no_input_refuses_naming_yes() {
    let setup = Setup::new();
    declare_all(&setup.workspace());
    let run = setup.approve("w", &[], None);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("--yes"), "{}", run.stderr);
    assert!(Setup::names(&setup.project_approvals("w")).is_empty());
    assert!(Setup::names(&setup.home().join("approvals")).is_empty());
    assert!(Setup::names(&setup.home().join("pinned")).is_empty());
}

#[test]
fn a_symbolic_link_out_of_the_repository_is_not_pinned() {
    let setup = Setup::new();
    let repo = setup.workspace();
    let outside = setup.root.path().join("secret.sh");
    fs::write(&outside, "echo secret\n").unwrap();
    fs::create_dir_all(repo.join("scripts")).unwrap();
    symlink(&outside, repo.join("scripts/link.sh")).unwrap();
    write(
        &repo.join(".fiber/config.json"),
        r#"{"mcp": {"servers": {"db": {"command": "scripts/link.sh"}}}}"#,
    );
    let run = setup.approve("w", &["--yes"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        run.stdout
            .contains("  not pinned: scripts/link.sh (outside the repository)\n"),
        "{}",
        run.stdout
    );
    let pinned = copy(&setup, run.approvals()[0][3]);
    assert!(
        Setup::names(&pinned).is_empty(),
        "{:?}",
        Setup::names(&pinned)
    );
}

#[test]
fn the_same_server_in_a_second_repository_is_not_offered_again() {
    let setup = Setup::new();
    let second = setup.repository("w2");
    for repo in [setup.workspace(), second.clone()] {
        write(&repo.join("srv/run.js"), "// server\n");
        write(
            &repo.join(".fiber/config.json"),
            r#"{"mcp": {"servers": {"db": {"command": "node", "args": ["srv/run.js"]}}}}"#,
        );
    }
    assert_eq!(setup.approve("w", &["--yes"], None).approvals().len(), 1);
    let run = setup.approve("w2", &["--yes"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "nothing to approve\n");
    // A hook is per project: the second repository is asked about its own.
    write(
        &second.join(HOOKS_FILE),
        r#"{"hooks": {"fmt": {"point": "after_tool", "command": "x"}}}"#,
    );
    assert_eq!(setup.approve("w2", &["--yes"], None).approvals().len(), 1);
}

#[test]
fn a_failing_install_step_exits_nonzero_names_the_item_and_records_nothing() {
    let setup = Setup::new();
    let repo = setup.workspace();
    write(
        &repo.join("pkg/extension.json"),
        &json!({
            "name": "fiber.test/broken", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
            "install": ["sh", "-c", "echo no good >&2; exit 4"],
        })
        .to_string(),
    );
    write(
        &repo.join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "pkg"}]}"#,
    );
    let run = setup.approve("w", &["--yes"], None);
    assert_ne!(run.code, Some(0));
    assert!(run.stderr.contains("fiber.test/broken"), "{}", run.stderr);
    assert!(run.stderr.contains("no good"), "{}", run.stderr);
    assert!(run.approvals().is_empty());
    assert!(Setup::names(&setup.project_approvals("w")).is_empty());
    assert!(copies(&setup).is_empty());
}

#[test]
fn a_package_path_outside_the_repository_is_an_error_and_records_nothing() {
    let setup = Setup::new();
    write(
        &setup.workspace().join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "../elsewhere"}]}"#,
    );
    let run = setup.approve("w", &["--yes"], None);
    assert_ne!(run.code, Some(0));
    assert!(run.stderr.contains("../elsewhere"), "{}", run.stderr);
    assert!(Setup::names(&setup.home().join("pinned")).is_empty());
}

#[test]
fn a_repository_that_declares_nothing_has_nothing_to_approve() {
    let setup = Setup::new();
    let run = setup.approve("w", &["--yes"], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stderr, "nothing to approve\n");
}

#[test]
fn an_install_step_runs_at_the_pinned_path_and_an_unfinished_copy_is_rebuilt() {
    let setup = Setup::new();
    let repo = setup.workspace();
    write(
        &repo.join("pkg/extension.json"),
        &json!({
            "name": "fiber.test/pkg", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
            "install": ["sh", "-c", r#"pwd > where; printf '#!/bin/sh\ncat "%s/payload.txt"\n' "$(pwd)" > run.sh"#],
        })
        .to_string(),
    );
    write(&repo.join("pkg/init.lua"), "-- entry\n");
    write(&repo.join("pkg/payload.txt"), "payload\n");
    write(
        &repo.join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "pkg"}]}"#,
    );
    let run = setup.approve("w", &["--yes"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.approvals().len(), 1, "{}", run.stderr);
    let dir = copy(&setup, run.approvals()[0][3]);
    let canonical = fs::canonicalize(&dir).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("where")).unwrap(),
        format!("{}\n", canonical.display())
    );
    // The script the step left reads the payload through the recorded
    // directory, so it only works when the step ran where the copy stayed.
    assert_eq!(script_output(&dir.join("run.sh")), "payload\n");
    // A kill before the `.ready` marker and the approval: the next approve
    // clears the unfinished copy and builds it again.
    let hash = dir.file_name().unwrap().to_string_lossy().into_owned();
    fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    write(&dir.join("junk"), "junk");
    fs::remove_file(setup.project_approvals("w").join(&hash)).unwrap();
    let again = setup.approve("w", &["--yes"], None);
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    assert_eq!(again.approvals().len(), 1, "{}", again.stderr);
    assert!(!dir.join("junk").exists());
    assert_eq!(
        fs::read_to_string(dir.join("where")).unwrap(),
        format!("{}\n", canonical.display())
    );
}

/// What the script the install step left prints, run with a deadline in its
/// own process group, so a hung script fails naming what it waited for.
fn script_output(script: &Path) -> String {
    let mut command = Command::new("sh");
    command
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(DEADLINE) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            fakes::kill_group(group, "KILL").unwrap();
            let reaped = finished.recv_timeout(DEADLINE).is_ok();
            panic!(
                "waited {DEADLINE:?} for `{}` to exit (reaped after the kill: {reaped})",
                script.display()
            );
        }
    };
    watchdog.stand_down(DEADLINE);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}
