//! `fiber extension install`, `update`, `remove` and `list` against a
//! temporary Fiber home, with extension sources built by hand: what each
//! command writes, the exit code it gives, and the directories it leaves.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::{Origin, Request};

use super::{Io, install, list, remove, update_all};

#[test]
fn public_install_wrapper_propagates_project_scope() {
    const SCOPED_CHILD: &str = "FIBER_TEST_SCOPED_INSTALL_CHILD";
    if std::env::var_os(SCOPED_CHILD).is_some() {
        let source = std::env::var("FIBER_TEST_SCOPED_SOURCE").unwrap();
        std::process::exit(super::extension_install(
            &source,
            true,
            FIBER_VERSION,
            &fakes::clock::SystemClock,
        ));
    }
    let setup = Setup::new();
    let source = setup.source("acme", &manifest("acme"), &[]);
    let workspace = setup.root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let name = format!(
        "{}::public_install_wrapper_propagates_project_scope",
        module_path!().split_once("::").unwrap().1
    );
    let output = fakes::rerun_prepared(
        &name,
        &[(SCOPED_CHILD, "1")],
        Duration::from_secs(20),
        |command| {
            command
                .env("FIBER_HOME", setup.home())
                .env("FIBER_TEST_SCOPED_SOURCE", &source)
                .current_dir(&workspace);
        },
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (global, project) = scope_files(&setup);
    assert_eq!(
        json_file(&global),
        serde_json::json!({"extensions":{"acme":{"enabled":false}}})
    );
    assert_eq!(
        json_file(&project),
        serde_json::json!({"extensions":{"acme":{"enabled":true}}})
    );
    assert!(setup.home().join("extensions/acme/.fiber.json").is_file());
}

#[test]
fn scoped_git_dependencies_keep_existing_global_state_and_write_all_projects_first() {
    fakes::within("scoped dependency writes", Duration::from_secs(40), || {
        let setup = Setup::new();
        let root = "example.com/acme/root";
        let dep = "example.com/acme/dep";
        let new = "example.com/acme/new";
        let mut paths = std::collections::BTreeMap::new();
        for name in [dep, new, root] {
            let m = if name == root {
                let mut m = manifest(root);
                m["depends"] = serde_json::json!({dep:"1.1", new:"1.0"});
                m
            } else {
                manifest(name)
            };
            let source = setup.source(name, &m, &[]);
            fakes::git::git(&source, &["init", "--quiet"]);
            fakes::git::git(&source, &["add", "."]);
            fakes::git::git(&source, &["commit", "--quiet", "-m", "release"]);
            fakes::git::git(&source, &["tag", "v1.0.0"]);
            if name == dep {
                fakes::git::git(&source, &["tag", "v1.1.0"]);
                assert_eq!(
                    run_install(
                        &setup.home(),
                        Request::Path(source.clone()),
                        &Origin::github(),
                        false,
                        &mut Unread
                    )
                    .1
                    .unwrap(),
                    0
                );
            }
            paths.insert(name.to_owned(), source);
        }
        let origin = Origin::new("git", move |name| {
            format!("file://{}", paths[name].display())
        });
        let workspace = setup.root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let (_, project) = crate::project_of(&setup.home(), &workspace).unwrap();
        let scope = super::ScopedInstall { workspace, project };
        let code = install(
            &setup.home(),
            Request::Install(root.into()),
            Some(&scope),
            FIBER_VERSION,
            &origin,
            &fakes::clock::SystemClock,
            Io {
                terminal: false,
                input: &mut Unread,
                err: &mut Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(code, 0);
        let (global, project) = scope_files(&setup);
        assert_eq!(
            json_file(&global),
            serde_json::json!({"extensions":{root:{"enabled":false},new:{"enabled":false}}})
        );
        assert_eq!(
            json_file(&project),
            serde_json::json!({"extensions":{root:{"enabled":true},new:{"enabled":true},dep:{"enabled":true}}})
        );
        let listed = extensions::list(&setup.home(), &fakes::clock::SystemClock).unwrap();
        assert_eq!(listed.installed.len(), 3);
        assert_eq!(
            listed
                .installed
                .iter()
                .find(|i| i.name == dep)
                .unwrap()
                .version,
            "v1.1.0"
        );
    });
}

#[test]
fn a_later_scope_key_failure_keeps_completed_keys_and_never_runs_install_steps() {
    fakes::within("partial scope writes", Duration::from_secs(20), || {
        let setup = Setup::new();
        let dep = "example.com/acme/dep";
        let dep_source = setup.source("dep", &manifest(dep), &[]);
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "."],
            vec!["commit", "--quiet", "-m", "release"],
            vec!["tag", "v1.0.0"],
        ] {
            fakes::git::git(&dep_source, &args);
        }
        let origin = Origin::new("git", move |_| format!("file://{}", dep_source.display()));
        let marker = setup.root.path().join("install-step");
        let mut m = manifest("z\"acme");
        m["depends"] = serde_json::json!({dep:"1.0"});
        m["install"] = serde_json::json!([
            "sh",
            "-c",
            format!("printf marker > '{}'", marker.display())
        ]);
        let source = setup.source("root", &m, &[]);
        let workspace = setup.root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let (_, project) = crate::project_of(&setup.home(), &workspace).unwrap();
        let scope = super::ScopedInstall { workspace, project };
        let failure = install(
            &setup.home(),
            Request::Path(source),
            Some(&scope),
            FIBER_VERSION,
            &origin,
            &fakes::clock::SystemClock,
            Io {
                terminal: false,
                input: &mut Unread,
                err: &mut Vec::new(),
            },
        )
        .unwrap_err();
        assert_eq!(failure.code, ErrorCode::Usage);
        let (global, project) = scope_files(&setup);
        assert_eq!(
            json_file(&project),
            serde_json::json!({"extensions":{dep:{"enabled":true}}})
        );
        assert!(!global.exists());
        assert!(!marker.exists());
        assert!(
            extensions::list(&setup.home(), &fakes::clock::SystemClock)
                .unwrap()
                .installed
                .is_empty()
        );
    });
}

fn scoped_install(
    setup: &Setup,
    source: &Path,
    terminal: bool,
    input: &mut dyn BufRead,
) -> Result<i32, Failure> {
    let workspace = setup.root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let (_, project) = crate::project_of(&setup.home(), &workspace).unwrap();
    let scope = super::ScopedInstall { workspace, project };
    install(
        &setup.home(),
        Request::Path(source.into()),
        Some(&scope),
        FIBER_VERSION,
        &Origin::github(),
        &fakes::clock::SystemClock,
        Io {
            terminal,
            input,
            err: &mut Vec::new(),
        },
    )
}

fn scope_files(setup: &Setup) -> (PathBuf, PathBuf) {
    let workspace = setup.root.path().join("workspace");
    let (_, project) = crate::project_of(&setup.home(), &workspace).unwrap();
    (
        setup.home().join("config.json"),
        setup
            .home()
            .join("projects")
            .join(project.as_str())
            .join("config.json"),
    )
}

fn json_file(file: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
}

#[test]
fn scoped_install_writes_keys_before_commit_and_other_commands_keep_them() {
    fakes::within(
        "scoped keys before install",
        Duration::from_secs(20),
        || {
            let setup = Setup::new();
            let workspace = setup.root.path().join("workspace");
            fs::create_dir_all(&workspace).unwrap();
            let (global, project) = scope_files(&setup);
            let artifact = setup.root.path().join("keys");
            let mut m = manifest("acme");
            m["install"] = serde_json::json!([
                "sh",
                "-c",
                format!(
                    "cat '{}' '{}' > '{}'",
                    project.display(),
                    global.display(),
                    artifact.display()
                )
            ]);
            let source = setup.source("acme", &m, &[]);
            assert_eq!(
                scoped_install(&setup, &source, false, &mut Unread).unwrap(),
                0
            );
            let on = serde_json::json!({"extensions":{"acme":{"enabled":true}}});
            let off = serde_json::json!({"extensions":{"acme":{"enabled":false}}});
            assert_eq!(json_file(&project), on);
            assert_eq!(json_file(&global), off);
            assert_eq!(
                fs::read_to_string(&artifact).unwrap(),
                format!(
                    "{}{}",
                    fs::read_to_string(&project).unwrap(),
                    fs::read_to_string(&global).unwrap()
                )
            );
            assert!(!workspace.join(".fiber").exists());
            for request in [
                Request::Path(source.clone()),
                Request::Update("acme".into()),
                Request::Reinstall("acme".into()),
            ] {
                assert_eq!(
                    run_install(
                        &setup.home(),
                        request,
                        &Origin::github(),
                        false,
                        &mut Unread
                    )
                    .1
                    .unwrap(),
                    0
                );
                assert_eq!(json_file(&project), on);
                assert_eq!(json_file(&global), off);
            }
            assert_eq!(
                run_remove(&setup.home(), "acme", false, &mut Unread)
                    .1
                    .unwrap(),
                0
            );
            assert_eq!(json_file(&project), on);
            assert_eq!(json_file(&global), off);
        },
    );
}

#[test]
fn existing_installs_keep_global_state_and_fresh_installs_replace_stale_keys() {
    fakes::within(
        "scoped freshness configuration",
        Duration::from_secs(20),
        || {
            for global in [None, Some(false), Some(true)] {
                let setup = Setup::new();
                let source = setup.source("acme", &manifest("acme"), &[]);
                assert_eq!(
                    run_install(
                        &setup.home(),
                        Request::Path(source.clone()),
                        &Origin::github(),
                        false,
                        &mut Unread
                    )
                    .1
                    .unwrap(),
                    0
                );
                let expected = global
                    .map(|value| serde_json::json!({"extensions":{"acme":{"enabled":value}}}));
                if let Some(ref expected) = expected {
                    write_file(&setup.home().join("config.json"), &expected.to_string());
                }
                assert_eq!(
                    scoped_install(&setup, &source, false, &mut Unread).unwrap(),
                    0
                );
                let (global_file, project) = scope_files(&setup);
                assert_eq!(
                    json_file(&project),
                    serde_json::json!({"extensions":{"acme":{"enabled":true}}})
                );
                assert_eq!(global_file.exists(), expected.is_some());
                if let Some(expected) = expected {
                    assert_eq!(json_file(&global_file), expected);
                }
            }
            let setup = Setup::new();
            let source = setup.source("acme", &manifest("acme"), &[]);
            write_file(
                &setup.home().join("config.json"),
                r#"{"extensions":{"acme":{"enabled":true}},"model":3}"#,
            );
            assert_eq!(
                scoped_install(&setup, &source, false, &mut Unread).unwrap(),
                0
            );
            let (global, project) = scope_files(&setup);
            assert_eq!(
                json_file(&global),
                serde_json::json!({"extensions":{"acme":{"enabled":false}},"model":3})
            );
            assert_eq!(
                json_file(&project),
                serde_json::json!({"extensions":{"acme":{"enabled":true}}})
            );
        },
    );
}

#[test]
fn scope_write_failures_stop_install_and_keep_completed_project_keys() {
    fakes::within(
        "scope write failure ordering",
        Duration::from_secs(20),
        || {
            for broken_project in [true, false] {
                let setup = Setup::new();
                fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
                let (global, project) = scope_files(&setup);
                let broken = if broken_project { &project } else { &global };
                write_file(broken, "not JSON");
                let marker = setup.root.path().join("installed");
                let mut m = manifest("acme");
                m["install"] = serde_json::json!([
                    "sh",
                    "-c",
                    format!("printf marker > '{}'", marker.display())
                ]);
                let source = setup.source("acme", &m, &[]);
                assert_eq!(
                    scoped_install(&setup, &source, false, &mut Unread)
                        .unwrap_err()
                        .code,
                    ErrorCode::ConfigInvalid
                );
                assert_eq!(fs::read_to_string(broken).unwrap(), "not JSON");
                assert!(!marker.exists());
                assert!(!setup.home().join("extensions/acme").exists());
                if broken_project {
                    assert!(!global.exists());
                } else {
                    assert_eq!(
                        json_file(&project),
                        serde_json::json!({"extensions":{"acme":{"enabled":true}}})
                    );
                }
            }
        },
    );
}

#[test]
fn declined_scope_and_failed_fresh_or_replacement_installs_preserve_safety() {
    fakes::within(
        "scope approval and install failure",
        Duration::from_secs(20),
        || {
            let setup = Setup::new();
            let source = setup.source("acme", &manifest("acme"), &[]);
            assert_eq!(
                scoped_install(&setup, &source, true, &mut io::Cursor::new(b"n\n")).unwrap(),
                1
            );
            let (global, project) = scope_files(&setup);
            assert!(!global.exists());
            assert!(!project.exists());
            let mut failed = manifest("acme");
            failed["install"] = serde_json::json!(["sh", "-c", "exit 1"]);
            write_file(&source.join("extension.json"), &failed.to_string());
            assert!(scoped_install(&setup, &source, false, &mut Unread).is_err());
            assert_eq!(
                json_file(&global),
                serde_json::json!({"extensions":{"acme":{"enabled":false}}})
            );
            assert_eq!(
                json_file(&project),
                serde_json::json!({"extensions":{"acme":{"enabled":true}}})
            );
            assert!(!setup.home().join("extensions/acme").exists());
            write_file(
                &source.join("extension.json"),
                &manifest("acme").to_string(),
            );
            assert_eq!(
                scoped_install(&setup, &source, false, &mut Unread).unwrap(),
                0
            );
            let old = fs::read(setup.home().join("extensions/acme/extension.json")).unwrap();
            write_file(&source.join("extension.json"), &failed.to_string());
            assert!(scoped_install(&setup, &source, false, &mut Unread).is_err());
            assert_eq!(
                fs::read(setup.home().join("extensions/acme/extension.json")).unwrap(),
                old
            );
        },
    );
}

/// The Fiber version an install checks against, as the manifest states.
const FIBER_VERSION: &str = "0.1.0";

/// The two extensions the update tests install, in listing order.
const NAME_A: &str = "github.com/acme/fiber-aaa";
const NAME_B: &str = "github.com/acme/fiber-bbb";

/// Their directories under `extensions/`.
const SLUG_A: &str = "github.com-acme-fiber-aaa";
const SLUG_B: &str = "github.com-acme-fiber-bbb";

/// A Fiber home and extension sources in a temporary directory, removed
/// on drop. No test touches the real home or the test process's
/// environment.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-cli-extension"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// An extension source directory named `dir` with this manifest and
    /// these provider files.
    fn source(
        &self,
        dir: &str,
        manifest: &serde_json::Value,
        providers: &[serde_json::Value],
    ) -> PathBuf {
        let mut manifest = manifest.clone();
        if manifest.get("providers").is_none() {
            let mut map = serde_json::Map::new();
            for data in providers {
                let Some(pname) = data.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let mut urls = Vec::new();
                for model in data
                    .get("models")
                    .and_then(|m| m.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(url) = model.get("base_url").and_then(|u| u.as_str())
                        && !urls.contains(&url)
                    {
                        urls.push(url);
                    }
                }
                map.insert(
                    pname.to_owned(),
                    serde_json::Value::Array(
                        urls.into_iter().map(serde_json::Value::from).collect(),
                    ),
                );
            }
            manifest["providers"] = serde_json::Value::Object(map);
        }
        let path = self.root.path().join("src").join(dir);
        write_file(&path.join("extension.json"), &manifest.to_string());
        for provider in providers {
            let name = provider["name"].as_str().unwrap();
            write_file(
                &path.join("providers").join(format!("{name}.json")),
                &provider.to_string(),
            );
        }
        path
    }
}

