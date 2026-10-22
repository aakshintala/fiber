//! `cargo xtask site --out <dir>` against a fixture git repository: tag
//! selection and the front-page copy. Each test builds its own repository
//! so the tag set is exactly the case under test.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); a failure is the test's"
)]

use std::process::{Command, Stdio};
use std::time::Duration;

use common::git;
use fakes::TempDir;

#[path = "common/mod.rs"]
mod common;

/// How long a test waits for one child; a passing run never waits on it,
/// it only bounds a hang.
const WITHIN: Duration = Duration::from_secs(60);

/// The front-page tree the builder copies, without its README.
const COPIED: [&str; 5] = [
    "index.html",
    "style.css",
    "site.js",
    "fiber-logo.png",
    "fiber-tui.png",
];

/// A fixture repository with `research/site-prototype/` committed.
struct Fixture {
    repo: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let repo = TempDir::new("xtask-site");
        git(&repo, &["init", "-q", "-b", "main"]);
        Self { repo }
    }

    /// Writes `content` to `research/site-prototype/<name>` in the working
    /// tree and commits it.
    fn commit(&self, name: &str, content: &[u8], message: &str) {
        let path = self.repo.path().join("research/site-prototype").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", message]);
    }

    fn commit_text(&self, name: &str, content: &str, message: &str) {
        self.commit(name, content.as_bytes(), message);
    }

    fn tag(&self, tag: &str) {
        git(&self.repo, &["tag", tag]);
    }

    fn tag_annotated(&self, tag: &str) {
        git(&self.repo, &["tag", "-a", "-m", tag, tag]);
    }

    /// The first front page: every copied file holds its own name.
    fn commit_front_page(&self, marker: &str) {
        for name in COPIED {
            self.commit_text(name, &format!("{marker}:{name}\n"), marker);
        }
        self.commit_text("README.md", &format!("{marker} prototype readme\n"), marker);
    }
}

/// Runs the built `cargo xtask site` with `args` in the fixture repository.
fn site(repo: &TempDir, args: &[&str]) -> (Option<i32>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .arg("site")
        .args(args)
        .current_dir(repo.path())
        .stdin(Stdio::null());
    common::run("xtask site", command, &[], WITHIN)
}

fn site_out(repo: &TempDir, out: &std::path::Path) -> (Option<i32>, String) {
    site(repo, &["--out", &out.display().to_string()])
}

fn read_out(out: &std::path::Path, name: &str) -> Vec<u8> {
    std::fs::read(out.join(name)).unwrap()
}

#[test]
fn nightly_only_builds_the_nightly_front_page() {
    let fixture = Fixture::new();
    fixture.commit_front_page("nightly-one");
    fixture.tag("v0.1.0-nightly.20261023");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"nightly-one:index.html\n".to_vec(),
        "{text}"
    );
}

#[test]
fn stable_wins_over_a_nightly_on_an_earlier_commit() {
    let fixture = Fixture::new();
    fixture.commit_front_page("nightly-one");
    fixture.tag("v0.1.0-nightly.20261023");
    fixture.commit_text("index.html", "stable:index.html\n", "stable front");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"stable:index.html\n".to_vec()
    );
}

#[test]
fn a_working_tree_edit_after_tagging_is_not_in_the_output() {
    let fixture = Fixture::new();
    fixture.commit_front_page("tagged");
    fixture.tag("v0.1.0");
    fixture.commit_text("index.html", "uncommitted:index.html\n", "later");
    // Leave the edit uncommitted on top is wrong: commit then dirty the
    // working tree without committing, so the tag still names "tagged".
    let path = fixture
        .repo
        .path()
        .join("research/site-prototype/index.html");
    std::fs::write(&path, b"dirty:index.html\n").unwrap();
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    // The committed-but-untagged "later" edit is not at the tag either.
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"tagged:index.html\n".to_vec()
    );
}

#[test]
fn higher_versions_win_numerically_not_lexically() {
    let fixture = Fixture::new();
    fixture.commit_front_page("v019");
    fixture.tag("v0.1.9");
    fixture.commit_front_page("v0110");
    fixture.tag("v0.1.10");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"v0110:index.html\n".to_vec(),
        "v0.1.10 beats v0.1.9: {text}"
    );
}

