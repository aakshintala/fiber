//! Tests of `scripts/release-pack` (`docs/releasing.md`, "What a release
//! publishes" and "Cutting a release"): fixture trees pack into plain-ustar
//! archives with beside-it `.sha256` files, and the draft-publish-last flow
//! runs against the stubbed `gh` under `release_pack_fixture/bin`, so no
//! request reaches the network. Every run carries the test's one deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::ustar::sha256;
use support::{Deadline, Output};

/// A temporary root holding a fixture tree, an output directory and the
/// stubbed-`gh` state directory, with the test's deadline.
struct Pack {
    deadline: Deadline,
    root: fakes::TempDir,
}

impl Pack {
    fn start() -> Self {
        let root = fakes::TempDir::new("rlp");
        let pack = Self {
            deadline: Deadline::start(),
            root,
        };
        for dir in ["tree", "out", "state", "tmp"] {
            pack.wdir(dir);
        }
        pack
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// Creates the directory at `rel` with owner `rwx`.
    fn wdir(&self, rel: &str) -> PathBuf {
        let at = self.path(rel);
        fs::create_dir_all(&at).unwrap();
        fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
        at
    }

    /// Writes `data` at `rel` with `mode`, creating parents as needed.
    fn wfile(&self, rel: &str, data: &[u8], mode: u32) -> PathBuf {
        let at = self.path(rel);
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&at, data).unwrap();
        fs::set_permissions(&at, fs::Permissions::from_mode(mode)).unwrap();
        at
    }

    /// Links `rel` at `target`, creating parents as needed.
    fn wlink(&self, rel: &str, target: &str) {
        let at = self.path(rel);
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        symlink(target, &at).unwrap();
    }

    fn state(&self) -> PathBuf {
        self.path("state")
    }

    /// Runs `scripts/release-pack` with `args`, with the stubbed `gh`
    /// first on `PATH`, and waits for it under the test's deadline.
    #[track_caller]
    fn pack(&self, args: &[&str]) -> Output {
        self.tool(&script(), args, None)
    }

    /// Runs `tool` with `args` under the test's deadline, with `stdin`
    /// as its standard input when given.
    #[track_caller]
    fn tool(&self, tool: &Path, args: &[&str], stdin: Option<&Path>) -> Output {
        let path = format!("{}:/usr/bin:/bin", fixture_bin().display());
        let mut command = Command::new(tool);
        command
            // Its own process group, so run_to_exit's group-alive check
            // probes this run alone.
            .process_group(0)
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", path)
            .env("HOME", self.root.path())
            .env("TMPDIR", self.path("tmp"))
            .env("RELEASE_PACK_STATE", self.state())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match stdin {
            Some(file) => {
                command.stdin(fs::File::open(file).unwrap());
            }
            None => {
                command.stdin(Stdio::null());
            }
        }
        Output::from(support::run_to_exit(
            self.deadline,
            "release-pack to exit",
            command,
        ))
    }

    /// Writes a `fiber` crate manifest at `tree/crates/main/Cargo.toml`
    /// with `version`.
    #[track_caller]
    fn manifest(&self, version: &str) {
        self.wfile(
            "tree/crates/main/Cargo.toml",
            format!("[package]\nname = \"fiber\"\nversion = \"{version}\"\n").as_bytes(),
            0o644,
        );
    }
    /// The members of `archive`, sorted, as `tar -tzf` lists them.
    #[track_caller]
    fn members(&self, archive: &str) -> Vec<String> {
        let at = self.path(&format!("out/{archive}"));
        let run = self.tool(Path::new("tar"), &["-tzf", &at.to_string_lossy()], None);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        let mut members: Vec<_> = run.stdout.lines().map(str::to_owned).collect();
        members.sort();
        members
    }

    /// Asserts `out/<archive>` exists with the members and the `.sha256`
    /// file `sha256sum` would write beside it.
    #[track_caller]
    fn assert_archive(&self, archive: &str, members: &[&str]) {
        let bytes = fs::read(self.path(&format!("out/{archive}"))).unwrap();
        let mut want: Vec<_> = members.iter().map(|member| (*member).to_owned()).collect();
        want.sort();
        assert_eq!(self.members(archive), want);
        assert_eq!(
            fs::read_to_string(self.path(&format!("out/{archive}.sha256"))).unwrap(),
            format!("{}  {archive}\n", sha256(&bytes))
        );
    }

    /// The stubbed `gh`'s call log, one line per invocation.
    fn calls(&self) -> String {
        fs::read_to_string(self.state().join("calls.log")).unwrap_or_default()
    }
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/release-pack")
}

fn fixture_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/release_pack_fixture/bin")
}

/// Runs `command` to completion under `deadline`: the watched-process
/// wait and output handling both subprocess helpers share.
#[track_caller]
fn run_watched(command: &mut Command, deadline: Deadline, what: &str) -> Output {
    let (child, watchdog) = support::spawn_watched(command);
    let group = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(deadline, group, &finished, what),
    };
    watchdog.stand_down(deadline.cleanup_phase());
    Output::from(output)
}

