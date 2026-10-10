//! `docs/extensions.md`, "Names", "Versions", "Installing" and "Staying
//! current": install, update, remove and list by name, fetched with the
//! system `git` from repositories made in a temporary directory. No network.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use contract::clock::Clock;

use common::{Setup, drive, manifest, provider, write};
use contract::ErrorCode;
use extensions::{Error, Installed, Origin, Provenance, Request, SHORT_NAMES, list, plan, removal};
use serde_json::{Value, json};

const FIBER: &str = "0.1.0";

/// Removes `typed` and what nothing else needs, as `fiber extension remove` does
/// without a terminal, and returns the names removed.
fn uninstall(home: &Path, typed: &str) -> Result<Vec<String>, Error> {
    let removal = removal(home, typed, &*fakes::clock::FakeClock::new())?;
    let names = removal.names.clone();
    removal.commit()?;
    Ok(names)
}

fn commit(i: &Installed) -> Option<String> {
    match &i.provenance {
        Provenance::Git { commit } => Some(commit.clone()),
        Provenance::Path(_) => None,
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Repositories by name, in the setup's directory.
struct Repos<'a> {
    setup: &'a Setup,
    paths: BTreeMap<String, PathBuf>,
}

impl<'a> Repos<'a> {
    fn new(setup: &'a Setup) -> Self {
        Self {
            setup,
            paths: BTreeMap::new(),
        }
    }

    /// A repository `host/owner/repo` holding an extension in `dir` (empty
    /// for its root), committed and tagged `tag`. A later call commits
    /// again.
    fn tag(&mut self, repo: &str, dir: &str, tag: &str, manifest: &Value, extra: &[(&str, &str)]) {
        let path = self.setup.root().join("repos").join(repo.replace('/', "-"));
        if !path.exists() {
            fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "--quiet"]);
        }
        let ext = path.join(dir);
        write(&ext.join("extension.json"), &manifest.to_string());
        write(
            &ext.join("providers/acme.json"),
            &provider("acme", &["m1"]).to_string(),
        );
        for (file, text) in extra {
            write(&ext.join(file), text);
        }
        git(&path, &["add", "."]);
        git(
            &path,
            &["commit", "--quiet", "--allow-empty", "-m", "release"],
        );
        if !tag.is_empty() {
            git(&path, &["tag", tag]);
        }
        self.paths.insert(repo.to_owned(), path);
    }

    fn commit(&self, repo: &str, tag: &str) -> String {
        git(
            &self.paths[repo],
            &["rev-parse", &format!("{tag}^{{commit}}")],
        )
    }

    fn origin(&self) -> Origin {
        let paths = self.paths.clone();
        Origin::new("git", move |repo| match paths.get(repo) {
            Some(path) => format!("file://{}", path.display()),
            None => "file:///nonexistent-repository".into(),
        })
    }
}

fn named(name: &str, depends: &[(&str, &str)]) -> Value {
    let mut m = manifest(name);
    m["depends"] = json!(depends.iter().copied().collect::<BTreeMap<_, _>>());
    m
}

fn install(setup: &Setup, repos: &Repos, name: &str) -> Result<Vec<String>, Error> {
    plan(
        &setup.home(),
        &Request::Install(name.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )?
    .commit()
}

fn versions(setup: &Setup) -> BTreeMap<String, String> {
    list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed
        .into_iter()
        .map(|i| (i.name, i.version))
        .collect()
}

fn dirs(setup: &Setup) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(setup.home().join("extensions"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.retain(|n| !n.starts_with('.'));
    found.sort();
    found
}

const LIB: &str = "example.com/acme/lib";

#[test]
fn an_install_fetches_the_newest_tag_and_records_its_commit() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[("old.txt", "x")]);
    repos.tag(LIB, "", "v1.1.0", &manifest(LIB), &[]);
    assert_eq!(install(&setup, &repos, LIB).unwrap(), [LIB]);
    let installed = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].version, "v1.1.0");
    assert_eq!(commit(&installed[0]).unwrap(), repos.commit(LIB, "v1.1.0"));
    assert!(installed[0].requested);
    let dir = setup.home().join("extensions/example.com-acme-lib");
    assert!(dir.join("providers/acme.json").is_file());
    assert!(!dir.join(".git").exists());
    assert_eq!(dirs(&setup), ["example.com-acme-lib"]);
}

#[test]
fn an_extension_in_a_directory_of_a_repository_installs_from_there() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let name = "example.com/acme/mono/tools/lint";
    repos.tag(
        "example.com/acme/mono",
        "tools/lint",
        "v0.2.0",
        &manifest(name),
        &[],
    );
    install(&setup, &repos, name).unwrap();
    assert!(
        setup
            .home()
            .join("extensions/example.com-acme-mono-tools-lint/extension.json")
            .is_file()
    );
}

#[test]
fn a_git_marker_names_a_repository_inside_subgroups() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let repo = "gitlab.com/group/subgroup/repo.git";
    let name = "gitlab.com/group/subgroup/repo.git/ext";
    repos.tag(repo, "ext", "v1.0.0", &manifest(name), &[]);
    install(&setup, &repos, name).unwrap();
    let dir = setup
        .home()
        .join("extensions/gitlab.com-group-subgroup-repo.git-ext");
    assert!(dir.join("extension.json").is_file());
    let listed = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, name);
    assert_eq!(uninstall(&setup.home(), name).unwrap(), [name]);
    assert!(!dir.exists());
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_short_name_installs_the_first_party_extension() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let name = "github.com/aakshintala/fiber/providers/muse";
    repos.tag(
        "github.com/aakshintala/fiber",
        "providers/muse",
        "v0.1.0",
        &manifest(name),
        &[],
    );
    assert_eq!(install(&setup, &repos, "muse").unwrap(), [name]);
    assert_eq!(extensions::full_name("muse"), name);
    assert_eq!(dirs(&setup), ["muse"]);
}

#[test]
fn memory_lives_under_its_short_name_and_a_third_party_under_its_slug() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let memory = "github.com/aakshintala/fiber/extensions/memory";
    let lint = "github.com/acme/lint";
    repos.tag(
        "github.com/aakshintala/fiber",
        "extensions/memory",
        "v0.1.0",
        &manifest(memory),
        &[],
    );
    repos.tag(lint, "", "v1.0.0", &manifest(lint), &[]);
    assert_eq!(install(&setup, &repos, "memory").unwrap(), [memory]);
    assert_eq!(install(&setup, &repos, lint).unwrap(), [lint]);
    assert_eq!(dirs(&setup), ["github.com-acme-lint", "memory"]);
    let home = setup.home();
    let mine = [
        home.join("data/memory/index.md"),
        home.join("projects/p/data/memory/index.md"),
        home.join("config/memory.json"),
        home.join("projects/p/config/memory.json"),
    ];
    let theirs = [
        home.join("data/github.com-acme-lint/index"),
        home.join("config/github.com-acme-lint.json"),
    ];
    for file in mine.iter().chain(&theirs) {
        write(file, "x");
    }
    removal(&home, "memory", &*fakes::clock::FakeClock::new())
        .unwrap()
        .commit()
        .unwrap();
    assert!(mine.iter().all(|f| !f.exists()));
    assert!(!home.join("extensions/memory").exists());
    assert!(!home.join("data/memory").exists());
    assert!(!home.join("projects/p/data/memory").exists());
    assert!(theirs.iter().all(|f| f.exists()));
    assert_eq!(dirs(&setup), ["github.com-acme-lint"]);
}

#[test]
fn every_short_name_asks_for_its_directory_of_the_fiber_repository() {
    for short in SHORT_NAMES {
        let setup = Setup::new();
        let mut repos = Repos::new(&setup);
        let name = format!("github.com/aakshintala/fiber/providers/{short}");
        repos.tag(
            "github.com/aakshintala/fiber",
            &format!("providers/{short}"),
            "v1.0.0",
            &manifest(&name),
            &[],
        );
        assert_eq!(install(&setup, &repos, short).unwrap(), [name], "{short}");
    }
}