fn write_file(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

fn manifest(name: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 })
}

/// A provider with one model per base URL, named `m0`, `m1` and so on.
fn provider(name: &str, urls: &[&str]) -> serde_json::Value {
    let models: Vec<serde_json::Value> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            serde_json::json!({
                "id": format!("m{i}"),
                "protocol": "openai-responses",
                "base_url": url,
                "context_window": 1000,
            })
        })
        .collect();
    serde_json::json!({
        "name": name,
        "credential": { "env": "FIBER_TEST_UNSET_KEY" },
        "models": models,
    })
}

/// Input that fails the test if anything reads it: without a terminal
/// nothing is asked and nothing is read.
struct Unread;

impl Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("nothing is read without a terminal");
    }
}

impl BufRead for Unread {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        panic!("nothing is read without a terminal");
    }

    fn consume(&mut self, _: usize) {}
}

/// Runs `install` for `request`: its stderr, and its exit code or
/// failure.
fn run_install(
    home: &Path,
    request: Request,
    origin: &Origin,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, Result<i32, Failure>) {
    let clock = fakes::clock::FakeClock::new();
    let mut err = Vec::new();
    let got = install(
        home,
        request,
        None,
        FIBER_VERSION,
        origin,
        &*clock,
        Io {
            terminal,
            input,
            err: &mut err,
        },
    );
    (String::from_utf8(err).unwrap(), got)
}

