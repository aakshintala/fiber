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
        self.tool(&script(), args)
    }

    /// Runs `tool` with `args` under the test's deadline.
    #[track_caller]
    fn tool(&self, tool: &Path, args: &[&str]) -> Output {
        let path = format!("{}:/usr/bin:/bin", fixture_bin().display());
        let mut command = Command::new(tool);
        command
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", path)
            .env("HOME", self.root.path())
            .env("TMPDIR", self.path("tmp"))
            .env("RELEASE_PACK_STATE", self.state())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = support::spawn_watched(&mut command);
        let group = child.id();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "release-pack to exit"),
        };
        watchdog.stand_down(self.deadline.cleanup());
        Output::from(output)
    }

    /// The members of `archive`, sorted, as `tar -tzf` lists them.
    #[track_caller]
    fn members(&self, archive: &str) -> Vec<String> {
        let at = self.path(&format!("out/{archive}"));
        let run = self.tool(Path::new("tar"), &["-tzf", &at.to_string_lossy()]);
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
    assert_eq!(
        pack.pack(&["draft-upload", "1.2.3", "out/missing-does-not-matter"])
            .code,
        Some(0)
    );
    // The failure step deletes the draft, with the tag its creation made.
    let run = pack.pack(&["draft-delete", "1.2.3"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!pack.state().join("draft-1.2.3").exists());
    assert!(!pack.state().join("published-1.2.3").exists());
    assert!(
        !pack.calls().lines().any(|line| line.starts_with("edit")),
        "publish ran before the failure step: {}",
        pack.calls()
    );
    let delete = pack
        .calls()
        .lines()
        .find(|line| line.starts_with("delete"))
        .unwrap()
        .to_owned();
    assert!(delete.contains("--cleanup-tag"), "{delete}");
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