#[test]
fn a_dependency_gets_the_lowest_version_meeting_every_minimum() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/oauth-helper";
    for tag in ["v1.0.0", "v1.2.0", "v1.4.0", "v1.9.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    let (a, b) = ("example.com/acme/openrouter", "example.com/acme/databricks");
    repos.tag(a, "", "v1.0.0", &named(a, &[(dep, "v1.2.0")]), &[]);
    repos.tag(b, "", "v1.0.0", &named(b, &[(dep, "1.4")]), &[]);
    install(&setup, &repos, a).unwrap();
    assert_eq!(versions(&setup)[dep], "v1.2.0");
    assert!(
        !list(&setup.home(), &*fakes::clock::FakeClock::new())
            .unwrap()
            .installed
            .iter()
            .find(|i| i.name == dep)
            .unwrap()
            .requested
    );
    // A second dependent raises the minimum, and the dependency moves up.
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Install(b.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    )
    .unwrap();
    let moved: Vec<_> = p.items().filter(|i| i.name == dep).collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].changes, None);
    p.commit().unwrap();
    assert_eq!(versions(&setup)[dep], "v1.4.0");
    let listed = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    let requested: Vec<_> = listed
        .iter()
        .map(|i| (i.name.as_str(), i.requested))
        .collect();
    assert_eq!(requested, [(b, true), (dep, false), (a, true)]);
}

#[test]
fn a_dependency_asked_for_by_name_stays_requested_when_it_moves_up() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    for tag in ["v1.0.0", "v1.5.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    let clock = fakes::clock::FakeClock::new();
    let first = plan(
        &setup.home(),
        &Request::Install(dep.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    );
    first.unwrap().commit().unwrap();
    repos.tag(dep, "", "v1.6.0", &manifest(dep), &[]);
    let top = "example.com/acme/top";
    repos.tag(top, "", "v1.0.0", &named(top, &[(dep, "1.6")]), &[]);
    install(&setup, &repos, top).unwrap();
    let listed = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    let dep_row = listed.iter().find(|i| i.name == dep).unwrap();
    assert_eq!(
        (dep_row.version.as_str(), dep_row.requested),
        ("v1.6.0", true)
    );
}

#[test]
fn an_extensions_directory_that_cannot_be_listed_is_io_failed() {
    let setup = Setup::new();
    write(&setup.home().join("extensions"), "not a directory");
    assert_eq!(
        list(&setup.home(), &*fakes::clock::FakeClock::new())
            .unwrap_err()
            .code(),
        ErrorCode::IoFailed
    );
}

#[test]
fn two_dependents_in_one_plan_settle_on_the_higher_minimum() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/oauth-helper";
    for tag in ["v1.2.0", "v1.4.0", "v1.9.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    let (top, a, b) = (
        "example.com/acme/top",
        "example.com/acme/a",
        "example.com/acme/b",
    );
    repos.tag(a, "", "v1.0.0", &named(a, &[(dep, "1.2")]), &[]);
    repos.tag(b, "", "v1.0.0", &named(b, &[(dep, "1.4")]), &[]);
    repos.tag(
        top,
        "",
        "v1.0.0",
        &named(top, &[(a, "1.0"), (b, "1.0")]),
        &[],
    );
    install(&setup, &repos, top).unwrap();
    let got = versions(&setup);
    assert_eq!(got[dep], "v1.4.0");
    assert_eq!(got.len(), 4);
}

#[test]
fn an_installed_dependency_that_meets_the_minimum_is_not_changed() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/oauth-helper";
    for tag in ["v1.2.0", "v1.4.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    install(&setup, &repos, dep).unwrap();
    assert_eq!(versions(&setup)[dep], "v1.4.0");
    let top = "example.com/acme/top";
    repos.tag(top, "", "v1.0.0", &named(top, &[(dep, "1.2")]), &[]);
    install(&setup, &repos, top).unwrap();
    assert_eq!(versions(&setup)[dep], "v1.4.0");
    assert!(
        list(&setup.home(), &*fakes::clock::FakeClock::new())
            .unwrap()
            .installed
            .iter()
            .find(|i| i.name == dep)
            .unwrap()
            .requested
    );
}

#[test]
fn two_majors_of_one_dependency_stop_the_install_naming_both() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/oauth-helper";
    for tag in ["v1.2.0", "v2.0.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    let (a, b) = ("example.com/acme/a", "example.com/acme/b");
    repos.tag(a, "", "v1.0.0", &named(a, &[(dep, "1.2")]), &[]);
    repos.tag(b, "", "v1.0.0", &named(b, &[(dep, "2.0")]), &[]);
    install(&setup, &repos, a).unwrap();
    let before = dirs(&setup);
    let err = install(&setup, &repos, b).unwrap_err();
    let text = err.to_string();
    for part in [a, dep, "1.2"] {
        assert!(text.contains(part), "{text}");
    }
    assert!(text.contains(b) && text.contains("2.0"), "{text}");
    assert_eq!(err.code(), ErrorCode::VersionConflict);
    assert_eq!(dirs(&setup), before);
    assert_eq!(versions(&setup)[dep], "v1.2.0");
}

#[test]
fn a_failed_install_installs_nothing() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    repos.tag(dep, "", "v1.0.0", &manifest(dep), &[]);
    let top = "example.com/acme/top";
    // No tag of `dep` is 3.0 or later.
    repos.tag(top, "", "v1.0.0", &named(top, &[(dep, "3.0")]), &[]);
    let err = install(&setup, &repos, top).unwrap_err();
    assert!(matches!(err, Error::NoVersion { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::VersionConflict);
    assert!(dirs(&setup).is_empty());

    // A dependency that needs a newer Fiber.
    let mut newer = manifest(dep);
    newer["fiber"] = json!("9.0.0");
    repos.tag(dep, "", "v1.1.0", &newer, &[]);
    repos.tag(top, "", "v1.1.0", &named(top, &[(dep, "1.1")]), &[]);
    let err = install(&setup, &repos, top).unwrap_err();
    assert!(matches!(err, Error::NeedsNewerFiber { .. }), "{err}");
    assert!(dirs(&setup).is_empty());

    // A dependency that is not in any repository.
    let ghost = "example.com/acme/ghost";
    repos.tag(top, "", "v1.2.0", &named(top, &[(ghost, "1.0")]), &[]);
    install(&setup, &repos, top).unwrap_err();
    assert!(dirs(&setup).is_empty());
}

#[test]
fn the_fetch_leaves_nothing_in_the_temporary_directory_or_home() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[]);
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    )
    .unwrap();
    assert_eq!(p.items().count(), 1);
    drop(p);
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_manifest_that_names_another_extension_is_refused() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest("example.com/acme/other"), &[]);
    let err = install(&setup, &repos, LIB).unwrap_err();
    assert!(matches!(err, Error::WrongName { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::ExtensionNotFound);
    assert!(dirs(&setup).is_empty());
}

#[test]
fn an_install_from_a_path_resolves_its_dependencies_from_git() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    repos.tag(dep, "", "v1.0.0", &manifest(dep), &[]);
    repos.tag(dep, "", "v1.3.0", &manifest(dep), &[]);
    let source = setup.source("local", &named("local", &[(dep, "1.2")]), &[]);
    plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let got = versions(&setup);
    assert_eq!(got["local"], "v1.0.0");
    assert_eq!(got[dep], "v1.3.0");
}

#[test]
fn an_update_moves_to_the_newest_tag_and_shows_what_changed() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[("a.lua", "return 1")]);
    install(&setup, &repos, LIB).unwrap();
    let first = repos.commit(LIB, "v1.0.0");
    repos.tag(LIB, "", "v1.1.0", &manifest(LIB), &[("b.lua", "return 2")]);
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Update(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    )
    .unwrap();
    let item = p.items().next().unwrap();
    assert_eq!(item.version, "v1.1.0");
    let changes = item.changes.clone().unwrap();
    assert!(changes.contains("b.lua"), "{changes}");
    // The plan holds the lock, so the record is read directly: planning
    // leaves the installed commit alone.
    let record = fs::read_to_string(
        setup
            .home()
            .join("extensions")
            .join(config::dir_name(LIB))
            .join(".fiber.json"),
    )
    .unwrap();
    assert!(record.contains(&first), "{record}");
    p.commit().unwrap();
    let now = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    assert_eq!(now[0].version, "v1.1.0");
    assert_eq!(commit(&now[0]).unwrap(), repos.commit(LIB, "v1.1.0"));
}