/// Runs `update_all`: its stderr, and its exit code or failure.
fn run_update_all(
    home: &Path,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, Result<i32, Failure>) {
    let clock = fakes::clock::FakeClock::new();
    let mut err = Vec::new();
    let got = update_all(
        home,
        FIBER_VERSION,
        &Origin::github(),
        &*clock,
        Io {
            terminal,
            input,
            err: &mut err,
        },
    );
    (String::from_utf8(err).unwrap(), got)
}

/// Runs `remove` for `typed`: its stderr, and its exit code or failure.
fn run_remove(
    home: &Path,
    typed: &str,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, Result<i32, Failure>) {
    let clock = fakes::clock::FakeClock::new();
    let mut err = Vec::new();
    let got = remove(
        home,
        typed,
        &*clock,
        Io {
            terminal,
            input,
            err: &mut err,
        },
    );
    (String::from_utf8(err).unwrap(), got)
}

/// Runs `list`: its stdout, and its failure, if any.
fn run_list(home: &Path) -> (String, Result<(), Failure>) {
    let clock = fakes::clock::FakeClock::new();
    let mut out = Vec::new();
    let got = list(home, &*clock, &mut out);
    (String::from_utf8(out).unwrap(), got)
}

/// Installs the extension in `source` without a terminal, failing the
/// test when it does not exit zero.
fn install_fixture(home: &Path, source: &Path) {
    let mut unread = Unread;
    let (err, got) = run_install(
        home,
        Request::Path(source.to_path_buf()),
        &Origin::github(),
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0, "{err}");
}