/// A release tree: two providers, one extension, docs with an excluded
/// subtree and file, a distinctive `install.sh`, and a binary plus
/// notices. The 100-byte name and the 600/700 modes pass exactly at the
/// bounds the script enforces.
fn good_tree(pack: &Pack) {
    pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
    pack.wfile("tree/providers/alpha/lib.lua", b"-- alpha", 0o644);
    pack.wfile("tree/providers/beta/manifest.json", b"{}", 0o644);
    pack.wfile("tree/providers/beta/secret", b"secret", 0o600);
    pack.wfile(
        &format!("tree/providers/beta/{}", "b".repeat(100)),
        b"edge",
        0o644,
    );
    pack.wlink("tree/providers/alpha/up-link", "manifest.json");
    pack.wfile("tree/extensions/gamma/manifest.json", b"{}", 0o644);
    pack.wfile("tree/extensions/gamma/data..bak", b"dots", 0o644);
    pack.wdir("tree/extensions/gamma/private");
    fs::set_permissions(
        pack.path("tree/extensions/gamma/private"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    pack.wfile("tree/docs/guide.md", b"guide", 0o644);
    pack.wfile("tree/docs/skills/memory/SKILL.md", b"skill", 0o644);
    pack.wfile("tree/docs/agents/secret.md", b"agents", 0o644);
    pack.wfile("tree/docs/workflow.md", b"workflow", 0o644);
    pack.wfile("tree/scripts/install.sh", b"# fixture installer\n", 0o755);
    pack.wfile("tree/bin/fiber", b"fiber", 0o755);
    pack.wfile("tree/bin/THIRD-PARTY-NOTICES", b"notices", 0o644);
}

#[test]
fn a_good_tree_packs_ustar_archives_with_checksums() {
    let pack = Pack::start();
    good_tree(&pack);
    for run in [
        pack.pack(&[
            "pack-target",
            "tree/bin",
            "x86_64-unknown-linux-musl",
            "out",
        ]),
        pack.pack(&["pack-docs", "tree", "out"]),
        pack.pack(&["pack-extensions", "tree", "out"]),
        pack.pack(&["stage-installer", "tree", "out"]),
    ] {
        assert_eq!(run.code, Some(0), "{}", run.stderr);
    }
    pack.assert_archive(
        "fiber-x86_64-unknown-linux-musl.tar.gz",
        &["THIRD-PARTY-NOTICES", "fiber"],
    );
    pack.assert_archive(
        "fiber-docs.tar.gz",
        &[
            "guide.md",
            "skills/",
            "skills/memory/",
            "skills/memory/SKILL.md",
        ],
    );
    assert!(
        !pack
            .members("fiber-docs.tar.gz")
            .iter()
            .any(|member| member.starts_with("agents") || member.starts_with("workflow.md")),
        "excluded docs packed"
    );
    pack.assert_archive(
        "fiber-extensions.tar.gz",
        &[
            "alpha/",
            "alpha/lib.lua",
            "alpha/manifest.json",
            "alpha/up-link",
            "beta/",
            &format!("beta/{}", "b".repeat(100)),
            "beta/manifest.json",
            "beta/secret",
            "gamma/",
            "gamma/data..bak",
            "gamma/manifest.json",
            "gamma/private/",
        ],
    );
    assert_eq!(
        fs::read(pack.path("out/install.sh")).unwrap(),
        b"# fixture installer\n"
    );
    assert!(
        !pack.path("out/install.sh.sha256").exists(),
        "install.sh carries no checksum"
    );
}

#[test]
fn an_empty_extension_set_packs_nothing() {
    for missing in [false, true] {
        let pack = Pack::start();
        pack.wdir("tree/providers");
        pack.wdir("tree/extensions");
        if missing {
            fs::remove_dir(pack.path("tree/providers")).unwrap();
        }
        let run = pack.pack(&["pack-extensions", "tree", "out"]);
        assert_eq!(run.code, Some(1), "{}", run.stderr);
        assert_eq!(
            run.stderr,
            "release-pack: no directory under providers/ or extensions/.\n"
        );
        assert!(
            !pack.path("out/fiber-extensions.tar.gz").exists(),
            "a failed pack wrote an archive"
        );
    }
}

#[test]
fn two_extensions_installing_as_one_name_fail() {
    let pack = Pack::start();
    pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
    pack.wfile("tree/extensions/alpha/manifest.json", b"{}", 0o644);
    let run = pack.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "release-pack: two extensions install as alpha.\n"
    );
    assert!(!pack.path("out/fiber-extensions.tar.gz").exists());
}

#[test]
fn a_name_past_one_hundred_bytes_fails() {
    let pack = Pack::start();
    pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
    pack.wfile(
        &format!("tree/providers/alpha/{}", "a".repeat(101)),
        b"long",
        0o644,
    );
    let run = pack.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(
        run.stderr.contains("longer than 100 bytes"),
        "{}",
        run.stderr
    );
    assert!(!pack.path("out/fiber-extensions.tar.gz").exists());
}

#[test]
fn a_prefix_of_155_bytes_passes_and_156_fails() {
    // The prefix is the `ext/<pad>/<pad>` directory part: 3 + 1 + 75 + 1
    // + 75, with every component inside the 100-byte name bound.
    let passing = Pack::start();
    passing.wfile("tree/providers/ext/manifest.json", b"{}", 0o644);
    passing.wfile(
        &format!("tree/providers/ext/{0}/{0}/leaf", "p".repeat(75)),
        b"leaf",
        0o644,
    );
    let run = passing.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);

    let failing = Pack::start();
    failing.wfile("tree/providers/ext/manifest.json", b"{}", 0o644);
    failing.wfile(
        &format!(
            "tree/providers/ext/{0}/{1}/leaf",
            "p".repeat(76),
            "p".repeat(75)
        ),
        b"leaf",
        0o644,
    );
    let run = failing.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(
        run.stderr.contains("longer than 155 bytes"),
        "{}",
        run.stderr
    );
}