#[test]
fn an_update_re_resolves_the_dependencies_and_an_uninstalled_name_is_refused() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    for tag in ["v1.0.0", "v1.5.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    repos.tag(LIB, "", "v1.0.0", &named(LIB, &[(dep, "1.0")]), &[]);
    let clock = fakes::clock::FakeClock::new();
    let err = plan(
        &setup.home(),
        &Request::Update(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    );
    assert!(matches!(err, Err(Error::NotInstalled { .. })));
    install(&setup, &repos, LIB).unwrap();
    assert_eq!(versions(&setup)[dep], "v1.0.0");
    repos.tag(LIB, "", "v1.1.0", &named(LIB, &[(dep, "1.5")]), &[]);
    plan(
        &setup.home(),
        &Request::Update(LIB.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(versions(&setup)[dep], "v1.5.0");
}

#[test]
fn a_remove_deletes_the_extension_and_the_dependencies_nothing_else_uses() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let (shared, own) = ("example.com/acme/shared", "example.com/acme/own");
    repos.tag(shared, "", "v1.0.0", &manifest(shared), &[]);
    repos.tag(own, "", "v1.0.0", &manifest(own), &[]);
    let (a, b) = ("example.com/acme/a", "example.com/acme/b");
    repos.tag(
        a,
        "",
        "v1.0.0",
        &named(a, &[(shared, "1.0"), (own, "1.0")]),
        &[],
    );
    repos.tag(b, "", "v1.0.0", &named(b, &[(shared, "1.0")]), &[]);
    install(&setup, &repos, a).unwrap();
    install(&setup, &repos, b).unwrap();
    assert_eq!(uninstall(&setup.home(), a).unwrap(), [a, own]);
    assert_eq!(
        versions(&setup).keys().cloned().collect::<Vec<_>>(),
        [b, shared]
    );
    assert_eq!(uninstall(&setup.home(), b).unwrap(), [b, shared]);
    assert!(
        list(&setup.home(), &*fakes::clock::FakeClock::new())
            .unwrap()
            .installed
            .is_empty()
    );
    let err = uninstall(&setup.home(), b).unwrap_err();
    assert!(matches!(err, Error::NotInstalled { .. }));
}

#[test]
fn a_remove_takes_the_record_with_the_directory() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[]);
    install(&setup, &repos, LIB).unwrap();
    assert!(
        setup
            .home()
            .join("extensions/example.com-acme-lib/.fiber.json")
            .is_file()
    );
    uninstall(&setup.home(), LIB).unwrap();
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_remove_by_short_name_finds_the_first_party_extension() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let name = "github.com/aakshintala/fiber/providers/muse";
    repos.tag(
        "github.com/aakshintala/fiber",
        "providers/muse",
        "v1.0.0",
        &manifest(name),
        &[],
    );
    install(&setup, &repos, "muse").unwrap();
    assert_eq!(uninstall(&setup.home(), "muse").unwrap(), [name]);
}

#[test]
fn a_local_install_lists_its_manifest_version_and_no_commit() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("local"), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    let listed = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    assert_eq!(listed[0].version, "v1.0.0");
    assert!(matches!(&listed[0].provenance, Provenance::Path(p) if p.is_absolute()));
}

#[test]
fn missing_git_fails_with_the_usage_code_and_says_to_install_it() {
    let setup = Setup::new();
    let origin = Origin::new("fiber-no-such-git-program", |repo| repo.to_owned());
    let clock = fakes::clock::FakeClock::new();
    let err = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &origin,
        &*clock,
    );
    let Err(err) = err else { panic!("planned") };
    assert!(matches!(err, Error::GitMissing));
    assert_eq!(err.code(), ErrorCode::Usage);
    assert!(err.to_string().contains("Install git"), "{err}");
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_missing_repository_is_extension_not_found() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    let err = install(&setup, &repos, LIB).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExtensionNotFound);
    let text = err.to_string();
    assert!(
        text.contains(LIB) && text.contains("was not found"),
        "{text}"
    );
    assert!(
        text.contains("does not appear to be a git repository"),
        "{text}"
    );
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_refused_connection_is_fetch_failed() {
    let setup = Setup::new();
    let origin = Origin::new("git", |repo| format!("https://127.0.0.1:1/{repo}"));
    let Err(err) = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &origin,
        &*fakes::clock::FakeClock::new(),
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::Git { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::FetchFailed);
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_name_that_is_not_where_and_what_is_refused_before_any_fetch() {
    let setup = Setup::new();
    let origin = Origin::new("fiber-no-such-git-program", |repo| repo.to_owned());
    let Err(err) = plan(
        &setup.home(),
        &Request::Install("nonsense".into()),
        FIBER,
        &origin,
        &*fakes::clock::FakeClock::new(),
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::BadName { .. }), "{err}");
}

// ---- Repairs: what a plan and its commit promise ----

#[test]
fn a_record_link_in_a_package_is_replaced_never_written_through() {
    let setup = Setup::new();
    let victim = setup.root().join("victim.txt");
    write(&victim, "precious");
    let source = setup.source("local", &manifest("acme"), &[]);
    std::os::unix::fs::symlink(&victim, source.join(".fiber.json")).unwrap();
    common::install(&setup.home(), &source, FIBER).unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    let record = setup.home().join("extensions/acme/.fiber.json");
    assert!(
        !fs::symlink_metadata(&record)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        list(&setup.home(), &*fakes::clock::FakeClock::new())
            .unwrap()
            .installed
            .len(),
        1
    );
}

#[test]
fn a_second_operation_while_one_holds_the_lock_fails_cleanly() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[]);
    let clock = fakes::clock::FakeClock::new();
    let started = clock.now();
    let held = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    )
    .unwrap();
    let Err(err) = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    ) else {
        panic!("a second plan")
    };
    // Fifty tries, ten milliseconds apart, and the first plan took the lock
    // without waiting.
    assert_eq!(
        clock.now().saturating_duration_since(started),
        Duration::from_millis(500)
    );
    assert!(matches!(err, Error::Busy), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
    assert!(matches!(
        removal(&setup.home(), LIB, &*fakes::clock::FakeClock::new()),
        Err(Error::Busy)
    ));
    // A list from another thread of this process waits for the lock too,
    // so it never reads a set the plan is part way through committing.
    let home = setup.home();
    let listed = std::thread::spawn(move || list(&home, &*fakes::clock::FakeClock::new()))
        .join()
        .unwrap();
    assert!(matches!(listed, Err(Error::Busy)), "{listed:?}");
    assert!(matches!(
        list(&setup.home(), &*fakes::clock::FakeClock::new()),
        Err(Error::Busy)
    ));
    assert!(setup.home().join("extensions/.lock").is_file());
    assert!(!setup.home().join(".extensions.lock").exists());
    // The first plan is untouched by the second's refusal.
    assert_eq!(held.commit().unwrap(), [LIB]);
    assert_eq!(versions(&setup).len(), 1);
    uninstall(&setup.home(), LIB).unwrap();
}

#[test]
fn a_plan_that_is_dropped_releases_the_lock_and_removes_what_it_fetched() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[]);
    let clock = fakes::clock::FakeClock::new();
    let first = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    )
    .unwrap();
    let staged = first.items().next().unwrap().staged().to_path_buf();
    assert!(staged.is_dir());
    drop(first);
    assert!(!staged.exists());
    install(&setup, &repos, LIB).unwrap();
}

#[test]
fn two_names_with_one_directory_are_refused_naming_both() {
    let setup = Setup::new();
    let one = setup.source("one", &manifest("example.com/a-b/c"), &[]);
    let two = setup.source("two", &manifest("example.com/a/b-c"), &[]);
    common::install(&setup.home(), &one, FIBER).unwrap();
    let err = common::install(&setup.home(), &two, FIBER).unwrap_err();
    assert!(matches!(err, Error::SlugTaken { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::Usage);
    let text = err.to_string();
    assert!(
        text.contains("example.com/a-b/c") && text.contains("example.com/a/b-c"),
        "{text}"
    );
    assert_eq!(
        versions(&setup).keys().collect::<Vec<_>>(),
        ["example.com/a-b/c"]
    );
}

#[test]
fn a_long_chain_of_dependencies_installs() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let name = |n: usize| format!("example.com/acme/d{n:02}");
    for n in 0..14 {
        let deps = if n < 13 {
            vec![(name(n + 1), "1.0".to_owned())]
        } else {
            vec![]
        };
        let deps: Vec<(&str, &str)> = deps.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        repos.tag(&name(n), "", "v1.0.0", &named(&name(n), &deps), &[]);
    }
    assert_eq!(install(&setup, &repos, &name(0)).unwrap().len(), 14);
    assert_eq!(versions(&setup).len(), 14);
}

#[test]
fn a_dependency_cycle_ends() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let (a, b) = ("example.com/acme/a", "example.com/acme/b");
    repos.tag(a, "", "v1.0.0", &named(a, &[(b, "1.0")]), &[]);
    repos.tag(b, "", "v1.0.0", &named(b, &[(a, "1.0")]), &[]);
    assert_eq!(install(&setup, &repos, a).unwrap(), [a, b]);
}