/// The damaged line an install or update names, without the `fiber: `
/// prefix, for a directory whose record does not read.
const BROKEN_SKIPPED: &str = "`broken` is damaged, so its dependency minimums are unknown and the versions chosen did not count them; run `fiber extension reinstall broken`.";

/// The damaged line `list` prints for the same directory.
const BROKEN_DISPLAY: &str = "`broken` is damaged; run `fiber extension reinstall broken`.";

#[test]
fn install_from_a_path_without_a_terminal_installs_and_names_it() {
    let setup = Setup::new();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    let mut unread = Unread;
    let (err, got) = run_install(
        &setup.home(),
        Request::Path(source),
        &Origin::github(),
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0);
    assert_eq!(err, format!("fiber: installed {NAME_A}\n"));
    assert!(
        setup
            .home()
            .join("extensions")
            .join(SLUG_A)
            .join(".fiber.json")
            .exists()
    );
    assert!(!err.contains("Go ahead?"), "{err}");
}

#[test]
fn install_in_a_terminal_shows_the_summary_and_installs_on_yes() {
    let setup = Setup::new();
    let mut manifest = manifest(NAME_A);
    manifest["process"] = serde_json::json!({"program": "prog-acme", "args": ["--serve", "8080"]});
    let source = setup.source(
        "aaa",
        &manifest,
        &[provider(
            "p",
            &["http://b.test/v1", "http://a.test/v1", "http://b.test/v1"],
        )],
    );
    let mut input = io::Cursor::new(b"y\n".to_vec());
    let (err, got) = run_install(
        &setup.home(),
        Request::Path(source.clone()),
        &Origin::github(),
        true,
        &mut input,
    );
    assert_eq!(got.unwrap(), 0);
    // The plan canonicalises the source, so the summary shows the
    // resolved path (`/var` is a link to `/private/var` on macOS).
    let shown = source.canonicalize().unwrap();
    assert_eq!(
        err,
        format!(
            "Install {NAME_A} from {}\n\
             Version v1.0.0\n\
             Provider p: http://a.test/v1, http://b.test/v1\n\
             Runs the program: prog-acme --serve 8080\n\
             Go ahead? [y/N/s to show the full source] fiber: installed {NAME_A}\n",
            shown.display(),
        )
    );
}

