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
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Setup, manifest, provider, write};
use contract::ErrorCode;
use extensions::{Error, Origin, Request, SHORT_NAMES, list, plan, uninstall};
use serde_json::{Value, json};

const FIBER: &str = "0.1.0";

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
        git(&path, &["commit", "--quiet", "--allow-empty", "-m", tag]);
        git(&path, &["tag", tag]);
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
    )?
    .commit()
}

fn versions(setup: &Setup) -> BTreeMap<String, String> {
    list(&setup.home())
        .unwrap()
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
    let installed = list(&setup.home()).unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].version, "v1.1.0");
    assert_eq!(
        installed[0].commit.clone().unwrap(),
        repos.commit(LIB, "v1.1.0")
    );
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
        !list(&setup.home())
            .unwrap()
            .iter()
            .find(|i| i.name == dep)
            .unwrap()
            .requested
    );
    // A second dependent raises the minimum, and the dependency moves up.
    let p = plan(
        &setup.home(),
        &Request::Install(b.into()),
        FIBER,
        &repos.origin(),
    )
    .unwrap();
    let moved: Vec<_> = p.items().filter(|i| i.name == dep).collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].changes, None);
    p.commit().unwrap();
    assert_eq!(versions(&setup)[dep], "v1.4.0");
    let listed = list(&setup.home()).unwrap();
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
    let first = plan(
        &setup.home(),
        &Request::Install(dep.into()),
        FIBER,
        &repos.origin(),
    );
    first.unwrap().commit().unwrap();
    repos.tag(dep, "", "v1.6.0", &manifest(dep), &[]);
    let top = "example.com/acme/top";
    repos.tag(top, "", "v1.0.0", &named(top, &[(dep, "1.6")]), &[]);
    install(&setup, &repos, top).unwrap();
    let listed = list(&setup.home()).unwrap();
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
    assert_eq!(list(&setup.home()).unwrap_err().code(), ErrorCode::IoFailed);
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
        list(&setup.home())
            .unwrap()
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
    let p = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
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
    let p = plan(
        &setup.home(),
        &Request::Update(LIB.into()),
        FIBER,
        &repos.origin(),
    )
    .unwrap();
    let item = p.items().next().unwrap();
    assert_eq!(item.version, "v1.1.0");
    let changes = item.changes.clone().unwrap();
    assert!(changes.contains("b.lua"), "{changes}");
    assert_eq!(
        list(&setup.home()).unwrap()[0].commit.as_deref(),
        Some(first.as_str())
    );
    p.commit().unwrap();
    let now = list(&setup.home()).unwrap();
    assert_eq!(now[0].version, "v1.1.0");
    assert_eq!(now[0].commit.clone().unwrap(), repos.commit(LIB, "v1.1.0"));
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
    let err = plan(
        &setup.home(),
        &Request::Update(LIB.into()),
        FIBER,
        &repos.origin(),
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
    assert!(list(&setup.home()).unwrap().is_empty());
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
    extensions::install(&setup.home(), &source, FIBER).unwrap();
    let listed = list(&setup.home()).unwrap();
    assert_eq!(listed[0].version, "v1.0.0");
    assert_eq!(listed[0].commit, None);
}

#[test]
fn missing_git_fails_with_the_usage_code_and_says_to_install_it() {
    let setup = Setup::new();
    let origin = Origin::new("fiber-no-such-git-program", |repo| repo.to_owned());
    let err = plan(&setup.home(), &Request::Install(LIB.into()), FIBER, &origin);
    let Err(err) = err else { panic!("planned") };
    assert!(matches!(err, Error::GitMissing));
    assert_eq!(err.code(), ErrorCode::Usage);
    assert!(err.to_string().contains("Install git"), "{err}");
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
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::BadName { .. }), "{err}");
}
