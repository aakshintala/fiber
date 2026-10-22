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

    /// Writes `content` to `docs/<name>` in the working tree and commits it.
    fn commit_docs(&self, name: &str, content: &[u8], message: &str) {
        let path = self.repo.path().join("docs").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", message]);
    }

    fn commit_docs_text(&self, name: &str, content: &str, message: &str) {
        self.commit_docs(name, content.as_bytes(), message);
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
    // The tag still names "tagged": the later commit is untagged and the
    // working tree holds an uncommitted edit.
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
fn empty_out_is_fresh_and_builds() {
    let fixture = Fixture::new();
    fixture.commit_front_page("tagged");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    std::fs::create_dir_all(&out_dir).unwrap();
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(
        read_out(&out_dir, "index.html"),
        b"tagged:index.html\n".to_vec(),
        "{text}"
    );
}

#[test]
fn file_out_exits_1_naming_not_a_directory() {
    let fixture = Fixture::new();
    fixture.commit_front_page("tagged");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    std::fs::write(&out_dir, b"stale\n").unwrap();
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("is not a directory."), "{text}");
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

/// A guide page with a table, links of every kind the rewriter handles,
/// and a repeated heading.
const INSTALL_STABLE: &str = "# Install\n\n| one | two |\n| --- | --- |\n| a | b |\n\nSee [the other page](other.md#two-words), [a crate file](../../crates/x.rs) and [the web](https://example.com).\n\n## Two Words\n\nStable first.\n\n## Two Words\n\nStable second.\n";

const INSTALL_NIGHTLY: &str = "# Install\n\n| one | two |\n| --- | --- |\n| a | b |\n\nSee [the other page](other.md#two-words), [a crate file](../../crates/x.rs) and [the web](https://example.com).\n\n## Two Words\n\nNightly first.\n\n## Two Words\n\nNightly second.\n";

/// A stable tag and a nightly tag with different docs, style and logo.
fn tag_both_trees(fixture: &Fixture) {
    fixture.commit_front_page("stable-front");
    fixture.commit_docs_text("user/install.md", INSTALL_STABLE, "stable docs");
    fixture.commit_docs_text("user/other.md", "# Other\n", "stable other");
    fixture.commit_docs_text("user/README.md", "# Guide\n", "stable guide");
    fixture.commit_docs_text("top.md", "# Top\n", "stable top");
    fixture.commit_docs_text(
        "deep/nested/page.md",
        "# Nested\n\nBack [up](../../top.md).\n",
        "stable nested",
    );
    fixture.tag("v0.4.0");
    fixture.commit_front_page("nightly-front");
    fixture.commit_docs_text("user/install.md", INSTALL_NIGHTLY, "nightly docs");
    fixture.tag("v0.4.1-nightly.20261101");
}

fn read_site(out_dir: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(out_dir.join(name)).unwrap()
}

#[test]
fn docs_render_to_html_with_rewritten_links_tables_and_heading_ids() {
    let fixture = Fixture::new();
    tag_both_trees(&fixture);
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");

    let stable = read_site(&out_dir, "user/install.html");
    assert!(stable.contains("<table"), "tables render: {text}");
    assert!(
        stable.contains("href=\"other.html#two-words\""),
        "same-directory links become html: {stable}"
    );
    assert!(
        stable.contains("href=\"https://github.com/aakshintala/fiber/blob/v0.4.0/crates/x.rs\""),
        "links leaving docs/ name the stable tag: {stable}"
    );
    assert!(
        stable.contains("href=\"https://example.com\""),
        "external links are untouched: {stable}"
    );
    assert!(
        stable.contains("id=\"two-words\"") && stable.contains("id=\"two-words-1\""),
        "repeated headings dedup like the docs check: {stable}"
    );
    assert!(
        stable.contains("Stable first.") && !stable.contains("Nightly first."),
        "the stable tree holds the stable tag's docs: {text}"
    );
    assert!(
        stable.contains("<title>Install</title>"),
        "the title is the first heading: {stable}"
    );
    assert!(
        stable.contains("href=\"../style.css\"") && stable.contains("href=\"../docs.css\""),
        "a nested page links both stylesheets by relative path: {stable}"
    );

    let nightly = read_site(&out_dir, "nightly/user/install.html");
    assert!(
        nightly.contains(
            "href=\"https://github.com/aakshintala/fiber/blob/v0.4.1-nightly.20261101/crates/x.rs\""
        ),
        "the nightly tree names the nightly tag: {nightly}"
    );
    assert!(
        nightly.contains("Nightly first.") && !nightly.contains("Stable first."),
        "the nightly tree holds the nightly tag's docs: {text}"
    );

    let nested = read_site(&out_dir, "deep/nested/page.html");
    assert!(
        nested.contains("href=\"../../top.html\""),
        "links climb directories relatively: {nested}"
    );
    assert!(
        nested.contains("href=\"../../style.css\"") && nested.contains("href=\"../../docs.css\""),
        "a deep page links both stylesheets by relative path: {nested}"
    );
    let top = read_site(&out_dir, "top.html");
    assert!(
        top.contains("href=\"style.css\"") && top.contains("href=\"docs.css\""),
        "a root page links both stylesheets by relative path: {top}"
    );
    assert!(
        top.contains("<img src=\"fiber-logo.png\""),
        "a root page reaches the logo relatively: {top}"
    );
    assert!(
        stable.contains("<img src=\"../fiber-logo.png\""),
        "a nested page reaches the logo relatively: {stable}"
    );
}

#[test]
fn each_tree_carries_its_own_tags_style_logo_and_docs() {
    let fixture = Fixture::new();
    tag_both_trees(&fixture);
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");

    assert_eq!(
        read_site(&out_dir, "index.html"),
        "stable-front:index.html\n",
        "the root is the front page at the stable tag: {text}"
    );
    assert_eq!(
        read_site(&out_dir, "style.css"),
        "stable-front:style.css\n",
        "{text}"
    );
    assert_eq!(
        read_site(&out_dir, "nightly/style.css"),
        "nightly-front:style.css\n",
        "the nightly tree carries its own tag's style: {text}"
    );
    assert_eq!(
        std::fs::read(out_dir.join("nightly/fiber-logo.png")).unwrap(),
        b"nightly-front:fiber-logo.png\n",
        "the nightly tree carries its own tag's logo: {text}"
    );
    assert!(
        out_dir.join("user/index.html").is_file(),
        "a README directory gains an index: {text}"
    );
    assert!(
        out_dir.join("nightly/user/index.html").is_file(),
        "a nightly README directory gains an index: {text}"
    );
    assert!(
        read_site(&out_dir, "user/index.html").contains("Guide"),
        "{text}"
    );
    let root_css = std::fs::read(out_dir.join("docs.css")).unwrap();
    let nightly_css = std::fs::read(out_dir.join("nightly/docs.css")).unwrap();
    assert_eq!(
        root_css, nightly_css,
        "docs.css comes from the deploying commit: {text}"
    );
    assert!(
        String::from_utf8(root_css).unwrap().contains(".fiber-docs"),
        "docs.css scopes under the docs shell: {text}"
    );
}

#[test]
fn the_nightly_index_is_a_contents_page_starting_with_the_guide() {
    let fixture = Fixture::new();
    tag_both_trees(&fixture);
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");

    let index = read_site(&out_dir, "nightly/index.html");
    assert!(
        index.contains("<p><a href=\"user/\">Guide</a></p>"),
        "the guide paragraph links user/ by the guide's title: {index}"
    );
    let guide = index.find("href=\"user/\"").unwrap();
    let install = index.find("href=\"user/install.html\"").unwrap();
    assert!(guide < install, "the guide link comes first: {index}");
    for wanted in ["Install", "Other", "Guide", "Top", "Nested"] {
        assert!(
            index.contains(wanted),
            "every page is linked by its heading: {index}"
        );
    }
    for page in [
        "user/install.html",
        "user/other.html",
        "user/index.html",
        "top.html",
        "deep/nested/page.html",
    ] {
        assert!(
            index.contains(page),
            "every rendered page is listed: {index}"
        );
    }
}

#[test]
fn a_stable_and_a_nightly_tag_on_one_commit_render_both_trees() {
    let fixture = Fixture::new();
    fixture.commit_front_page("both");
    fixture.commit_docs_text("user/install.md", INSTALL_STABLE, "docs");
    fixture.commit_docs_text("user/README.md", "# Guide\n", "guide");
    fixture.tag("v0.1.0");
    fixture.tag("v0.1.0-nightly.20261023");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert!(
        read_site(&out_dir, "user/install.html").contains("Stable first."),
        "{text}"
    );
    assert!(
        read_site(&out_dir, "nightly/user/install.html").contains("Stable first."),
        "both trees render from the one commit: {text}"
    );
    assert!(out_dir.join("nightly/index.html").is_file(), "{text}");
}

#[test]
fn no_nightly_tag_writes_no_nightly_tree() {
    let fixture = Fixture::new();
    fixture.commit_front_page("stable");
    fixture.commit_docs_text("user/install.md", INSTALL_STABLE, "docs");
    fixture.tag("v0.1.0");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert!(!out_dir.join("nightly").exists(), "{text}");
    assert!(
        read_site(&out_dir, "user/install.html").contains("Stable first."),
        "{text}"
    );
}

#[test]
fn nightly_pages_open_with_a_banner_while_stable_pages_carry_none() {
    let fixture = Fixture::new();
    tag_both_trees(&fixture);
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");

    let nightly = read_site(&out_dir, "nightly/user/install.html");
    assert!(
        nightly.contains("Nightly docs, v0.4.1-nightly.20261101."),
        "the banner names the nightly tag: {nightly}"
    );
    assert!(
        nightly.contains("href=\"../../user/\""),
        "the banner links the stable guide: {nightly}"
    );
    assert!(
        nightly.find("Nightly docs").unwrap() < nightly.find("Nightly first.").unwrap(),
        "the page opens with the banner: {text}"
    );
    assert!(
        read_site(&out_dir, "nightly/index.html").contains("href=\"../user/\""),
        "the contents page banners the stable guide too: {text}"
    );
    assert!(
        !read_site(&out_dir, "user/install.html").contains("Nightly docs"),
        "stable pages carry no banner: {text}"
    );
}

#[test]
fn before_the_first_stable_the_root_tree_has_no_banner() {
    let fixture = Fixture::new();
    fixture.commit_front_page("nightly-front");
    fixture.commit_docs_text("user/install.md", INSTALL_NIGHTLY, "nightly docs");
    fixture.commit_docs_text("user/README.md", "# Guide\n", "nightly guide");
    fixture.tag("v0.1.0-nightly.20261023");
    let out = TempDir::new("xtask-site-out");
    let out_dir = out.path().join("site");
    let (code, text) = site_out(&fixture.repo, &out_dir);
    assert_eq!(code, Some(0), "{text}");
    assert!(
        !read_site(&out_dir, "user/install.html").contains("Nightly docs"),
        "the root tree never banners, even from a nightly tag: {text}"
    );
    assert!(
        read_site(&out_dir, "nightly/user/install.html")
            .contains("Nightly docs, v0.1.0-nightly.20261023."),
        "the nightly tree banners: {text}"
    );
}