#[test]
fn an_absolute_or_escaping_symlink_fails() {
    for (name, target) in [("abs", "/etc/hostname"), ("dot", "../alpha")] {
        let pack = Pack::start();
        pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
        pack.wlink(&format!("tree/providers/alpha/{name}"), target);
        let run = pack.pack(&["pack-extensions", "tree", "out"]);
        assert_eq!(run.code, Some(1), "{name}: {}", run.stderr);
        assert!(!pack.path("out/fiber-extensions.tar.gz").exists());
    }
}

#[test]
fn a_file_or_directory_without_owner_bits_fails() {
    let pack = Pack::start();
    pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
    pack.wfile("tree/providers/alpha/locked", b"locked", 0o444);
    let run = pack.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(
        run.stderr.contains("carries no owner file bits"),
        "{}",
        run.stderr
    );

    let pack = Pack::start();
    pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
    let at = pack.wdir("tree/providers/alpha/plain");
    fs::set_permissions(&at, fs::Permissions::from_mode(0o555)).unwrap();
    let run = pack.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(
        run.stderr.contains("carries no owner dir bits"),
        "{}",
        run.stderr
    );
}

#[test]
fn the_draft_flow_publishes_last() {
    let pack = Pack::start();
    pack.wfile("notes.md", b"notes\n", 0o644);
    let notes = pack.path("notes.md").to_string_lossy().into_owned();
    let run = pack.pack(&["draft-begin", "1.2.3", &"a".repeat(40), &notes]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        fs::read_to_string(pack.state().join("draft-1.2.3")).unwrap(),
        format!("{}\n", "a".repeat(40))
    );
    assert_eq!(
        fs::read_to_string(pack.state().join("notes-1.2.3")).unwrap(),
        "notes\n"
    );
    let asset = pack.wfile("out/fiber-docs.tar.gz", b"docs", 0o644);
    let asset_name = asset.to_string_lossy().into_owned();
    let run = pack.pack(&["draft-upload", "1.2.3", &asset_name]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        fs::read_to_string(pack.state().join("uploads-1.2.3")).unwrap(),
        "fiber-docs.tar.gz\n"
    );
    let run = pack.pack(&["draft-publish", "1.2.3"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(pack.state().join("published-1.2.3").exists());
    for line in pack.calls().lines() {
        let head = line.split(' ').next().unwrap_or("");
        assert!(
            ["view", "create", "upload", "edit", "delete"].contains(&head),
            "a tag step ran: {line}"
        );
    }
    let create = pack
        .calls()
        .lines()
        .find(|line| line.starts_with("create"))
        .unwrap()
        .to_owned();
    assert!(create.contains("--draft"), "{create}");
    assert!(create.contains(&"a".repeat(40)), "{create}");
}

#[test]
fn a_failure_before_publish_leaves_no_tag_and_no_release() {
    let pack = Pack::start();
    pack.wfile("notes.md", b"notes\n", 0o644);
    let notes = pack.path("notes.md").to_string_lossy().into_owned();
    assert_eq!(
        pack.pack(&["draft-begin", "1.2.3", &"b".repeat(40), &notes])
            .code,
        Some(0)
    );
    // The upload names a file that is not there, as a failed step would:
    // the stub refuses it, so the run fails before anything publishes.
    let failed = pack.pack(&["draft-upload", "1.2.3", "out/missing-asset.tar.gz"]);
    assert_eq!(failed.code, Some(1), "{}", failed.stderr);
    assert!(
        failed.stderr.contains("missing-asset.tar.gz"),
        "{}",
        failed.stderr
    );
    assert!(
        !pack.calls().lines().any(|line| line.starts_with("edit")),
        "publish ran before the failure: {}",
        pack.calls()
    );
    assert!(!pack.state().join("published-1.2.3").exists());
    // The failure step deletes the draft, with the tag its creation made.
    let run = pack.pack(&["draft-delete", "1.2.3"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!pack.state().join("draft-1.2.3").exists());
    assert!(!pack.state().join("published-1.2.3").exists());
    let delete = pack
        .calls()
        .lines()
        .find(|line| line.starts_with("delete"))
        .unwrap()
        .to_owned();
    assert!(delete.contains("--cleanup-tag"), "{delete}");
}

#[test]
fn a_hard_link_fails() {
    // A hard-linked file would pack as a hard-link entry, which the
    // shipped unpacker refuses, so the pack fails before writing.
    let pack = Pack::start();
    pack.wfile("tree/providers/alpha/manifest.json", b"{}", 0o644);
    fs::hard_link(
        pack.path("tree/providers/alpha/manifest.json"),
        pack.path("tree/providers/alpha/copy.json"),
    )
    .unwrap();
    let run = pack.pack(&["pack-extensions", "tree", "out"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(run.stderr.contains("hard link"), "{}", run.stderr);
    assert!(!pack.path("out/fiber-extensions.tar.gz").exists());
}

#[test]
fn a_re_run_deletes_the_leftover_draft_first() {
    let pack = Pack::start();
    pack.wfile("notes.md", b"notes\n", 0o644);
    let notes = pack.path("notes.md").to_string_lossy().into_owned();
    assert_eq!(
        pack.pack(&["draft-begin", "1.2.3", &"b".repeat(40), &notes])
            .code,
        Some(0)
    );
    let run = pack.pack(&["draft-begin", "1.2.3", &"c".repeat(40), &notes]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        fs::read_to_string(pack.state().join("draft-1.2.3")).unwrap(),
        format!("{}\n", "c".repeat(40))
    );
    let calls = pack.calls();
    let order: Vec<_> = calls
        .lines()
        .filter_map(|line| line.split(' ').next())
        .collect();
    assert_eq!(
        order,
        ["view", "create", "view", "delete", "create"],
        "{order:?}"
    );
}

#[test]
fn a_published_version_refuses_before_building() {
    let pack = Pack::start();
    pack.wfile("state/published-9.9.9", b"old\n", 0o644);
    pack.wfile("notes.md", b"notes\n", 0o644);
    let notes = pack.path("notes.md").to_string_lossy().into_owned();
    let run = pack.pack(&["draft-begin", "9.9.9", &"d".repeat(40), &notes]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "release-pack: v9.9.9 is already published; refusing to build over it.\n"
    );
    assert!(
        !pack.calls().lines().any(|line| line.starts_with("create")),
        "a build started over a published release: {}",
        pack.calls()
    );
    assert!(
        !pack.calls().lines().any(|line| line.starts_with("delete")),
        "the published release was touched: {}",
        pack.calls()
    );
}

#[test]
fn deleting_a_published_release_is_refused() {
    let pack = Pack::start();
    pack.wfile("state/published-9.9.9", b"old\n", 0o644);
    let run = pack.pack(&["draft-delete", "9.9.9"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "release-pack: v9.9.9 is published; refusing to delete it.\n"
    );
    assert!(pack.state().join("published-9.9.9").exists());
}

#[test]
fn macos_samples_parse_to_bytes() {
    let pack = Pack::start();
    pack.wfile(
        "time.txt",
        b"        0.42 real         0.10 user         0.05 sys\n   46137344  maximum resident set size\n          0  average shared memory size\n",
        0o644,
    );
    let at = pack.path("time.txt").to_string_lossy().into_owned();
    let run = pack.pack(&["parse-time-l", &at]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "46137344\n");
    pack.wfile("empty-time.txt", b"no figures here\n", 0o644);
    let at = pack.path("empty-time.txt").to_string_lossy().into_owned();
    let run = pack.pack(&["parse-time-l", &at]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        format!("release-pack: {at} holds no maximum resident set size.\n")
    );

    pack.wfile(
        "footprint.txt",
        b"Physical footprint: 15360K\nPeak physical footprint: 20480K\nPhysical footprint: 1M\n",
        0o644,
    );
    let at = pack.path("footprint.txt").to_string_lossy().into_owned();
    let run = pack.pack(&["parse-footprint", &at]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "20971520\n");

    pack.wfile(
        "footprint-small.txt",
        b"Cache footprint: 12.5K\nWired footprint: 4096\nTotal footprint: 2G\n",
        0o644,
    );
    let at = pack
        .path("footprint-small.txt")
        .to_string_lossy()
        .into_owned();
    let run = pack.pack(&["parse-footprint", &at]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "2147483648\n");

    pack.wfile("empty-footprint.txt", b"nothing to see\n", 0o644);
    let at = pack
        .path("empty-footprint.txt")
        .to_string_lossy()
        .into_owned();
    let run = pack.pack(&["parse-footprint", &at]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
}

/// T6: the fixture end to end and the binary-level reinstall proof. A
/// fixture release holds the T2-packed archives served over loopback
/// HTTP: the target archive carries the real `fiber` binary, so
/// `scripts/install.sh --base-url` installs it and its `release-install`
/// stages the docs and every top-level archive directory as a first-party
/// extension, with the full-SHA record. No `git` and no network beyond
/// the fixture take part.
///
/// The version the binaries under test were built as.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release target `scripts/install.sh` picks on this host.
fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        (os, arch) => panic!("the fixture serves no release for {os}/{arch}"),
    }
}

/// A fixture release served over loopback HTTP, installed with
/// `scripts/install.sh` and driven with the installed binary, all under
/// the test's one deadline. The addresses stay on loopback: no `git` and
/// no network beyond the fixture take part.
struct Release {
    setup: support::Setup,
    server: fakes::ProviderServer,
}

impl Release {
    fn root(&self, rel: &str) -> PathBuf {
        self.setup.root.path().join(rel)
    }

    /// Writes `data` at `rel` under the root, creating parents as needed.
    fn write(&self, rel: &str, data: &[u8]) {
        let at = self.root(rel);
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&at, data).unwrap();
    }

    /// Runs `tool` with `args` and each of `env`, in its own process
    /// group with stdio piped, and waits for it under the deadline.
    #[track_caller]
    fn run(&self, tool: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(tool);
        command
            .args(args)
            .current_dir(self.setup.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.setup.root.path())
            .env("TMPDIR", self.root("tmp"))
            .env("FIBER_HOME", self.setup.home())
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_watched(
            &mut command,
            self.setup.deadline,
            &format!("{} to exit", tool.display()),
        )
    }

    /// The fixture's base URL: the loopback server's address.
    fn base(&self) -> String {
        self.server.url()
    }

    /// Runs `scripts/install.sh --base-url` on the fixture, with
    /// `FIBER_INSTALL_DIR` set to `dest` and then each of `env`.
    #[track_caller]
    fn install(&self, env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
        let dest = self.root("dest").to_string_lossy().into_owned();
        let mut full = vec![("FIBER_INSTALL_DIR", dest.as_str())];
        full.extend(env.iter().copied());
        self.run(&script, &["--base-url", &self.base()], &full)
    }

    /// Runs the installed `fiber` with `args` and each of `env`.
    #[track_caller]
    fn fiber(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_fiber"));
        self.run(&binary, args, env)
    }

    /// Packs a fixture release with the T2 script and serves it from
    /// `release/download/v<version>/`: the target archive carries the
    /// real `fiber` binary, the docs archive a README, and the extensions
    /// archive one provider and one extension with manifests at this
    /// binary's version.
    #[track_caller]
    fn serve() -> Self {
        let setup = support::Setup::new();
        let mut release = Self {
            setup,
            // Replaced with the responder below; every file is read
            // before the first install runs.
            server: fakes::ProviderServer::start_responding(|_| {
                fakes::Response::status(404, "not found")
            })
            .unwrap(),
        };
        for dir in ["tmp", "tree/bin", "out"] {
            fs::create_dir_all(release.root(dir)).unwrap();
        }
        let manifest = |short: &str, kind: &str| {
            serde_json::json!({
                "name": format!("github.com/aakshintala/fiber/{kind}/{short}"),
                "version": VERSION,
                "fiber": VERSION,
                "api": 1,
            })
            .to_string()
        };
        release.write(
            "tree/providers/anthropic/extension.json",
            manifest("anthropic", "providers").as_bytes(),
        );
        release.write(
            "tree/extensions/memory/extension.json",
            manifest("memory", "extensions").as_bytes(),
        );
        release.write("tree/docs/README.md", b"fixture docs");
        release.write("tree/docs/guide.md", b"guide");
        fs::copy(
            PathBuf::from(env!("CARGO_BIN_EXE_fiber")),
            release.root("tree/bin/fiber"),
        )
        .unwrap();
        fs::set_permissions(
            release.root("tree/bin/fiber"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        release.write("tree/bin/THIRD-PARTY-NOTICES", b"notices");
        for args in [
            vec!["pack-target", "tree/bin", host_target(), "out"],
            vec!["pack-docs", "tree", "out"],
            vec!["pack-extensions", "tree", "out"],
        ] {
            let run = release.run(&script(), &args, &[]);
            assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
        }
        let mut files = std::collections::HashMap::new();
        for name in [
            format!("fiber-{}.tar.gz", host_target()),
            "fiber-docs.tar.gz".to_owned(),
            "fiber-extensions.tar.gz".to_owned(),
        ] {
            let bytes = fs::read(release.root(&format!("out/{name}"))).unwrap();
            let sum = fs::read(release.root(&format!("out/{name}.sha256"))).unwrap();
            files.insert(format!("/download/v{VERSION}/{name}"), bytes);
            files.insert(format!("/download/v{VERSION}/{name}.sha256"), sum);
        }
        // A responder, not routes: routes answer once, and the mismatch
        // plus reinstall installs request the same paths again.
        release.server = fakes::ProviderServer::start_responding(move |request| {
            files
                .get(request.path.split('?').next().unwrap_or_default())
                .map(|body| fakes::Response::status(200, body.clone()))
                .unwrap_or_else(|| fakes::Response::status(404, "not found"))
        })
        .unwrap();
        release
    }
}

/// The commit the install record names for `name`, and its `.fiber.json`.
#[track_caller]
fn record_commit(home: &Path, name: &str) -> (PathBuf, String) {
    for entry in fs::read_dir(home.join("extensions")).unwrap() {
        let record = entry.unwrap().path().join(".fiber.json");
        let Ok(text) = fs::read_to_string(&record) else {
            continue;
        };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        if json.get("name").and_then(serde_json::Value::as_str) == Some(name) {
            let commit = json
                .get("source")
                .and_then(|source| source.get("commit"))
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_owned();
            return (record, commit);
        }
    }
    panic!("no install record for {name}")
}

/// Whether `text` is a full commit SHA: 40 lowercase hex digits, which is
/// what `Origin::clone_at` checks out.
fn is_full_sha(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `fiber --version` and the install record agree: the line prints the
/// version and the record's first 7 characters.
#[track_caller]
fn assert_version_agreement(release: &Release, commit: &str) {
    assert!(is_full_sha(commit), "the install record holds {commit:?}");
    let version = release.fiber(&["--version"], &[]);
    assert_eq!(version.code, Some(0), "{}", version.stderr);
    assert_eq!(
        version.stdout,
        format!("fiber {VERSION} ({})\n", &commit[..7]),
        "--version prints the record's first 7 characters"
    );
}

#[test]
fn install_sh_serves_the_target_and_refuses_a_mismatch() {
    let release = Release::serve();
    // The fixture serves no 999.0.0: the mismatch stays failed and
    // creates nothing.
    let bad = release.install(&[("FIBER_VERSION", "999.0.0")]);
    assert_eq!(bad.code, Some(1), "{}", bad.stderr);
    assert!(
        bad.stderr.ends_with(&format!(
            "fiber: fiber-{}.tar.gz could not be downloaded from {}/download/v999.0.0.\n",
            host_target(),
            release.base()
        )),
        "{}",
        bad.stderr
    );
    assert!(!release.root("dest").exists());
    assert!(!release.setup.home().join("docs").exists());
    assert!(!release.setup.home().join("extensions").exists());
    // The target passes in the same run: the binary installs, and its
    // release-install stages the docs and every top-level archive
    // directory as a first-party extension, with the full-SHA record.
    let good = release.install(&[("FIBER_VERSION", VERSION)]);
    assert_eq!(good.code, Some(0), "{}", good.stderr);
    assert_eq!(
        fs::read_to_string(release.setup.home().join("docs/README.md")).unwrap(),
        "fixture docs"
    );
    let listed = release.fiber(&["extension", "list"], &[]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    let mut commits: Vec<String> = Vec::new();
    for name in [
        "github.com/aakshintala/fiber/extensions/memory",
        "github.com/aakshintala/fiber/providers/anthropic",
    ] {
        let line = listed
            .stdout
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("{name} is not listed:\n{}", listed.stdout));
        let mut fields = line.split_whitespace();
        assert_eq!(fields.next(), Some(name));
        assert_eq!(fields.next(), Some(VERSION));
        commits.push(
            fields
                .next()
                .unwrap_or_else(|| panic!("{line:?} names no commit"))
                .to_owned(),
        );
        assert!(fields.next().is_none(), "{line:?} has trailing fields");
    }
    assert!(!listed.stdout.contains("damaged"), "{}", listed.stdout);
    assert_eq!(commits[0], commits[1], "each record holds the same commit");
    assert_version_agreement(&release, &commits[0]);
    // Over another version: stale docs and a stale manifest version come
    // back to the target on reinstall.
    fs::write(release.setup.home().join("docs/README.md"), "stale").unwrap();
    let (record, _) = record_commit(
        &release.setup.home(),
        "github.com/aakshintala/fiber/extensions/memory",
    );
    let manifest = record.parent().unwrap().join("extension.json");
    let stale = serde_json::json!({
        "name": "github.com/aakshintala/fiber/extensions/memory",
        "version": "9.9.9",
        "fiber": VERSION,
        "api": 1,
    });
    fs::write(&manifest, stale.to_string()).unwrap();
    let again = release.install(&[("FIBER_VERSION", VERSION)]);
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    assert_eq!(
        fs::read_to_string(release.setup.home().join("docs/README.md")).unwrap(),
        "fixture docs"
    );
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    assert_eq!(
        json.get("version").and_then(serde_json::Value::as_str),
        Some(VERSION)
    );
}

#[test]
fn the_recorded_full_sha_reinstalls_and_a_short_commit_is_refused() {
    let release = Release::serve();
    let good = release.install(&[("FIBER_VERSION", VERSION)]);
    assert_eq!(good.code, Some(0), "{}", good.stderr);
    let home = release.setup.home();
    let (record, commit) = record_commit(&home, "github.com/aakshintala/fiber/extensions/memory");
    assert_version_agreement(&release, &commit);
    // A short record fails loudly before `git` runs, never silently.
    let mut short =
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&record).unwrap()).unwrap();
    short["source"]["commit"] = serde_json::Value::String("4f2a9c1".to_owned());
    fs::write(&record, short.to_string()).unwrap();
    let refused = release.fiber(&["extension", "reinstall", "memory"], &[]);
    assert_ne!(refused.code, Some(0), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("the recorded commit `4f2a9c1` is not a full commit hash"),
        "{}",
        refused.stderr
    );
    // The recorded full SHA is accepted: a fixture repository holding it
    // checks out through the reinstall, with no network beyond it.
    let repos = release.root("repos");
    let source = repos.join("extensions/memory");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("extension.json"),
        serde_json::json!({
            "name": "github.com/aakshintala/fiber/extensions/memory",
            "version": VERSION,
            "fiber": VERSION,
            "api": 1,
        })
        .to_string(),
    )
    .unwrap();
    fakes::git::git(&repos, &["init", "--quiet"]);
    fakes::git::git(&repos, &["add", "."]);
    fakes::git::git(&repos, &["commit", "--quiet", "-m", "release"]);
    let head = fakes::git::git(&repos, &["rev-parse", "HEAD"]);
    assert!(is_full_sha(&head));
    let mut full =
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&record).unwrap()).unwrap();
    full["source"]["commit"] = serde_json::Value::String(head.clone());
    fs::write(&record, full.to_string()).unwrap();
    let key = format!("url.file://{}/.insteadOf", repos.display());
    let reinstalled = release.fiber(
        &["extension", "reinstall", "memory"],
        &[
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", key.as_str()),
            ("GIT_CONFIG_VALUE_0", "https://github.com/aakshintala/fiber"),
            ("GIT_CONFIG_KEY_1", "protocol.file.allow"),
            ("GIT_CONFIG_VALUE_1", "always"),
        ],
    );
    assert_eq!(reinstalled.code, Some(0), "{}", reinstalled.stderr);
    let (_, commit) = record_commit(&home, "github.com/aakshintala/fiber/extensions/memory");
    assert_eq!(commit, head);
}

/// `fiber --version` prints the version alone when no commit was recorded
/// (`docs/releasing.md`, "Versions"): a build with no git history, such
/// as a source tarball or a mutation-testing copy, leaves `FIBER_COMMIT`
/// unset, so the line is `fiber <version>` with no commit suffix. With a
/// recorded commit the line names its first 7 characters.
#[test]
fn version_prints_the_version_alone_without_a_recorded_commit() {
    let release = Release::serve();
    let version = release.fiber(&["--version"], &[]);
    assert_eq!(version.code, Some(0), "{}", version.stderr);
    match option_env!("FIBER_COMMIT").filter(|commit| !commit.is_empty()) {
        Some(commit) => assert_eq!(
            version.stdout,
            format!("fiber {VERSION} ({})\n", commit.get(..7).unwrap_or(commit)),
            "--version names the recorded commit's first 7 characters"
        ),
        None => assert_eq!(
            version.stdout,
            format!("fiber {VERSION}\n"),
            "--version without a recorded commit is the version alone"
        ),
    }
}

/// `nightly-version` bumps the `fiber` crate's patch and appends the
/// date (`docs/releasing.md`, "Cutting a release"): a tree at `0.0.0`
/// with `20261020` prints `0.0.1-nightly.20261020`.
#[test]
fn nightly_version_bumps_patch_and_appends_the_date() {
    let pack = Pack::start();
    pack.manifest("0.0.0");
    let tree = pack.path("tree").to_string_lossy().into_owned();
    let run = pack.pack(&["nightly-version", &tree, "20261020"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "0.0.1-nightly.20261020\n");
}

/// The bump carries across `0.0.9` to `0.0.10`: the compare is numeric,
/// not lexical.
#[test]
fn nightly_version_bumps_nine_to_ten() {
    let pack = Pack::start();
    pack.manifest("0.0.9");
    let tree = pack.path("tree").to_string_lossy().into_owned();
    let run = pack.pack(&["nightly-version", &tree, "20261020"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "0.0.10-nightly.20261020\n");
}

/// A missing manifest and a bad date fail with exit 2 naming the fault.
#[test]
fn nightly_version_refuses_a_bad_tree_or_date() {
    let pack = Pack::start();
    let tree = pack.path("tree").to_string_lossy().into_owned();
    let run = pack.pack(&["nightly-version", &tree, "20261020"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(
        run.stderr.contains("holds no fiber version"),
        "{}",
        run.stderr
    );

    let pack = Pack::start();
    pack.manifest("0.0.0");
    let tree = pack.path("tree").to_string_lossy().into_owned();
    for date in ["2026-10-20", "abcdefgh", "2026102", "202610200", ""] {
        let run = pack.pack(&["nightly-version", &tree, date]);
        assert_eq!(run.code, Some(2), "{date:?}: {}", run.stderr);
        assert!(run.stderr.contains(date), "{date:?}: {}", run.stderr);
    }
}

/// A version that is not three dot-separated numbers fails with exit 2
/// naming the version: `1.2` has only two components, so the split and
/// rejoin no longer round-trips; `1..3`, `.2.3` and `1.2.` each pin one
/// empty-component alternative of the numeric check, while the rest fail
/// the numeric check itself.
#[test]
fn nightly_version_refuses_a_non_xyz_version() {
    for version in [
        "1.2", "1", "1.2.3.4", "a.b.c", "1.2.x", "1..3", ".2.3", "1.2.",
    ] {
        let pack = Pack::start();
        pack.manifest(version);
        let tree = pack.path("tree").to_string_lossy().into_owned();
        let run = pack.pack(&["nightly-version", &tree, "20261020"]);
        assert_eq!(run.code, Some(2), "{version:?}: {}", run.stderr);
        assert_eq!(
            run.stderr,
            format!("release-pack: {version} is not a version such as 0.0.0.\n")
        );
    }
}

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn tag_line(sha: &str, tag: &str) -> String {
    format!("{sha}\trefs/tags/{tag}\n")
}

/// `nightly-needed` prints `publish` when HEAD is ahead of the newest
/// nightly tag.
#[test]
fn nightly_needed_publishes_when_head_is_ahead() {
    let pack = Pack::start();
    let input = tag_line(SHA_A, "v0.0.1-nightly.20261018");
    let stdin = pack.wfile("stdin.txt", input.as_bytes(), 0o644);
    let run = pack.tool(&script(), &["nightly-needed", SHA_B], Some(&stdin));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "publish\n");
}

/// `nightly-needed` prints `skip` naming the newest nightly tag if and
/// only if HEAD equals that tag's commit.
#[test]
fn nightly_needed_skips_only_at_the_newest_tag() {
    let pack = Pack::start();
    let input = tag_line(SHA_A, "v0.0.1-nightly.20261018");
    let stdin = pack.wfile("stdin.txt", input.as_bytes(), 0o644);
    let run = pack.tool(&script(), &["nightly-needed", SHA_A], Some(&stdin));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "skip: main unchanged since v0.0.1-nightly.20261018\n"
    );
}

/// An older nightly tag at HEAD while a newer tag exists elsewhere still
/// publishes: the skip compares against the newest tag only.
#[test]
fn nightly_needed_publishes_when_only_an_older_tag_is_at_head() {
    let pack = Pack::start();
    let input = format!(
        "{}{}",
        tag_line(SHA_A, "v0.0.1-nightly.20261018"),
        tag_line(SHA_B, "v0.0.1-nightly.20261019")
    );
    let stdin = pack.wfile("stdin.txt", input.as_bytes(), 0o644);
    let run = pack.tool(&script(), &["nightly-needed", SHA_A], Some(&stdin));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "publish\n");
}

/// An empty tag list, non-nightly tags only, and malformed lines all
/// publish: there is no nightly to be unchanged since.
#[test]
fn nightly_needed_publishes_without_any_nightly_tag() {
    for input in [
        String::new(),
        tag_line(SHA_A, "v0.0.9"),
        format!("{}{}", tag_line(SHA_A, "v0.0.9"), "not a ls-remote line\n"),
    ] {
        let pack = Pack::start();
        let stdin = pack.wfile("stdin.txt", input.as_bytes(), 0o644);
        let run = pack.tool(&script(), &["nightly-needed", SHA_A], Some(&stdin));
        assert_eq!(run.code, Some(0), "{input:?}: {}", run.stderr);
        assert_eq!(run.stdout, "publish\n", "{input:?}");
    }
}

/// The newest tag is the version-date maximum: `0.0.10-nightly.*` beats
/// `0.0.9-nightly.*` whatever the dates say.
#[test]
fn nightly_needed_compares_versions_numerically() {
    let pack = Pack::start();
    let input = format!(
        "{}{}",
        tag_line(SHA_A, "v0.0.9-nightly.20261020"),
        tag_line(SHA_B, "v0.0.10-nightly.20261018")
    );
    let stdin = pack.wfile("stdin.txt", input.as_bytes(), 0o644);
    let at_newest = pack.tool(&script(), &["nightly-needed", SHA_B], Some(&stdin));
    assert_eq!(at_newest.code, Some(0), "{}", at_newest.stderr);
    assert_eq!(
        at_newest.stdout,
        "skip: main unchanged since v0.0.10-nightly.20261018\n"
    );
    let at_older = pack.tool(&script(), &["nightly-needed", SHA_A], Some(&stdin));
    assert_eq!(at_older.code, Some(0), "{}", at_older.stderr);
    assert_eq!(at_older.stdout, "publish\n");
}

/// `nightly-notes` prints the `git log` range since the stable tag, one
/// `- <short> <subject>` line per commit. The repository is the run's
/// own directory, as the workflow's checkout is.
#[test]
fn nightly_notes_prints_the_range_since_the_stable_tag() {
    let pack = Pack::start();
    let repo = pack.root.path().to_path_buf();
    fakes::git::git(&repo, &["init", "--quiet"]);
    pack.wfile("note-src/one.txt", b"one", 0o644);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "first"]);
    fakes::git::git(&repo, &["tag", "v0.0.9"]);
    pack.wfile("note-src/two.txt", b"two", 0o644);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "second"]);
    let head = fakes::git::git(&repo, &["rev-parse", "HEAD"]);
    let run = pack.pack(&["nightly-notes", "v0.0.9", &head]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1);
    assert!(run.stdout.starts_with("- "), "{}", run.stdout);
    assert!(run.stdout.contains("second"), "{}", run.stdout);
}

