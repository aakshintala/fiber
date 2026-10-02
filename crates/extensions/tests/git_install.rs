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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Setup, manifest, provider, write};
use contract::ErrorCode;
use extensions::{Error, Installed, Origin, Provenance, Request, SHORT_NAMES, list, plan, removal};
use serde_json::{Value, json};

const FIBER: &str = "0.1.0";

/// Removes `typed` and what nothing else needs, as `fiber remove` does
/// without a terminal, and returns the names removed.
fn uninstall(home: &Path, typed: &str) -> Result<Vec<String>, Error> {
    let removal = removal(home, typed)?;
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
    let installed = list(&setup.home()).unwrap();
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
        commit(&list(&setup.home()).unwrap()[0]).as_deref(),
        Some(first.as_str())
    );
    p.commit().unwrap();
    let now = list(&setup.home()).unwrap();
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
    common::install(&setup.home(), &source, FIBER).unwrap();
    let listed = list(&setup.home()).unwrap();
    assert_eq!(listed[0].version, "v1.0.0");
    assert!(matches!(&listed[0].provenance, Provenance::Path(p) if p.is_absolute()));
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
    assert_eq!(list(&setup.home()).unwrap().len(), 1);
}

#[test]
fn a_second_operation_while_one_holds_the_lock_fails_cleanly() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "v1.0.0", &manifest(LIB), &[]);
    let held = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
    )
    .unwrap();
    let Err(err) = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
    ) else {
        panic!("a second plan")
    };
    assert!(matches!(err, Error::Busy), "{err}");
    assert!(matches!(removal(&setup.home(), LIB), Err(Error::Busy)));
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
    let first = plan(
        &setup.home(),
        &Request::Install(LIB.into()),
        FIBER,
        &repos.origin(),
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
    ) else {
        panic!("planned")
    };
    assert!(matches!(err, Error::MajorConflict { .. }), "{err}");
    assert!(err.to_string().contains(user), "{err}");
    assert_eq!(versions(&setup)[dep], "v1.2.0");
}

#[test]
fn a_repository_with_no_version_tag_is_refused() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    repos.tag(LIB, "", "", &manifest(LIB), &[]);
    repos.tag(LIB, "", "latest", &manifest(LIB), &[]);
    let err = install(&setup, &repos, LIB).unwrap_err();
    assert!(matches!(err, Error::NoTag { .. }), "{err}");
    assert!(dirs(&setup).is_empty());
}

#[test]
fn a_local_record_holds_the_absolute_path_and_an_update_checks_the_name() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    let roundabout = source.join("../local");
    common::install(&setup.home(), &roundabout, FIBER).unwrap();
    let listed = list(&setup.home()).unwrap();
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
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(versions(&setup)["acme"], "v1.1.0");
}

#[test]
fn a_listing_names_the_file_it_cannot_read() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    common::install(&setup.home(), &source, FIBER).unwrap();
    let dir = setup.home().join("extensions/acme");
    for (file, text, code) in [
        (".fiber.json", "{", ErrorCode::IoFailed),
        (
            ".fiber.json",
            r#"{"name":"acme","version":"v1","requested":true,"source":{}}"#,
            ErrorCode::IoFailed,
        ),
        ("extension.json", "{", ErrorCode::ConfigInvalid),
    ] {
        let kept = fs::read_to_string(dir.join(file)).unwrap();
        write(&dir.join(file), text);
        let err = list(&setup.home()).unwrap_err();
        assert_eq!(err.code(), code, "{err}");
        assert!(err.to_string().contains(file), "{err}");
        write(&dir.join(file), &kept);
    }
    fs::remove_file(dir.join(".fiber.json")).unwrap();
    let err = list(&setup.home()).unwrap_err();
    assert!(matches!(err, Error::BadRecord { .. }), "{err}");
    assert!(err.to_string().contains(".fiber.json"), "{err}");
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
    let r = removal(&home, "example.com/acme/x").unwrap();
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
    removal(&home, "example.com/acme/x")
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
fn an_install_step_runs_after_the_plan_in_the_staged_directory_and_again_on_update() {
    let setup = Setup::new();
    let marker = setup.root().join("ran");
    let step = format!(
        "echo ran >> '{}' && echo built > built.txt",
        marker.display()
    );
    let source = setup.source("local", &with_step("acme", &["sh", "-c", &step]), &[]);
    let p = plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &Origin::github(),
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
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "ran\nran\n");
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
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallStep { .. }), "{err}");
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
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(err, Error::InstallStep { .. }), "{err}");
    assert!(dirs(&setup).is_empty());
}

/// Serves `body` once on a local port, and returns the URL. The thread gives
/// up after a deadline.
fn serve_once(body: &'static [u8]) -> (String, std::thread::JoinHandle<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/tool-1.0", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_nonblocking(false).unwrap();
                let mut seen = Vec::new();
                let mut buf = [0; 512];
                while !seen.ends_with(b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    seen.extend_from_slice(&buf[..n]);
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    });
    (url, handle)
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
    assert!(served.join().unwrap());
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
    assert!(served.join().unwrap());
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
    assert!(dirs(&setup).is_empty());
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
    let p = plan(
        &setup.home(),
        &Request::Path(source),
        FIBER,
        &Origin::github(),
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
    let Err(err) = removal(&setup.home(), "acme") else {
        panic!("planned")
    };
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err}");
}

#[test]
fn an_items_source_is_its_path_or_its_name() {
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
    assert_eq!(p.items().next().unwrap().source(), LIB);
    drop(p);
    let source = setup.source("local", &manifest("acme"), &[]);
    let p = plan(
        &setup.home(),
        &Request::Path(source.clone()),
        FIBER,
        &Origin::github(),
    )
    .unwrap();
    let shown = fs::canonicalize(&source).unwrap().display().to_string();
    assert_eq!(p.items().next().unwrap().source(), shown);
}