/// Verify counterexample: `r→z1,zz1`, `z1→d1`, `zz1→z1.5,d2`, `z1.5→d2`.
/// `d` sorts before `z`, so the 1-against-2 conflict is visible while `z` 1
/// is still staged. It is not final: `z` 1.5 replaces that manifest first.
#[test]
fn a_stale_manifest_conflict_is_not_final_until_the_fixpoint() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (r, z, zz, d) = (n("r"), n("z"), n("zz"), n("d"));
    for tag in ["v1.0.0", "v2.0.0"] {
        repos.tag(&d, "", tag, &manifest(&d), &[]);
    }
    repos.tag(&z, "", "v1.0.0", &named(&z, &[(&d, "1")]), &[]);
    repos.tag(&z, "", "v1.5.0", &named(&z, &[(&d, "2")]), &[]);
    repos.tag(
        &zz,
        "",
        "v1.0.0",
        &named(&zz, &[(&z, "1.5"), (&d, "2")]),
        &[],
    );
    repos.tag(&r, "", "v1.0.0", &named(&r, &[(&z, "1"), (&zz, "1")]), &[]);
    install(&setup, &repos, &r).unwrap();
    let got = versions(&setup);
    assert_eq!(got[&z], "v1.5.0", "{got:?}");
    assert_eq!(got[&zz], "v1.0.0", "{got:?}");
    assert_eq!(got[&d], "v2.0.0", "{got:?}");
    assert_eq!(got[&r], "v1.0.0", "{got:?}");
}

/// Verify counterexample: `r→z1,zz1`, `z1→a` (missing), `zz1→z1.5`, `z1.5`
/// has no dependencies. Fetching `a` fails before `zz` is fetched; `a` is
/// not reachable once `z` 1.5 replaces `z` 1, so the failure is not reported.
#[test]
fn a_fetch_failure_of_an_unreachable_dependency_is_not_final() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (r, z, zz, a) = (n("r"), n("z"), n("zz"), n("a"));
    repos.tag(&z, "", "v1.0.0", &named(&z, &[(&a, "1")]), &[]);
    repos.tag(&z, "", "v1.5.0", &manifest(&z), &[]);
    repos.tag(&zz, "", "v1.0.0", &named(&zz, &[(&z, "1.5")]), &[]);
    repos.tag(&r, "", "v1.0.0", &named(&r, &[(&z, "1"), (&zz, "1")]), &[]);
    install(&setup, &repos, &r).unwrap();
    let got = versions(&setup);
    assert_eq!(got[&z], "v1.5.0", "{got:?}");
    assert_eq!(got[&zz], "v1.0.0", "{got:?}");
    assert!(!got.contains_key(&a), "{got:?}");
}

/// The same shape as the missing-repository case, except `a` exists and is
/// fetched from the manifest `z` 1.5 then drops. It must not stay installed.
#[test]
fn a_dependency_fetched_from_a_replaced_manifest_is_not_installed() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (r, z, zz, a) = (n("r"), n("z"), n("zz"), n("a"));
    repos.tag(&a, "", "v1.0.0", &manifest(&a), &[]);
    repos.tag(&z, "", "v1.0.0", &named(&z, &[(&a, "1")]), &[]);
    repos.tag(&z, "", "v1.5.0", &manifest(&z), &[]);
    repos.tag(&zz, "", "v1.0.0", &named(&zz, &[(&z, "1.5")]), &[]);
    repos.tag(&r, "", "v1.0.0", &named(&r, &[(&z, "1"), (&zz, "1")]), &[]);
    install(&setup, &repos, &r).unwrap();
    let got = versions(&setup);
    assert_eq!(got[&z], "v1.5.0", "{got:?}");
    assert!(!got.contains_key(&a), "{got:?}");
}

#[test]
fn a_replaced_manifest_neither_conflicts_nor_leaves_its_dependencies_in_the_plan() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (r, a, b, d, x) = (n("r"), n("a"), n("b"), n("d"), n("x"));
    for tag in ["v1.0.0", "v2.0.0"] {
        repos.tag(&d, "", tag, &manifest(&d), &[]);
    }
    repos.tag(&x, "", "v1.0.0", &manifest(&x), &[]);
    // `a` 1.0 needs `d` 1.x and `x`; `a` 1.5 needs `d` 2.x only.
    repos.tag(
        &a,
        "",
        "v1.0.0",
        &named(&a, &[(&d, "1.0"), (&x, "1.0")]),
        &[],
    );
    repos.tag(&a, "", "v1.5.0", &named(&a, &[(&d, "2.0")]), &[]);
    repos.tag(&b, "", "v1.0.0", &named(&b, &[(&a, "1.5")]), &[]);
    repos.tag(
        &r,
        "",
        "v1.0.0",
        &named(&r, &[(&a, "1.0"), (&b, "1.0")]),
        &[],
    );
    install(&setup, &repos, &r).unwrap();
    let got = versions(&setup);
    assert_eq!(got[&a], "v1.5.0");
    assert_eq!(got[&d], "v2.0.0");
    assert!(!got.contains_key(&x), "{got:?}");
}

#[test]
fn updating_a_dependency_an_installed_dependent_pins_to_an_older_major_stops() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let (dep, user) = ("example.com/acme/dep", "example.com/acme/user");
    repos.tag(dep, "", "v1.2.0", &manifest(dep), &[]);
    repos.tag(user, "", "v1.0.0", &named(user, &[(dep, "1.2")]), &[]);
    install(&setup, &repos, user).unwrap();
    repos.tag(dep, "", "v2.0.0", &manifest(dep), &[]);
    let Err(err) = plan(
        &setup.home(),
        &Request::Update(dep.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::MajorConflict { .. }), "{err}");
    assert!(err.to_string().contains(user), "{err}");
    assert_eq!(versions(&setup)[dep], "v1.2.0");
}