#[test]
fn newer_nightly_dates_win_and_minor_beats_patch_nightly() {
    let fixture = Fixture::new();
    fixture.commit_front_page("old-minor-new-date");
    fixture.tag("v0.1.9-nightly.20261231");
    fixture.commit_front_page("oct31");
    fixture.tag("v0.2.0-nightly.20261031");
    fixture.commit_front_page("nov01");
    fixture.tag("v0.2.0-nightly.20261101");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    // v0.2.0-nightly.20261101 beats v0.2.0-nightly.20261031 beats
    // v0.1.9-nightly.20261231: the base version matters first.
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"nov01:index.html\n".to_vec()
    );
}

#[test]
fn annotated_and_lightweight_tags_on_one_commit_agree() {
    let fixture = Fixture::new();
    fixture.commit_front_page("both");
    fixture.tag("v0.1.0-nightly.20261023");
    fixture.tag_annotated("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"both:index.html\n".to_vec()
    );
}

#[test]
fn prerelease_and_other_tags_are_ignored() {
    let fixture = Fixture::new();
    fixture.commit_front_page("real");
    fixture.tag("v0.1.0");
    fixture.tag("v0.1.0-rc1");
    fixture.tag("docs-x");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"real:index.html\n".to_vec()
    );
}

#[test]
fn unparsable_tags_are_ignored_without_failing() {
    let fixture = Fixture::new();
    fixture.commit_front_page("real");
    fixture.tag("v0.1.0");
    fixture.tag("v99999999999999999999.0.0");
    fixture.tag("v01.2.3");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"real:index.html\n".to_vec()
    );
}

#[test]
fn a_numeric_tie_breaks_by_tag_name() {
    let fixture = Fixture::new();
    fixture.commit_text("index.html", "tie:index.html\n", "tie");
    fixture.commit_text("style.css", "tie\n", "tie");
    fixture.commit_text("site.js", "tie\n", "tie");
    fixture.commit_text("fiber-logo.png", "tie\n", "tie");
    fixture.commit_text("fiber-tui.png", "tie\n", "tie");
    fixture.commit_text("README.md", "tie\n", "tie");
    fixture.tag("v0.1.0-nightly.20261023");
    // Distinct tags never share a numeric key, so the name break is only
    // reached on exact ties; the unit tests in site.rs pin it. Here the
    // greater date wins, fixing which tag the tie-break would keep.
    fixture.tag("v0.1.0-nightly.20261024");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"tie:index.html\n".to_vec()
    );
}

#[test]
fn readme_is_absent_and_every_other_file_is_copied() {
    let fixture = Fixture::new();
    fixture.commit_front_page("files");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    for name in COPIED {
        assert_eq!(
            read_out(&out_dir, name),
            format!("files:{name}\n").into_bytes(),
            "{name} comes from the tag"
        );
    }
    assert!(
        !out_dir.join("README.md").exists(),
        "the prototype README is excluded: {text}"
    );
}

#[test]
fn no_tags_exits_1_naming_no_release_tag() {
    let fixture = Fixture::new();
    fixture.commit_front_page("untagged");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(1), "{text}");
    assert!(
        text.contains("site: no release tag to build from."),
        "{text}"
    );
}

#[test]
fn non_empty_out_exits_1_naming_the_dir() {
    let fixture = Fixture::new();
    fixture.commit_front_page("tagged");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("stale.txt"), b"stale\n").unwrap();
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains(&out_dir.display().to_string()), "{text}");
}

#[test]
fn two_runs_from_the_same_commit_write_equal_trees() {
    let fixture = Fixture::new();
    fixture.commit_front_page("same");
    fixture.tag("v0.1.0-nightly.20261023");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let first = out.path().join("first");
    let second = out.path().join("second");
    let (code, text) = site_out(&fixture.repo, &first);
    assert_eq!(code, Some(0), "{text}");
    let (code, text) = site_out(&fixture.repo, &second);
    assert_eq!(code, Some(0), "{text}");
    for name in COPIED {
        assert_eq!(
            std::fs::read(first.join(name)).unwrap(),
            std::fs::read(second.join(name)).unwrap(),
            "{name} is tag-derived only"
        );
    }
}
