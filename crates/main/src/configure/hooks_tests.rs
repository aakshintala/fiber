//! Public Configure coverage for filtered names, fresh projects and global installs.
use std::fs;
use std::path::Path;
use std::sync::Arc;

use contract::ErrorCode;
use fakes::{Deadline, clock::FakeClock};
use serde_json::{Value, json};
use tui::Configure;

use crate::configure::{Seam, tests::write};
use crate::test_support::{Rig, install_extension};

const NAME: &str = "github.com/aakshintala/fiber/extensions/hooks";

fn override_entry(name: &str, mode: &str) -> String {
    format!(
        "extensions.\"{NAME}\".settings.hooks.{name}={}",
        entry(mode)
    )
}

fn entry(mode: &str) -> Value {
    json!({"point":"session_start","command":"true","timeout":1000,"on_failure":mode})
}

fn hook_file(file: &Path, hooks: Value) {
    write(file, &json!({"hooks":hooks}).to_string());
}

fn approve(rig: &Rig, project: &config::ProjectKey, name: &str) {
    let clock = FakeClock::new();
    let items = extensions::declared_items(&rig.workspace, clock.as_ref()).unwrap();
    let item = items.iter().find(|item| item.name == name).unwrap();
    let digest = extensions::hash(&mut extensions::Index::load(&rig.home), item).unwrap();
    extensions::Store::new(&rig.home, project, clock)
        .approve(item, &digest)
        .unwrap();
}