#[test]
fn an_update_by_name_is_kept_when_a_later_install_allows_an_older_version() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    for tag in ["v1.0.0", "v1.5.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    let (a, b) = ("example.com/acme/a", "example.com/acme/b");
    repos.tag(a, "", "v1.0.0", &named(a, &[(dep, "1.0")]), &[]);
    install(&setup, &repos, a).unwrap();
    assert_eq!(versions(&setup)[dep], "v1.0.0");
    plan(
        &setup.home(),
        &Request::Update(dep.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(versions(&setup)[dep], "v1.5.0");
    repos.tag(b, "", "v1.0.0", &named(b, &[(dep, "1.0")]), &[]);
    install(&setup, &repos, b).unwrap();
    assert_eq!(versions(&setup)[dep], "v1.5.0");
}

#[test]
fn a_requested_major_stops_an_install_that_needs_another() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    for tag in ["v1.0.0", "v2.0.0"] {
        repos.tag(dep, "", tag, &manifest(dep), &[]);
    }
    install(&setup, &repos, dep).unwrap();
    assert_eq!(versions(&setup)[dep], "v2.0.0");
    let user = "example.com/acme/user";
    repos.tag(user, "", "v1.0.0", &named(user, &[(dep, "1.0")]), &[]);
    let err = install(&setup, &repos, user).unwrap_err();
    assert!(matches!(err, Error::MajorConflict { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::VersionConflict);
    let text = err.to_string();
    assert!(
        text.contains("your install") && text.contains(user),
        "{text}"
    );
    assert_eq!(versions(&setup)[dep], "v2.0.0");
    assert!(!versions(&setup).contains_key(user));
}

#[test]
fn an_update_replaces_the_minimum_from_the_version_it_moves_off() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    repos.tag(dep, "", "v1.0.0", &manifest(dep), &[]);
    install(&setup, &repos, dep).unwrap();
    repos.tag(dep, "", "v2.0.0", &manifest(dep), &[]);
    plan(
        &setup.home(),
        &Request::Update(dep.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(versions(&setup)[dep], "v2.0.0");
}

#[test]
fn a_repository_with_no_version_tag_is_refused() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "", &manifest(LIB), &[]);
    repos.tag(LIB, "", "latest", &manifest(LIB), &[]);
    let err = install(&setup, &repos, LIB).unwrap_err();
    assert!(matches!(err, Error::NoTag { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::ExtensionNotFound);
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_local_record_holds_the_absolute_path_and_an_update_checks_the_name() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    let roundabout = source.join("../local");
    common::install(&setup.home(), &roundabout, FIBER).unwrap();
    let listed = list(&setup.home(), &*fakes::clock::FakeClock::new())
        .unwrap()
        .installed;
    assert_eq!(
        listed[0].provenance,
        Provenance::Path(fs::canonicalize(&source).unwrap())
    );
    // The same directory now holds another extension.
    write(
        &source.join("extension.json"),
        &manifest("other").to_string(),
    );
    let Err(err) = plan(
        &setup.home(),
        &Request::Update("acme".into()),
        FIBER,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::WrongName { .. }), "{err}");
    assert_eq!(versions(&setup).keys().collect::<Vec<_>>(), ["acme"]);
    // And once it is the same extension again, the update goes through.
    let mut newer = manifest("acme");
    newer["version"] = json!("v1.1.0");
    write(&source.join("extension.json"), &newer.to_string());
    plan(
        &setup.home(),
        &Request::Update("acme".into()),
        FIBER,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(versions(&setup)["acme"], "v1.1.0");
}

#[test]
fn a_missing_or_unreadable_record_lists_the_extension_as_damaged() {
    for damage in [
        None,
        Some("{"),
        Some(r#"{"name":"acme","version":"v1","requested":true,"source":{}}"#),
        Some(r#"{"name":"acme","version":"v1.0.0","source":{"path":"/tmp/x"}}"#),
    ] {
        let setup = Setup::new();
        let healthy = setup.source("healthy", &manifest("example.com/acme/healthy"), &[]);
        let broken = setup.source("broken", &manifest("example.com/acme/broken"), &[]);
        common::install(&setup.home(), &healthy, FIBER).unwrap();
        common::install(&setup.home(), &broken, FIBER).unwrap();
        let dir = setup.home().join("extensions/example.com-acme-broken");
        match damage {
            None => fs::remove_file(dir.join(".fiber.json")).unwrap(),
            Some(text) => write(&dir.join(".fiber.json"), text),
        }
        let listing = list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
        assert_eq!(
            listing
                .installed
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["example.com/acme/healthy"],
            "{damage:?}"
        );
        assert_eq!(listing.damaged.len(), 1, "{damage:?}");
        assert_eq!(
            listing.damaged[0].name, "example.com/acme/broken",
            "{damage:?}"
        );
        let shown = listing.damaged[0].to_string();
        assert!(shown.contains("example.com/acme/broken"), "{shown}");
        assert!(!shown.contains(".fiber.json"), "{shown}");
    }
}

#[test]
fn a_damaged_extension_without_a_manifest_is_named_by_its_directory() {
    let setup = Setup::new();
    let healthy = setup.source("healthy", &manifest("example.com/acme/healthy"), &[]);
    common::install(&setup.home(), &healthy, FIBER).unwrap();
    let dir = setup.home().join("extensions/some-dir");
    fs::create_dir_all(&dir).unwrap();
    let listing = list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(listing.installed.len(), 1);
    assert_eq!(listing.damaged.len(), 1);
    assert_eq!(listing.damaged[0].name, "some-dir");
}

#[test]
fn hidden_directories_and_stray_files_are_not_damaged() {
    let setup = Setup::new();
    let healthy = setup.source("healthy", &manifest("example.com/acme/healthy"), &[]);
    common::install(&setup.home(), &healthy, FIBER).unwrap();
    write(
        &setup.home().join("extensions/.acme.1.new/extension.json"),
        "{}",
    );
    write(&setup.home().join("extensions/stray"), "x");
    let listing = list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(
        listing
            .installed
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        ["example.com/acme/healthy"]
    );
    assert!(listing.damaged.is_empty());
}

#[test]
fn a_damaged_messages_name_the_extension_and_the_fix() {
    let setup = Setup::new();
    let name = "github.com/aakshintala/fiber/providers/opencode";
    let source = setup.source("broken", &manifest(name), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    let dir = setup.home().join("extensions/opencode");
    fs::remove_file(dir.join(".fiber.json")).unwrap();
    let listing = list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(listing.damaged.len(), 1);
    assert_eq!(listing.damaged[0].name, name);
    assert_eq!(
        listing.damaged[0].to_string(),
        "`opencode` is damaged; run `fiber extension remove opencode`, then install it again."
    );
    let shown = listing.damaged[0].to_string();
    assert!(!shown.contains(setup.home().to_str().unwrap()), "{shown}");
    assert!(!shown.contains("os error"), "{shown}");
    assert_eq!(
        listing.damaged[0].skipped(),
        "`opencode` is damaged, so its dependency minimums are unknown and the versions chosen did not count them; run `fiber extension remove opencode`, then install it again."
    );
}

#[test]
fn damaged_extensions_list_sorted_by_name() {
    let setup = Setup::new();
    for (dir, name) in [
        ("b-src", "example.com/acme/b"),
        ("a-src", "example.com/acme/a"),
    ] {
        let source = setup.source(dir, &manifest(name), &[]);
        common::install(&setup.home(), &source, FIBER).unwrap();
    }
    for slug in ["example.com-acme-a", "example.com-acme-b"] {
        fs::remove_file(setup.home().join(format!("extensions/{slug}/.fiber.json"))).unwrap();
    }
    let listing = list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
    assert!(listing.installed.is_empty());
    assert_eq!(
        listing
            .damaged
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["example.com/acme/a", "example.com/acme/b"]
    );
}

#[test]
fn a_healthy_record_with_an_unreadable_manifest_is_still_a_hard_error() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    let dir = setup.home().join("extensions/acme");
    let kept = fs::read_to_string(dir.join("extension.json")).unwrap();
    write(&dir.join("extension.json"), "{");
    let err = list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{err}");
    write(&dir.join("extension.json"), &kept);
}

#[test]
fn installing_another_extension_with_a_damaged_one_present_succeeds_and_names_it() {
    let setup = Setup::new();
    let keeper = setup.source("keeper", &manifest("example.com/acme/keeper"), &[]);
    let broken = setup.source("broken", &manifest("example.com/acme/broken"), &[]);
    common::install(&setup.home(), &keeper, FIBER).unwrap();
    common::install(&setup.home(), &broken, FIBER).unwrap();
    fs::remove_file(
        setup
            .home()
            .join("extensions/example.com-acme-broken/.fiber.json"),
    )
    .unwrap();
    let fresh = setup.source("fresh", &manifest("example.com/acme/fresh"), &[]);
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Path(fresh),
        FIBER,
        &Origin::github(),
        &*clock,
    )
    .unwrap();
    assert_eq!(
        p.damaged()
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["example.com/acme/broken"]
    );
    p.commit().unwrap();
    assert_eq!(
        versions(&setup).keys().cloned().collect::<Vec<_>>(),
        ["example.com/acme/fresh", "example.com/acme/keeper"]
    );
    assert!(
        setup
            .home()
            .join("extensions/example.com-acme-broken")
            .is_dir()
    );
}

#[test]
fn install_or_update_of_a_damaged_name_fails_as_damaged() {
    for damaged_record in [true, false] {
        let setup = Setup::new();
        let name = "github.com/aakshintala/fiber/providers/opencode";
        let source = setup.source("broken", &manifest(name), &[]);
        common::install(&setup.home(), &source, FIBER).unwrap();
        let dir = setup.home().join("extensions/opencode");
        if damaged_record {
            fs::remove_file(dir.join(".fiber.json")).unwrap();
        } else {
            write(&dir.join(".fiber.json"), "{");
        }
        let fix =
            "`opencode` is damaged; run `fiber extension remove opencode`, then install it again.";
        for request in [
            Request::Install(name.into()),
            Request::Update("opencode".into()),
        ] {
            let Err(err) = plan(
                &setup.home(),
                &request,
                FIBER,
                &Origin::github(),
                &*fakes::clock::FakeClock::new(),
            ) else {
                panic!("planned")
            };
            assert!(matches!(err, Error::Damaged(_)), "{err}");
            assert_eq!(err.code(), ErrorCode::ExtensionFailed, "{err}");
            assert_eq!(err.to_string(), fix, "{err}");
        }
    }
}

#[test]
fn a_dependency_on_a_damaged_slug_fails_as_damaged() {
    let setup = Setup::new();
    let dep = setup.source("dep", &manifest("example.com/acme/dep"), &[]);
    common::install(&setup.home(), &dep, FIBER).unwrap();
    fs::remove_file(
        setup
            .home()
            .join("extensions/example.com-acme-dep/.fiber.json"),
    )
    .unwrap();
    let top = setup.source(
        "top",
        &named("example.com/acme/top", &[("example.com/acme/dep", "1.0")]),
        &[],
    );
    let Err(err) = plan(
        &setup.home(),
        &Request::Path(top),
        FIBER,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::Damaged(_)), "{err}");
    assert_eq!(err.code(), ErrorCode::ExtensionFailed, "{err}");
}

#[test]
fn a_damaged_extension_removes_by_its_displayed_name() {
    for invalid in [false, true] {
        let setup = Setup::new();
        let name = "github.com/aakshintala/fiber/providers/opencode";
        let slug = "opencode";
        let healthy = setup.source("healthy", &manifest("example.com/acme/healthy"), &[]);
        let broken = setup.source("broken", &manifest(name), &[]);
        common::install(&setup.home(), &healthy, FIBER).unwrap();
        common::install(&setup.home(), &broken, FIBER).unwrap();
        let dir = setup.home().join(format!("extensions/{slug}"));
        if invalid {
            write(&dir.join(".fiber.json"), "{");
        } else {
            fs::remove_file(dir.join(".fiber.json")).unwrap();
        }
        let home = setup.home();
        let mine = [
            home.join(format!("data/{slug}/index")),
            home.join(format!("projects/p/data/{slug}/index")),
            home.join(format!("config/{slug}.json")),
            home.join(format!("projects/p/config/{slug}.json")),
        ];
        let theirs = home.join("data/example.com-acme-healthy/index");
        for file in mine.iter().chain([&theirs]) {
            write(file, "x");
        }
        let r = removal(&home, "opencode", &*fakes::clock::FakeClock::new()).unwrap();
        assert_eq!(r.names, [name], "invalid={invalid}");
        r.commit().unwrap();
        assert!(!dir.exists(), "invalid={invalid}");
        assert!(mine.iter().all(|f| !f.exists()), "invalid={invalid}");
        assert!(theirs.exists(), "invalid={invalid}");
        assert!(
            home.join("extensions/example.com-acme-healthy").exists(),
            "invalid={invalid}"
        );
    }
}

#[test]
fn a_damaged_extension_removes_by_its_full_name() {
    let setup = Setup::new();
    let name = "example.com/acme/broken";
    let broken = setup.source("broken", &manifest(name), &[]);
    common::install(&setup.home(), &broken, FIBER).unwrap();
    fs::remove_file(
        setup
            .home()
            .join("extensions/example.com-acme-broken/.fiber.json"),
    )
    .unwrap();
    let r = removal(&setup.home(), name, &*fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(r.names, [name]);
    r.commit().unwrap();
    assert!(
        !setup
            .home()
            .join("extensions/example.com-acme-broken")
            .exists()
    );
}

#[test]
fn removing_a_damaged_extension_also_removes_its_orphaned_dependency() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    repos.tag(dep, "", "v1.0.0", &manifest(dep), &[]);
    let top = "example.com/acme/top";
    repos.tag(top, "", "v1.0.0", &named(top, &[(dep, "1.0")]), &[]);
    let keeper = "example.com/acme/keeper";
    repos.tag(keeper, "", "v1.0.0", &manifest(keeper), &[]);
    install(&setup, &repos, top).unwrap();
    install(&setup, &repos, keeper).unwrap();
    // Damaging the parent orphans its dependency: nothing healthy
    // needs it now.
    fs::remove_file(
        setup
            .home()
            .join("extensions/example.com-acme-top/.fiber.json"),
    )
    .unwrap();
    assert_eq!(uninstall(&setup.home(), top).unwrap(), [top, dep]);
    assert_eq!(dirs(&setup), ["example.com-acme-keeper"]);
    assert_eq!(
        versions(&setup).keys().cloned().collect::<Vec<_>>(),
        [keeper]
    );
}

#[test]
fn a_remove_lists_and_then_deletes_the_data_and_settings_of_what_it_removes() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("example.com/acme/x"), &[]);
    let other = setup.source("other", &manifest("example.com/acme/y"), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    common::install(&setup.home(), &other, FIBER).unwrap();
    let home = setup.home();
    let mine = [
        home.join("data/example.com-acme-x/index"),
        home.join("projects/p/data/example.com-acme-x/index"),
        home.join("config/example.com-acme-x.json"),
        home.join("projects/p/config/example.com-acme-x.json"),
    ];
    let theirs = home.join("data/example.com-acme-y/index");
    for file in mine.iter().chain([&theirs]) {
        write(file, "x");
    }
    let r = removal(
        &home,
        "example.com/acme/x",
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(r.names, ["example.com/acme/x"]);
    let mut shown = r.data.clone();
    shown.sort();
    let mut expect: Vec<PathBuf> = vec![
        home.join("data/example.com-acme-x"),
        home.join("projects/p/data/example.com-acme-x"),
        home.join("config/example.com-acme-x.json"),
        home.join("projects/p/config/example.com-acme-x.json"),
    ];
    expect.sort();
    assert_eq!(shown, expect);
    drop(r);
    assert!(
        mine.iter().all(|f| f.exists()),
        "dropping a removal deletes nothing"
    );
    removal(
        &home,
        "example.com/acme/x",
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert!(mine.iter().all(|f| !f.exists()));
    assert!(!home.join("data/example.com-acme-x").exists());
    assert!(theirs.exists());
    assert!(home.join("extensions/example.com-acme-y").exists());
}

fn with_step(name: &str, step: &[&str]) -> Value {
    let mut m = manifest(name);
    m["install"] = json!(step);
    m
}

#[test]
fn an_install_step_runs_at_the_final_path_after_the_plan_and_again_on_update() {
    let setup = Setup::new();
    let marker = setup.root().join("ran");
    let step = format!(
        "echo ran >> '{}' && echo built > built.txt",
        marker.display()
    );
    let source = setup.source("local", &with_step("acme", &["sh", "-c", &step]), &[]);
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &Origin::github(),
        &*clock,
    )
    .unwrap();
    assert!(!marker.exists(), "the step must wait for the approval");
    p.commit().unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "ran\n");
    assert_eq!(
        fs::read_to_string(setup.home().join("extensions/acme/built.txt")).unwrap(),
        "built\n"
    );
    plan(
        &setup.home(),
        &Request::Update("acme".into()),
        FIBER,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "ran\nran\n");
}

#[test]
fn an_install_step_sees_its_final_path_on_install_and_on_update() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let name = "example.com/acme/x";
    let stepped = |payload: &str| {
        let mut m = with_step(
            name,
            &["sh", "-c", "pwd > where.txt; cat payload.txt > built.txt"],
        );
        m["version"] = json!(payload);
        m
    };
    repos.tag(
        name,
        "",
        "v1.0.0",
        &stepped("one"),
        &[("payload.txt", "one")],
    );
    install(&setup, &repos, name).unwrap();
    let dir = setup.home().join("extensions/example.com-acme-x");
    let canonical = fs::canonicalize(&dir).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("where.txt")).unwrap(),
        format!("{}\n", canonical.display()),
    );
    // The step ran after the new files were in place, not before.
    assert_eq!(fs::read_to_string(dir.join("built.txt")).unwrap(), "one");
    repos.tag(
        name,
        "",
        "v1.1.0",
        &stepped("two"),
        &[("payload.txt", "two")],
    );
    plan(
        &setup.home(),
        &Request::Update(name.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("where.txt")).unwrap(),
        format!("{}\n", canonical.display()),
    );
    assert_eq!(fs::read_to_string(dir.join("built.txt")).unwrap(), "two");
}