#[test]
fn install_in_a_terminal_shows_what_the_extension_replaces() {
    let setup = Setup::new();
    let mut manifest = manifest(NAME_A);
    manifest["replaces"] = serde_json::json!(["shell", "read"]);
    let source = setup.source("aaa", &manifest, &[]);
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (err, _) = run_install(
        &setup.home(),
        Request::Path(source),
        &Origin::github(),
        true,
        &mut input,
    );
    assert!(err.contains("Replaces `shell`\nReplaces `read`\n"), "{err}");
}

#[test]
fn install_declined_installs_nothing_and_exits_one() {
    let setup = Setup::new();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (err, got) = run_install(
        &setup.home(),
        Request::Path(source),
        &Origin::github(),
        true,
        &mut input,
    );
    assert_eq!(got.unwrap(), 1);
    assert!(err.ends_with("fiber: nothing was installed.\n"), "{err}");
    assert!(
        !setup.home().join("extensions").join(SLUG_A).exists(),
        "a declined install puts nothing in place"
    );
}

#[test]
fn install_names_each_damaged_directory_before_the_summary() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("extensions").join("broken")).unwrap();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (err, got) = run_install(
        &setup.home(),
        Request::Path(source),
        &Origin::github(),
        true,
        &mut input,
    );
    assert_eq!(got.unwrap(), 1);
    assert!(
        err.starts_with(&format!("fiber: {BROKEN_SKIPPED}\n")),
        "{err}"
    );
    assert!(err.contains("Go ahead?"), "{err}");
}

#[test]
fn install_by_name_without_git_is_a_usage_failure() {
    let setup = Setup::new();
    let origin = Origin::new("fiber-no-such-git-program", |r| r.to_owned());
    let mut unread = Unread;
    let (_, got) = run_install(
        &setup.home(),
        Request::Install("github.com/acme/fiber-acme".into()),
        &origin,
        false,
        &mut unread,
    );
    let failure = got.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
}