/// With no stable tag yet the notes are the whole log, capped at 50.
#[test]
fn nightly_notes_without_a_stable_tag_prints_the_log() {
    let pack = Pack::start();
    let repo = pack.root.path().to_path_buf();
    fakes::git::git(&repo, &["init", "--quiet"]);
    pack.wfile("note-src/one.txt", b"one", 0o644);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "first"]);
    let head = fakes::git::git(&repo, &["rev-parse", "HEAD"]);
    let run = pack.pack(&["nightly-notes", "", &head]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1);
    assert!(run.stdout.contains("first"), "{}", run.stdout);
}

/// An empty range never prints nothing: the stable tag at HEAD prints one
/// line for HEAD itself.
#[test]
fn nightly_notes_at_the_stable_tag_prints_head_itself() {
    let pack = Pack::start();
    let repo = pack.root.path().to_path_buf();
    fakes::git::git(&repo, &["init", "--quiet"]);
    pack.wfile("note-src/one.txt", b"one", 0o644);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "first"]);
    fakes::git::git(&repo, &["tag", "v0.0.9"]);
    let head = fakes::git::git(&repo, &["rev-parse", "HEAD"]);
    let run = pack.pack(&["nightly-notes", "v0.0.9", &head]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1);
    assert!(run.stdout.contains("first"), "{}", run.stdout);
}