/// Every entry in `extensions/`, hidden or not, except the lock file,
/// which every operation leaves behind.
fn all_dirs(setup: &Setup) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(setup.home().join("extensions"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.retain(|n| n != ".lock");
    found.sort();
    found
}

#[test]
fn a_failing_step_on_update_keeps_the_previous_version_working() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let name = "example.com/acme/x";
    repos.tag(name, "", "v1.0.0", &manifest(name), &[("v1.txt", "v1")]);
    install(&setup, &repos, name).unwrap();
    let first = repos.commit(name, "v1.0.0");
    let mut bad = with_step(name, &["sh", "-c", "echo broken >&2; exit 3"]);
    bad["version"] = json!("v1.1.0");
    repos.tag(name, "", "v1.1.0", &bad, &[("v2.txt", "v2")]);
    let err = plan(
        &setup.home(),
        &Request::Update(name.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallExited { .. }), "{err}");
    assert!(err.to_string().contains("broken"), "{err}");
    let dir = setup.home().join("extensions/example.com-acme-x");
    assert_eq!(fs::read_to_string(dir.join("v1.txt")).unwrap(), "v1");
    assert!(!dir.join("v2.txt").exists());
    assert_eq!(versions(&setup)[name], "v1.0.0");
    assert!(
        fs::read_to_string(dir.join(".fiber.json"))
            .unwrap()
            .contains(&first),
        "the record still names the first commit"
    );
    assert_eq!(all_dirs(&setup), ["example.com-acme-x"]);
}

#[test]
fn a_failing_step_on_a_fresh_install_leaves_nothing() {
    let setup = Setup::new();
    let source = setup.source(
        "local",
        &with_step(
            "acme",
            &["sh", "-c", "mkdir built; echo broken >&2; exit 3"],
        ),
        &[],
    );
    let err = plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallExited { .. }), "{err}");
    // The step wrote into the fresh copy before it failed, and all of it
    // went with the rollback.
    assert_eq!(all_dirs(&setup), Vec::<String>::new());
}