#[test]
fn reinstall_of_a_path_extension_without_a_terminal_reinstalls_and_names_it() {
    let setup = Setup::new();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    install_fixture(&setup.home(), &source);
    let mut unread = Unread;
    let (err, got) = run_install(
        &setup.home(),
        Request::Reinstall(NAME_A.into()),
        &Origin::github(),
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0);
    assert_eq!(err, format!("fiber: installed {NAME_A}\n"));
}

#[test]
fn reinstall_names_each_other_damaged_directory_before_the_summary() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("extensions").join("broken")).unwrap();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    install_fixture(&setup.home(), &source);
    let mut unread = Unread;
    let (err, got) = run_install(
        &setup.home(),
        Request::Reinstall(NAME_A.into()),
        &Origin::github(),
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0);
    assert_eq!(
        err,
        format!("fiber: {BROKEN_SKIPPED}\nfiber: installed {NAME_A}\n")
    );
}

#[test]
fn reinstall_of_a_name_not_installed_fails() {
    let setup = Setup::new();
    let mut unread = Unread;
    let (_, got) = run_install(
        &setup.home(),
        Request::Reinstall(NAME_A.into()),
        &Origin::github(),
        false,
        &mut unread,
    );
    let failure = got.unwrap_err();
    assert_eq!(failure.code, ErrorCode::ExtensionMissing);
}

#[test]
fn update_of_one_extension_reinstalls_it() {
    let setup = Setup::new();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    install_fixture(&setup.home(), &source);
    let mut unread = Unread;
    let (err, got) = run_install(
        &setup.home(),
        Request::Update(NAME_A.into()),
        &Origin::github(),
        false,
        &mut unread,
    );
    assert_eq!(got.unwrap(), 0);
    assert_eq!(err, format!("fiber: installed {NAME_A}\n"));
}