#[test]
fn query_keeps_override_order_and_returns_only_absent_blockers() {
    fakes::within("hooks query layers", Deadline::start(), || {
        let rig = Rig::new("hooks-query-layers");
        let seam = Seam::new(rig.home.clone());
        assert_eq!(
            seam.blocking_hooks(&rig.workspace, &[]).unwrap(),
            Vec::<String>::new()
        );
        let overrides = vec![
            override_entry("z", "blocking"),
            override_entry("a", "blocking"),
            override_entry("z", "non-blocking"),
        ];
        assert_eq!(
            seam.blocking_hooks(&rig.workspace, &overrides).unwrap(),
            ["a"]
        );
        assert!(
            seam.blocking_hooks(&rig.workspace, &[override_entry("notice", "non-blocking")])
                .unwrap()
                .is_empty()
        );
        assert!(seam.blocking_hooks(&rig.workspace, &[format!("extensions.\"{NAME}\".settings.hooks.watch={{\"watch\":[],\"command\":\"true\",\"timeout\":1000}}")]).unwrap().is_empty());
        install_extension(
            &rig.home,
            "extensions/hooks",
            json!({"name":NAME,"version":"0.1.0","fiber":"0.0.0","api":1}),
            &[],
        );
        // An installed package is not repaired or enabled by another install.
        rig.write_global(&json!({"extensions":{NAME:{"enabled":false}}}));
        assert!(
            seam.blocking_hooks(&rig.workspace, &overrides)
                .unwrap()
                .is_empty()
        );
        rig.write_global(&json!({}));
        write(
            &rig.home.join("extensions/hooks/init.lua"),
            "error('cannot load')\n",
        );
        assert!(
            seam.blocking_hooks(&rig.workspace, &overrides)
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn query_withholds_unapproved_only_repository_hooks() {
    fakes::within("unapproved hooks query", Deadline::start(), || {
        let rig = Rig::new("hooks-query-withheld");
        fakes::git::git(&rig.workspace, &["init", "-q"]);
        write(&rig.workspace.join("scripts/repo.sh"), "echo repo\n");
        let mut hook = entry("blocking");
        hook["command"] = json!("scripts/repo.sh");
        hook_file(
            &rig.workspace.join(".fiber/config/hooks.json"),
            json!({"withheld":hook}),
        );
        let seam = Seam::new(rig.home.clone());
        assert!(seam.blocking_hooks(&rig.workspace, &[]).unwrap().is_empty());
    });
}

#[test]
fn query_filters_repository_names_without_starting_any_vm() {
    fakes::within("filtered hooks query", Deadline::start(), || {
        let rig = Rig::new("hooks-query-filter");
        fakes::git::git(&rig.workspace, &["init", "-q"]);
        write(&rig.workspace.join("scripts/repo.sh"), "echo repo\n");
        let mut repo_entry = entry("blocking");
        repo_entry["command"] = json!("scripts/repo.sh");
        hook_file(
            &rig.workspace.join(".fiber/config/hooks.json"),
            json!({"approved":repo_entry,"withheld":repo_entry,"personal":entry("non-blocking")}),
        );
        let seam = Seam::new(rig.home.clone());
        let (_, project) = ::cli::project_of(&rig.home, &rig.workspace).unwrap();
        approve(&rig, &project, "approved");
        hook_file(
            &rig.home.join("config/hooks.json"),
            json!({"personal":entry("blocking")}),
        );
        install_extension(
            &rig.home,
            "extensions/sentinel",
            json!({"name":"sentinel","version":"0.1.0","fiber":"0.0.0","api":1}),
            &[],
        );
        let sentinel = rig.root.path().join("vm-started");
        write(
            &rig.home.join("extensions/sentinel/init.lua"),
            &format!("host.fs.write({:?}, 'started')", sentinel.to_str().unwrap()),
        );
        assert_eq!(
            seam.blocking_hooks(&rig.workspace, &[override_entry("run", "blocking")])
                .unwrap(),
            ["approved", "personal", "run"]
        );
        assert!(
            !sentinel.exists(),
            "a names query must not initialize extension VMs"
        );
    });
}

#[test]
fn query_resolves_both_project_settings_and_approvals_after_cache_priming() {
    fakes::within("fresh hooks project", Deadline::start(), || {
        let rig = Rig::new("hooks-query-fresh-project");
        let (_, old) = ::cli::project_of(&rig.home, &rig.workspace).unwrap();
        hook_file(
            &rig.home
                .join("projects")
                .join(old.as_str())
                .join("config/hooks.json"),
            json!({"old":entry("blocking")}),
        );
        let seam = Seam::new(rig.home.clone());
        seam.settings(&rig.workspace).unwrap();
        fakes::git::git(&rig.workspace, &["init", "-q"]);
        let (_, fresh) = ::cli::project_of(&rig.home, &rig.workspace).unwrap();
        assert_ne!(old, fresh);
        hook_file(
            &rig.home
                .join("projects")
                .join(fresh.as_str())
                .join("config/hooks.json"),
            json!({"new":entry("blocking")}),
        );
        write(&rig.workspace.join("scripts/repo.sh"), "echo repo\n");
        let mut repository = entry("blocking");
        repository["command"] = json!("scripts/repo.sh");
        hook_file(
            &rig.workspace.join(".fiber/config/hooks.json"),
            json!({"repository":repository}),
        );
        approve(&rig, &fresh, "repository");
        assert_eq!(
            seam.blocking_hooks(&rig.workspace, &[]).unwrap(),
            ["new", "repository"]
        );
    });
}

#[test]
fn query_preserves_config_syntax_and_read_errors() {
    fakes::within("hooks query errors", Deadline::start(), || {
        let rig = Rig::new("hooks-query-errors");
        let seam = Seam::new(rig.home.clone());
        let absent = Path::new("/invalid\0workspace");
        let expected: contract::shapes::Failure =
            ::cli::project_of(&rig.home, absent).unwrap_err().into();
        let actual = seam.blocking_hooks(absent, &[]).unwrap_err();
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.message, expected.message);
        write(&rig.home.join("config/hooks.json"), "{not json");
        let error = seam.blocking_hooks(&rig.workspace, &[]).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains("hooks.json"));
        fs::remove_file(rig.home.join("config/hooks.json")).unwrap();
        fs::create_dir(rig.home.join("config/hooks.json")).unwrap();
        assert_eq!(
            seam.blocking_hooks(&rig.workspace, &[]).unwrap_err().code,
            ErrorCode::IoFailed
        );
        fs::remove_dir(rig.home.join("config/hooks.json")).unwrap();
        let overrides = vec!["=bad".to_owned()];
        let (_, project) = ::cli::project_of(&rig.home, &rig.workspace).unwrap();
        let expected = config::Config::load(config::Sources {
            home: rig.home.clone(),
            workspace: rig.workspace.clone(),
            project,
            overrides: overrides.clone(),
        })
        .err()
        .unwrap();
        let actual = seam.blocking_hooks(&rig.workspace, &overrides).unwrap_err();
        assert_eq!(actual.code, expected.code());
        assert_eq!(actual.message, expected.to_string());
        write(&rig.home.join("extensions"), "not a directory");
        let expected = extensions::list(&rig.home, FakeClock::new().as_ref()).unwrap_err();
        let actual = seam.blocking_hooks(&rig.workspace, &[]).unwrap_err();
        assert_eq!(actual.code, expected.code());
        assert_eq!(actual.message, expected.to_string());
    });
}

fn repository(fail: bool) -> fakes::TempDir {
    let repo = fakes::git::repo("hooks-install-source");
    let dir = repo.path().join("extensions/hooks");
    let mut manifest =
        json!({"name":NAME,"version":"0.1.0","fiber":env!("CARGO_PKG_VERSION"),"api":1});
    if fail {
        manifest["install"] = json!(["sh", "-c", "printf 'fixture failure' >&2; exit 1"]);
    }
    write(&dir.join("extension.json"), &manifest.to_string());
    fakes::git::git(repo.path(), &["add", "."]);
    fakes::git::git(repo.path(), &["commit", "-qm", "hooks fixture"]);
    fakes::git::git(repo.path(), &["tag", "v0.1.0"]);
    repo
}

fn origin(repo: &Path) -> Arc<extensions::Origin> {
    let repo = repo.to_path_buf();
    Arc::new(extensions::Origin::new("git", move |name| {
        assert_eq!(name, "github.com/aakshintala/fiber");
        format!("file://{}", repo.display())
    }))
}

#[test]
fn install_commits_the_tagged_package_globally_then_query_is_empty() {
    fakes::within("hooks global install", Deadline::start(), || {
        let rig = Rig::new("hooks-install");
        let repo = repository(false);
        let seam = Seam::with_origin(rig.home.clone(), origin(repo.path()));
        let overrides = vec![override_entry("guard", "blocking")];
        assert_eq!(
            seam.blocking_hooks(&rig.workspace, &overrides).unwrap(),
            ["guard"]
        );
        write(&rig.home.join("config.json"), "not JSON");
        seam.install_hooks(FakeClock::new()).unwrap();
        fs::remove_file(rig.home.join("config.json")).unwrap();
        let listing = extensions::list(&rig.home, FakeClock::new().as_ref()).unwrap();
        let installed = listing
            .installed
            .iter()
            .find(|item| item.name == NAME)
            .unwrap();
        assert_eq!(
            installed.provenance,
            extensions::Provenance::Git {
                commit: fakes::git::git(repo.path(), &["rev-parse", "v0.1.0^{commit}"])
            }
        );
        assert!(rig.home.join("extensions/hooks/extension.json").is_file());
        assert!(!rig.workspace.join(".fiber/extensions").exists());
        assert!(
            seam.blocking_hooks(&rig.workspace, &overrides)
                .unwrap()
                .is_empty()
        );
        use hub::Machine;
        assert_eq!(
            seam.configure(
                "install_hooks",
                json!({}).as_object().unwrap(),
                FakeClock::new()
            )
            .unwrap(),
            Some(json!({}))
        );
    });
}

#[test]
fn install_preserves_plan_and_commit_errors_without_partial_commit() {
    fakes::within("hooks install failures", Deadline::start(), || {
        let rig = Rig::new("hooks-install-failure");
        let repo = repository(true);
        let source = origin(repo.path());
        let seam = Seam::with_origin(rig.home.clone(), source.clone());
        let clock = FakeClock::new();
        let request = extensions::Request::Install("hooks".into());
        let expected = extensions::plan(
            &rig.home,
            &request,
            env!("CARGO_PKG_VERSION"),
            &source,
            clock.as_ref(),
        )
        .unwrap()
        .commit()
        .unwrap_err();
        let actual = seam.install_hooks(clock.clone()).unwrap_err();
        assert_eq!(actual.code, expected.code());
        assert_eq!(actual.message, expected.to_string());
        use hub::Machine;
        let wire = seam
            .configure(
                "install_hooks",
                json!({}).as_object().unwrap(),
                clock.clone(),
            )
            .unwrap_err();
        assert_eq!(wire.code, expected.code());
        assert_eq!(wire.message, expected.to_string());
        assert!(
            extensions::list(&rig.home, clock.as_ref())
                .unwrap()
                .installed
                .is_empty()
        );
        assert!(!rig.home.join("extensions/hooks").exists());
        let source = origin(&rig.root.path().join("absent"));
        let expected = match extensions::plan(
            &rig.home,
            &request,
            env!("CARGO_PKG_VERSION"),
            &source,
            clock.as_ref(),
        ) {
            Ok(_) => panic!("missing source must fail"),
            Err(error) => error,
        };
        let seam = Seam::with_origin(rig.home.clone(), source);
        let actual = seam.install_hooks(clock.clone()).unwrap_err();
        assert_eq!(actual.code, expected.code());
        assert_eq!(actual.message, expected.to_string());
        let wire = seam
            .configure(
                "install_hooks",
                json!({}).as_object().unwrap(),
                clock.clone(),
            )
            .unwrap_err();
        assert_eq!(wire.code, expected.code());
        assert_eq!(wire.message, expected.to_string());
        assert!(
            extensions::list(&rig.home, clock.as_ref())
                .unwrap()
                .installed
                .is_empty()
        );
    });
}