#[test]
fn a_failing_step_in_one_item_puts_the_other_items_back() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    let top = "example.com/acme/top";
    repos.tag(dep, "", "v1.0.0", &manifest(dep), &[("v.txt", "dep-one")]);
    repos.tag(
        top,
        "",
        "v1.0.0",
        &named(top, &[(dep, "1.0")]),
        &[("v.txt", "top-one")],
    );
    install(&setup, &repos, top).unwrap();
    // A new top needs a new dep, but the top's own step fails: both stay.
    let mut dep_two = manifest(dep);
    dep_two["version"] = json!("v2.0.0");
    repos.tag(dep, "", "v2.0.0", &dep_two, &[("v.txt", "dep-two")]);
    let mut bad = with_step(top, &["sh", "-c", "echo broken >&2; exit 3"]);
    bad["depends"] = json!({ dep: "2.0" });
    bad["version"] = json!("v1.1.0");
    repos.tag(top, "", "v1.1.0", &bad, &[("v.txt", "top-two")]);
    let err = plan(
        &setup.home(),
        &Request::Update(top.into()),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallExited { .. }), "{err}");
    // The files are read before any listing: a listing would finish a
    // leftover journal, hiding a commit that did not roll itself back.
    assert_eq!(
        fs::read_to_string(setup.home().join("extensions/example.com-acme-dep/v.txt")).unwrap(),
        "dep-one"
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("extensions/example.com-acme-top/v.txt")).unwrap(),
        "top-one"
    );
    assert_eq!(versions(&setup)[dep], "v1.0.0");
    assert_eq!(versions(&setup)[top], "v1.0.0");
    assert_eq!(
        all_dirs(&setup),
        ["example.com-acme-dep", "example.com-acme-top"]
    );
}

#[test]
fn a_failing_install_step_aborts_with_nothing_installed_even_of_the_dependencies() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let dep = "example.com/acme/dep";
    repos.tag(dep, "", "v1.0.0", &manifest(dep), &[]);
    let mut bad = with_step("acme", &["sh", "-c", "echo nope >&2; exit 3"]);
    bad["depends"] = json!({ dep: "1.0" });
    let source = setup.source("local", &bad, &[]);
    let err = plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallExited { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::NonzeroExit, "{err}");
    assert!(err.to_string().contains("nope"), "{err}");
    assert!(dirs(&setup).is_empty());
    let missing = setup.source(
        "missing",
        &with_step("acme", &["no-such-program-here"]),
        &[],
    );
    let err = plan(
        &setup.home(),
        &Request::Path(missing),
        FIBER,
        &repos.origin(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallStep { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
    assert!(dirs(&setup).is_empty());
}

/// Serves `body` once. The receiver gets whether a request arrived and was
/// answered. A request that never comes leaves the thread blocked in
/// `accept`; dropping the receiver does not unblock it.
fn serve_once(body: &'static [u8]) -> (String, std::sync::mpsc::Receiver<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/tool-1.0", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => {
                match tx.send(false) {
                    Ok(()) | Err(std::sync::mpsc::SendError(_)) => {}
                }
                return;
            }
        };
        let mut seen = Vec::new();
        let mut buf = [0; 512];
        while !seen.ends_with(b"\r\n\r\n") {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => {
                    match tx.send(false) {
                        Ok(()) | Err(std::sync::mpsc::SendError(_)) => {}
                    }
                    return;
                }
                Ok(n) => n,
            };
            seen.extend_from_slice(&buf[..n]);
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let sent = stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body))
            .is_ok();
        match tx.send(sent) {
            Ok(()) | Err(std::sync::mpsc::SendError(_)) => {}
        }
    });
    (url, rx)
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn with_binary(platform: &str, url: &str, sha: &str) -> Value {
    let mut m = manifest("acme");
    m["binaries"] = json!({ platform: { "url": url, "sha256": sha } });
    m
}

#[test]
fn this_platforms_binary_is_downloaded_checked_and_made_executable() {
    use std::os::unix::fs::PermissionsExt;
    let setup = Setup::new();
    let (url, served) = serve_once(b"#!/bin/sh\necho hi\n");
    let m = with_binary(
        &extensions::platform(),
        &url,
        &sha256(b"#!/bin/sh\necho hi\n"),
    );
    let source = setup.source("local", &m, &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    assert!(
        served
            .recv_timeout(Duration::from_secs(10))
            .expect("waited for the binary to be downloaded")
    );
    let file = setup.home().join("extensions/acme/bin/tool-1.0");
    assert_eq!(fs::read(&file).unwrap(), b"#!/bin/sh\necho hi\n");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o111,
        0o111
    );
}

#[test]
fn a_binary_whose_checksum_differs_aborts_with_nothing_installed() {
    let setup = Setup::new();
    let (url, served) = serve_once(b"tampered");
    let m = with_binary(&extensions::platform(), &url, &sha256(b"original"));
    let source = setup.source("local", &m, &[]);
    let err = common::install(&setup.home(), &source, FIBER).unwrap_err();
    assert!(matches!(err, Error::BinaryChecksum { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
    assert!(
        served
            .recv_timeout(Duration::from_secs(10))
            .expect("waited for the binary download")
    );
    assert!(dirs(&setup).is_empty());
}

#[test]
fn another_platforms_binary_is_never_downloaded() {
    let setup = Setup::new();
    let (url, served) = serve_once(b"other");
    let m = with_binary("plan9-mips", &url, &sha256(b"other"));
    let source = setup.source("local", &m, &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    assert!(!setup.home().join("extensions/acme/bin").exists());
    // Nothing connected, so the server is still waiting; let it go.
    drop(served);
}

#[test]
fn a_binary_that_cannot_be_downloaded_aborts() {
    let setup = Setup::new();
    let m = with_binary(
        &extensions::platform(),
        "http://127.0.0.1:1/tool",
        &sha256(b"x"),
    );
    let source = setup.source("local", &m, &[]);
    let err = common::install(&setup.home(), &source, FIBER).unwrap_err();
    assert!(matches!(err, Error::Download { .. }), "{err}");
    assert_eq!(err.code(), ErrorCode::FetchFailed, "{err}");
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_raised_memory_cap_appears_in_carries_only_for_lua_extensions() {
    let setup = Setup::new();
    let cases: [(u64, bool, Option<&str>); 3] = [
        (8, false, Some("memory cap: 8 MiB")),
        (1, false, None),
        (8, true, None),
    ];
    for (memory_mib, process, expect) in cases {
        let mut m = manifest("acme");
        m["memory_mib"] = json!(memory_mib);
        if process {
            m["process"] = json!({ "program": "node", "args": [] });
        }
        let source = setup.source("local", &m, &[]);
        let clock = fakes::clock::FakeClock::new();
        let p = plan(
            &setup.home(),
            &Request::Path(source),
            FIBER,
            &Origin::github(),
            &*clock,
        )
        .unwrap();
        let carries = p.items().next().unwrap().carries();
        match expect {
            Some(line) => assert!(carries.iter().any(|l| l == line), "{carries:?}"),
            None => assert!(
                !carries.iter().any(|l| l.starts_with("memory cap:")),
                "{carries:?}"
            ),
        }
    }
}

#[test]
fn what_a_package_carries_is_listed_from_its_files_and_manifest() {
    let setup = Setup::new();
    let mut m = manifest("acme");
    m["prompt"] = json!("prompt.md");
    m["binaries"] = json!({ "darwin-arm64": { "url": "http://x/y", "sha256": "0" } });
    let source = setup.source("local", &m, &[]);
    write(&source.join("skills/review/SKILL.md"), "s");
    write(&source.join("skills/plan/SKILL.md"), "s");
    write(&source.join("themes/dark.json"), "{}");
    write(&source.join("tui/init.lua"), "");
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &Origin::github(),
        &*clock,
    )
    .unwrap();
    let carries = p.items().next().unwrap().carries();
    assert_eq!(
        carries,
        [
            "skills: plan, review",
            "themes: dark.json",
            "TUI extension: init.lua",
            "system prompt text: prompt.md",
            "binaries for darwin-arm64, of which only this platform's is downloaded",
        ]
    );
}

#[test]
fn a_listing_skips_stray_files_and_hidden_directories() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    write(&setup.home().join("extensions/stray.txt"), "x");
    fs::create_dir_all(setup.home().join("extensions/.scratch")).unwrap();
    assert_eq!(versions(&setup).keys().collect::<Vec<_>>(), ["acme"]);
}

#[test]
fn a_projects_directory_that_cannot_be_listed_fails_the_removal() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    write(&setup.home().join("projects"), "not a directory");
    let Err(err) = removal(&setup.home(), "acme", &*fakes::clock::FakeClock::new()) else {
        panic!("planned")
    };
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
}

#[test]
fn an_items_source_is_its_path_or_its_name() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[]);
    let clock = fakes::clock::FakeClock::new();
    let p = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
        &*clock,
    )
    .unwrap();
    assert_eq!(p.items().next().unwrap().source(), LIB);
    drop(p);
    let source = setup.source("local", &manifest("acme"), &[]);
    let p = plan(
        &setup.home(),
        &Request::Path(source.clone()),
        FIBER,
        &Origin::github(),
        &*clock,
    )
    .unwrap();
    let shown = fs::canonicalize(&source).unwrap().display().to_string();
    assert_eq!(p.items().next().unwrap().source(), shown);
}