#[test]
fn update_all_updates_each_requested_extension_in_order() {
    let setup = Setup::new();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    install_fixture(
        &setup.home(),
        &setup.source(
            "bbb",
            &manifest(NAME_B),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let mut unread = Unread;
    let (err, got) = run_update_all(&setup.home(), false, &mut unread);
    assert_eq!(got.unwrap(), 0);
    assert_eq!(
        err,
        format!("fiber: installed {NAME_A}\nfiber: installed {NAME_B}\n")
    );
}

#[test]
fn update_all_skips_extensions_nobody_asked_for() {
    let setup = Setup::new();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    install_fixture(
        &setup.home(),
        &setup.source(
            "bbb",
            &manifest(NAME_B),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let record = setup
        .home()
        .join("extensions")
        .join(SLUG_B)
        .join(".fiber.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
    json["requested"] = serde_json::Value::Bool(false);
    fs::write(&record, json.to_string()).unwrap();
    let mut unread = Unread;
    let (err, got) = run_update_all(&setup.home(), false, &mut unread);
    assert_eq!(got.unwrap(), 0);
    assert_eq!(err, format!("fiber: installed {NAME_A}\n"));
}

#[test]
fn update_all_stops_at_the_first_declined_install() {
    let setup = Setup::new();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    install_fixture(
        &setup.home(),
        &setup.source(
            "bbb",
            &manifest(NAME_B),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (err, got) = run_update_all(&setup.home(), true, &mut input);
    assert_eq!(got.unwrap(), 1);
    assert_eq!(err.matches("Go ahead?").count(), 1, "{err}");
    assert!(err.ends_with("fiber: nothing was installed.\n"), "{err}");
    assert!(!err.contains(NAME_B), "{err}");
}

#[test]
fn update_all_stops_at_the_first_failed_plan() {
    let setup = Setup::new();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    install_fixture(&setup.home(), &source);
    install_fixture(
        &setup.home(),
        &setup.source(
            "bbb",
            &manifest(NAME_B),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    fs::remove_dir_all(&source).unwrap();
    let mut unread = Unread;
    let (err, got) = run_update_all(&setup.home(), false, &mut unread);
    assert!(got.is_err(), "a missing source fails the plan");
    assert!(!err.contains(&format!("installed {NAME_B}")), "{err}");
}

#[test]
fn update_all_names_each_damaged_directory_once() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("extensions").join("broken")).unwrap();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let mut unread = Unread;
    let (err, got) = run_update_all(&setup.home(), false, &mut unread);
    assert_eq!(got.unwrap(), 0);
    assert_eq!(
        err,
        format!("fiber: {BROKEN_SKIPPED}\nfiber: installed {NAME_A}\n")
    );
}

#[test]
fn update_all_on_an_empty_home_is_zero_and_silent() {
    let setup = Setup::new();
    let mut unread = Unread;
    let (err, got) = run_update_all(&setup.home(), false, &mut unread);
    assert_eq!(got.unwrap(), 0);
    assert_eq!(err, "");
}

#[test]
fn remove_without_a_terminal_removes_and_names_it() {
    let setup = Setup::new();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let mut unread = Unread;
    let (err, got) = run_remove(&setup.home(), NAME_A, false, &mut unread);
    assert_eq!(got.unwrap(), 0);
    assert_eq!(err, format!("fiber: removed {NAME_A}\n"));
    assert!(!setup.home().join("extensions").join(SLUG_A).exists());
}

#[test]
fn remove_declined_removes_nothing_and_exits_one() {
    let setup = Setup::new();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (err, got) = run_remove(&setup.home(), NAME_A, true, &mut input);
    assert_eq!(got.unwrap(), 1);
    assert!(err.ends_with("fiber: nothing was removed.\n"), "{err}");
    assert!(setup.home().join("extensions").join(SLUG_A).exists());
}

#[test]
fn remove_of_a_name_not_installed_fails() {
    let setup = Setup::new();
    let mut unread = Unread;
    let (_, got) = run_remove(&setup.home(), NAME_A, false, &mut unread);
    let failure = got.unwrap_err();
    assert!(
        failure.message.contains("is not installed"),
        "{}",
        failure.message
    );
}

#[test]
fn list_prints_damaged_directories_then_each_extension_with_its_commit() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("extensions").join("broken")).unwrap();
    install_fixture(
        &setup.home(),
        &setup.source(
            "aaa",
            &manifest(NAME_A),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    install_fixture(
        &setup.home(),
        &setup.source(
            "bbb",
            &manifest(NAME_B),
            &[provider("p", &["http://127.0.0.1:1/v1"])],
        ),
    );
    let record = setup
        .home()
        .join("extensions")
        .join(SLUG_B)
        .join(".fiber.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
    json["source"] = serde_json::json!({"commit": "0123abcd"});
    fs::write(&record, json.to_string()).unwrap();
    let (out, got) = run_list(&setup.home());
    got.unwrap();
    assert_eq!(
        out,
        format!("{BROKEN_DISPLAY}\n{NAME_A} v1.0.0 local\n{NAME_B} v1.0.0 0123abcd\n")
    );
}

#[test]
fn list_of_an_empty_home_prints_nothing() {
    let setup = Setup::new();
    let (out, got) = run_list(&setup.home());
    got.unwrap();
    assert_eq!(out, "");
}

/// The child's marker: set, the test runs one public wrapper and exits
/// with its code.
const CHILD: &str = "FIBER_CLI_EXTENSION_CHILD";

/// The path the `install` child installs, on the command only: the child
/// cannot otherwise know the source the parent built.
const CHILD_SOURCE: &str = "FIBER_CLI_EXTENSION_SOURCE";

/// How long a child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(50);

/// The child's stdout before the test's own lines: the libtest harness
/// writes its prelude, then names the running test before it runs. No
/// result line ever follows, since the child exits from inside the
/// test. Fails the test when either marker is absent, so a harness
/// change is loud rather than a false pass.
fn strip_prelude<'a>(out: &'a str, name: &str) -> &'a str {
    const PRELUDE: &str = "running 1 test\n";
    let rest = match out.find(PRELUDE) {
        Some(at) => &out[at + PRELUDE.len()..],
        None => panic!("the child wrote no libtest prelude: {out:?}"),
    };
    match rest.strip_prefix(&format!("test {name} ... ")) {
        Some(command) => command,
        None => panic!("the child wrote no test line: {rest:?}"),
    }
}

fn child_test_name(case: &str) -> &'static str {
    match case {
        "list" => "list_command_gives_its_exit_code",
        "install" => "install_command_gives_its_exit_code",
        "update-one" => "update_one_command_gives_its_exit_code",
        "update-all" => "update_all_commands_give_their_exit_code",
        "remove" => "remove_command_gives_its_exit_code",
        "reinstall" => "reinstall_command_gives_its_exit_code",
        "install-by-name" => "install_by_name_command_gives_its_exit_code",
        unknown => panic!("unknown extension child case: {unknown}"),
    }
}

#[track_caller]
fn run_case(case: &str) {
    if let Ok(child_case) = std::env::var(CHILD) {
        assert_eq!(child_case, case);
        let clock = fakes::clock::FakeClock::new();
        let code = match child_case.as_str() {
            "list" => super::extension_list(&*clock),
            "install" => super::extension_install(
                &std::env::var(CHILD_SOURCE).unwrap(),
                false,
                FIBER_VERSION,
                &*clock,
            ),
            "update-one" => super::extension_update(Some(NAME_A), FIBER_VERSION, &*clock),
            "update-all" => super::extension_update(None, FIBER_VERSION, &*clock),
            "remove" => super::extension_remove(NAME_A, &*clock),
            "reinstall" => super::extension_reinstall(NAME_A, FIBER_VERSION, &*clock),
            "install-by-name" => super::extension_install(
                "github.com/acme/fiber-acme",
                false,
                FIBER_VERSION,
                &*clock,
            ),
            unknown => panic!("unknown extension child case: {unknown}"),
        };
        std::process::exit(code);
    }
    let setup = Setup::new();
    let source = setup.source(
        "aaa",
        &manifest(NAME_A),
        &[provider("p", &["http://127.0.0.1:1/v1"])],
    );
    let test_name = child_test_name(case);
    // Each case's home, prepared by the parent through the private
    // functions; no variable is set in the test process itself.
    let home = setup.root.path().join(format!("home-{case}"));
    fs::create_dir_all(&home).unwrap();
    if case != "install" && case != "install-by-name" {
        install_fixture(&home, &source);
    }
    let full_test_name = format!(
        "{}::{test_name}",
        module_path!().split_once("::").unwrap().1
    );
    let output = fakes::rerun_prepared(
        &full_test_name,
        &[(CHILD, case)],
        CHILD_DEADLINE,
        |command| {
            command.env("FIBER_HOME", &home).env(CHILD_SOURCE, &source);
            if case == "install-by-name" {
                // No `git` on the path: the install fails before any fetch.
                command.env("PATH", "");
            }
        },
    );
    let stdout_text = String::from_utf8(output.stdout).unwrap();
    let stdout = strip_prelude(&stdout_text, &full_test_name);
    let stderr = String::from_utf8(output.stderr).unwrap();
    match case {
        "list" => {
            assert_eq!(output.status.code(), Some(0), "{case}: {stderr}");
            assert_eq!(stdout, format!("{NAME_A} v1.0.0 local\n"));
            assert_eq!(stderr, "");
        }
        "install" => {
            assert_eq!(output.status.code(), Some(0), "{case}: {stderr}");
            assert_eq!(stdout, "");
            assert_eq!(stderr, format!("fiber: installed {NAME_A}\n"));
            assert!(
                home.join("extensions")
                    .join(SLUG_A)
                    .join(".fiber.json")
                    .exists()
            );
        }
        "update-one" | "update-all" | "reinstall" => {
            assert_eq!(output.status.code(), Some(0), "{case}: {stderr}");
            assert_eq!(stdout, "");
            assert_eq!(stderr, format!("fiber: installed {NAME_A}\n"));
        }
        "remove" => {
            assert_eq!(output.status.code(), Some(0), "{case}: {stderr}");
            assert_eq!(stdout, "");
            assert_eq!(stderr, format!("fiber: removed {NAME_A}\n"));
            assert!(!home.join("extensions").join(SLUG_A).exists());
        }
        "install-by-name" => {
            assert_eq!(output.status.code(), Some(2), "{case}: {stderr}");
            assert!(stderr.contains("Install git"), "{stderr}");
        }
        _ => unreachable!("every case has its own test"),
    }
}

#[test]
fn list_command_gives_its_exit_code() {
    run_case("list");
}

#[test]
fn install_command_gives_its_exit_code() {
    run_case("install");
}

#[test]
fn update_one_command_gives_its_exit_code() {
    run_case("update-one");
}

#[test]
fn update_all_commands_give_their_exit_code() {
    run_case("update-all");
}

#[test]
fn remove_command_gives_its_exit_code() {
    run_case("remove");
}

#[test]
fn reinstall_command_gives_its_exit_code() {
    run_case("reinstall");
}

#[test]
fn install_by_name_command_gives_its_exit_code() {
    run_case("install-by-name");
}

#[test]
fn install_summary_lists_a_lua_only_declared_provider() {
    let setup = Setup::new();
    let mut declared = manifest(NAME_A);
    declared["providers"] = serde_json::json!({"p": ["http://b.test/v1", "http://a.test/v1"]});
    let source = setup.source("aaa", &declared, &[]);
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (err, _) = run_install(
        &setup.home(),
        Request::Path(source),
        &Origin::github(),
        true,
        &mut input,
    );
    assert!(
        err.contains("Provider p: http://a.test/v1, http://b.test/v1\n"),
        "{err}"
    );
}