// debt: git setup mirrors nightly_notes_prints_the_range_since_the_stable_tag; fold the two into one helper when Pack::tool takes a working directory.
// The mirror is deliberate: the ticket orders an additive test and the bug rule needs the red proof standing alone.
/// `nightly-notes` reads the repository at `GITHUB_WORKSPACE`, so the
/// publish job can run it from outside the checkout.
#[test]
fn nightly_notes_outside_the_checkout_reads_the_workspace() {
    let pack = Pack::start();
    let repo = pack.root.path().to_path_buf();
    fakes::git::git(&repo, &["init", "--quiet"]);
    pack.wfile("note-src/one.txt", b"one", 0o644);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "first"]);
    fakes::git::git(&repo, &["tag", "v0.0.9"]);
    pack.wfile("note-src/two.txt", b"two", 0o644);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "second"]);
    let head = fakes::git::git(&repo, &["rev-parse", "HEAD"]);
    let outside = fakes::TempDir::new("rlp-outside");
    let path = format!("{}:/usr/bin:/bin", fixture_bin().display());
    // debt: hand-rolled Command mirrors Pack::tool field for field; switch to Pack::tool when it takes a working directory and extra env.
    let mut command = Command::new(script());
    command
        .process_group(0)
        .args(["nightly-notes", "v0.0.9", &head])
        .current_dir(outside.path())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", path)
        .env("HOME", pack.root.path())
        .env("TMPDIR", pack.path("tmp"))
        .env("RELEASE_PACK_STATE", pack.state())
        .env("GITHUB_WORKSPACE", &repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let run = run_watched(&mut command, pack.deadline, "nightly-notes to exit");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1);
    assert!(run.stdout.starts_with("- "), "{}", run.stdout);
    assert!(run.stdout.contains("second"), "{}", run.stdout);
}

/// `draft-begin --prerelease` carries `--prerelease` through to the
/// `gh release create` call.
#[test]
fn draft_begin_prerelease_marks_the_release() {
    let pack = Pack::start();
    pack.wfile("notes.md", b"notes\n", 0o644);
    let notes = pack.path("notes.md").to_string_lossy().into_owned();
    let run = pack.pack(&[
        "draft-begin",
        "--prerelease",
        "0.0.1-nightly.20261020",
        &"c".repeat(40),
        &notes,
    ]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let create = pack
        .calls()
        .lines()
        .find(|line| line.starts_with("create"))
        .unwrap()
        .to_owned();
    assert!(create.contains("--prerelease"), "{create}");
    assert!(
        create.contains("v0.0.1-nightly.20261020 --draft --prerelease"),
        "{create}"
    );
}