fn plant(home: &Path, committed: bool, target: &Path, old: &Path, fresh: &Path, had_old: bool) {
    let text = serde_json::json!({
        "committed": committed,
        "steps": [{
            "target": target.display().to_string(),
            "old": old.display().to_string(),
            "fresh": fresh.display().to_string(),
            "had_old": had_old,
        }]
    });
    write(&home.join("extensions/.commit"), &text.to_string());
}

#[test]
fn a_listing_rolls_back_a_commit_that_stopped_halfway() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    write(&source.join("marker"), "old");
    common::install(&setup.home(), &source, FIBER).unwrap();
    let ext = setup.home().join("extensions/acme");
    let saved = setup.home().join("extensions/.acme.saved");
    let fresh = setup.home().join("extensions/.acme.fresh");
    fs::rename(&ext, &saved).unwrap();
    write(&ext.join("marker"), "new");
    write(&fresh.join("marker"), "staged");
    plant(&setup.home(), false, &ext, &saved, &fresh, true);
    list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(
        fs::read_to_string(setup.home().join("extensions/acme/marker")).unwrap(),
        "old"
    );
    assert!(!saved.exists());
    assert!(!fresh.exists());
    assert!(!setup.home().join("extensions/.commit").exists());
    assert!(!setup.home().join(".extensions.lock").exists());
}

#[test]
fn a_listing_drops_backups_once_a_commit_has_finished() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    write(&source.join("marker"), "kept");
    common::install(&setup.home(), &source, FIBER).unwrap();
    let ext = setup.home().join("extensions/acme");
    let saved = setup.home().join("extensions/.acme.saved");
    let fresh = setup.home().join("extensions/.acme.fresh");
    write(&saved.join("marker"), "backup");
    write(&fresh.join("marker"), "staged");
    plant(&setup.home(), true, &ext, &saved, &fresh, true);
    list(&setup.home(), &*fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(fs::read_to_string(ext.join("marker")).unwrap(), "kept");
    assert!(!saved.exists());
    assert!(!fresh.exists());
    assert!(!setup.home().join("extensions/.commit").exists());
}

#[test]
fn a_remove_treats_only_not_found_as_already_gone() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    let home = setup.home();
    let ext = home.join("extensions/acme");
    let data = home.join("data/acme");

    common::install(&home, &source, FIBER).unwrap();
    removal(&home, "acme", &*fakes::clock::FakeClock::new())
        .unwrap()
        .commit()
        .unwrap();
    assert!(
        !ext.exists(),
        "no data directory still removes the extension"
    );

    common::install(&home, &source, FIBER).unwrap();
    write(&data.join("x"), "x");
    let planned = removal(&home, "acme", &*fakes::clock::FakeClock::new()).unwrap();
    assert!(planned.data.iter().any(|path| path == &data));
    fs::remove_dir_all(&data).unwrap();
    planned.commit().unwrap();
    assert!(!ext.exists(), "a path that vanished is already gone");

    common::install(&home, &source, FIBER).unwrap();
    write(&data.join("x"), "x");
    let parent = home.join("data");
    let mut perms = fs::metadata(&parent).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&parent, perms).unwrap();
    let err = match removal(&home, "acme", &*fakes::clock::FakeClock::new()) {
        Ok(_) => panic!("a metadata error was treated as absence"),
        Err(err) => err,
    };
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
    assert!(
        err.to_string().contains(&data.display().to_string()),
        "{err}"
    );
    assert!(ext.exists(), "a metadata error removes nothing");

    let mut perms = fs::metadata(&parent).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&parent, perms).unwrap();
    write(&data.join("x"), "x");
    let planned = removal(&home, "acme", &*fakes::clock::FakeClock::new()).unwrap();
    let mut perms = fs::metadata(&parent).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&parent, perms).unwrap();
    let err = planned.commit().unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
    assert!(
        err.to_string().contains(&data.display().to_string()),
        "{err}"
    );
    assert!(
        ext.exists(),
        "commit stops before deleting when metadata fails"
    );
}

#[test]
fn a_data_directory_that_cannot_be_removed_fails_naming_it() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    let home = setup.home();
    common::install(&home, &source, FIBER).unwrap();
    let data = home.join("data/acme");
    let inner = data.join("inner");
    write(&inner.join("x"), "x");
    let mut perms = fs::metadata(&inner).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&inner, perms).unwrap();
    let err = removal(&home, "acme", &*fakes::clock::FakeClock::new())
        .unwrap()
        .commit()
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
    assert!(
        err.to_string().contains(&data.display().to_string()),
        "{err}"
    );
    assert!(
        inner.exists(),
        "the directory that could not be removed stays"
    );
}

/// The checked-in stand-in for `git`: it answers `ls-remote` with one tag
/// and stalls on its markers (`docs/testing.md`, "Testing an extension").
fn fake_git() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-git/git")
}

/// Kills the stalled processes matching `unique` by pid alone, then
/// requires that none are left: the run kills what it stopped, so leftovers
/// fail the test without leaking. By pid, never by group: a stalled `git`
/// shares this test's process group.
fn no_stall_left(unique: &str) {
    let leftovers = fakes::matching(unique).unwrap();
    for pid in &leftovers {
        drop(fakes::kill_pid(*pid, "KILL"));
    }
    assert!(
        leftovers.is_empty(),
        "the stalled git is gone: {leftovers:?}"
    );
}

/// A `ls-remote` that never answers fails the plan at the git deadline.
#[test]
fn a_stalled_ls_remote_fails_the_plan_at_the_git_deadline() {
    let setup = Setup::new();
    let unique = format!("stall-ls-remote-{}", setup.root().display());
    let watching = unique.clone();
    let git = fake_git().to_string_lossy().into_owned();
    let clock = fakes::clock::FakeClock::new();
    let worker_clock = std::sync::Arc::clone(&clock);
    let home = setup.home();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("stalled ls-remote".into())
        .spawn(move || {
            // Built here: `Origin` holds a closure, so it never crosses a
            // thread.
            let origin = Origin::new(git, move |repo| format!("{unique}/{repo}"));
            let _sent = done_tx.send(
                plan(
                    &home,
                    &Request::Install("github.com/acme/stalled".into()),
                    FIBER,
                    &origin,
                    &*worker_clock,
                )
                .map(|_| ()),
            );
        })
        .unwrap();
    let err = drive(&clock, done_rx, &watching).unwrap_err();
    assert!(
        matches!(err, Error::Git { .. }),
        "a stalled ls-remote fails as git failed: {err}"
    );
    assert!(
        err.to_string().contains("did not finish within"),
        "the failure names the deadline: {err}"
    );
    assert!(
        err.to_string().contains("so it was stopped"),
        "the failure names the stop: {err}"
    );
    no_stall_left(&watching);
}

/// A `clone` that never finishes fails the plan at the git deadline: the
/// canned `ls-remote` answer resolves the tag, and only the clone stalls.
#[test]
fn a_stalled_clone_fails_the_plan_at_the_git_deadline() {
    let setup = Setup::new();
    let unique = format!("stall-clone-{}", setup.root().display());
    let watching = unique.clone();
    let git = fake_git().to_string_lossy().into_owned();
    let clock = fakes::clock::FakeClock::new();
    let worker_clock = std::sync::Arc::clone(&clock);
    let home = setup.home();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("stalled clone".into())
        .spawn(move || {
            // Built here: `Origin` holds a closure, so it never crosses a
            // thread.
            let origin = Origin::new(git, move |repo| format!("{unique}/{repo}"));
            let _sent = done_tx.send(
                plan(
                    &home,
                    &Request::Install("github.com/acme/stalled".into()),
                    FIBER,
                    &origin,
                    &*worker_clock,
                )
                .map(|_| ()),
            );
        })
        .unwrap();
    let err = drive(&clock, done_rx, &watching).unwrap_err();
    assert!(
        matches!(err, Error::Git { .. }),
        "a stalled clone fails as git failed: {err}"
    );
    assert!(
        err.to_string().contains("did not finish within"),
        "the failure names the deadline: {err}"
    );
    assert!(
        err.to_string().contains("so it was stopped"),
        "the failure names the stop: {err}"
    );
    no_stall_left(&watching);
}
