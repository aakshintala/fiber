// debt: the unit registration and candidate tests below repeat tests/commands/completion.rs; the duplication stays until a public-API test kills the same mutants. Upgrade trigger: a mutants run on the cut shows no MISSED.
use std::collections::BTreeSet;
use std::ffi::OsStr;

use clap::{Arg, ArgAction, Command};

use super::{
    command, extension_candidates_in, model_candidates_in, registration, registration_for_exe,
    separate_help_and_version_flags,
};
use crate::completion::{Shell, visible};

#[test]
fn registration_contains_each_supported_shell_and_executable() {
    let executable = OsStr::new("/test/bin/fiber");
    for (shell, marker) in [
        (Shell::Bash, "complete -o nospace"),
        (Shell::Zsh, "#compdef fiber"),
        (
            Shell::Fish,
            "complete --keep-order --exclusive --command fiber",
        ),
    ] {
        let script = String::from_utf8(registration_for_exe(shell, executable).unwrap()).unwrap();
        assert!(script.contains("FIBER_COMPLETE="), "{shell:?}: {script}");
        assert!(script.contains(shell.name()), "{shell:?}: {script}");
        assert!(script.contains("fiber"), "{shell:?}: {script}");
        assert!(script.contains("/test/bin/fiber"), "{shell:?}: {script}");
        assert!(script.contains(marker), "{shell:?}: {script}");
        assert_eq!(
            registration_for_exe(shell, executable).unwrap(),
            registration_for_exe(shell, executable).unwrap(),
            "{shell:?} registration changed between calls"
        );
    }
}

#[test]
fn registration_uses_the_current_executable() {
    let executable = std::env::current_exe().unwrap();
    let executable = executable.to_str().unwrap();
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let script = String::from_utf8(registration(shell).unwrap()).unwrap();
        assert!(script.contains(executable), "{shell:?}: {script}");
        assert_eq!(registration(shell).unwrap(), registration(shell).unwrap());
    }
}

#[test]
fn registration_bash_strips_bashdefault() {
    let script = String::from_utf8(
        registration_for_exe(Shell::Bash, OsStr::new("/test/bin/fiber")).unwrap(),
    )
    .unwrap();
    assert!(!script.contains("bashdefault"), "{script}");
}

#[test]
fn extension_candidates_include_healthy_installs_in_directory_order() {
    let home = fakes::TempDir::new("fiber-completion-extensions");
    write_extension_record(home.path(), "b", "example/beta");
    write_extension_record(home.path(), "a", "example/alpha");
    write_extension_record(home.path(), ".staging", "example/staging");
    let damaged = home.path().join("extensions/damaged");
    std::fs::create_dir_all(&damaged).unwrap();
    std::fs::write(damaged.join(".fiber.json"), "not json").unwrap();

    assert_eq!(
        candidate_values(extension_candidates_in(OsStr::new("example/"), home.path())),
        set(&["example/alpha", "example/beta"])
    );
    assert_eq!(
        candidate_values(extension_candidates_in(
            OsStr::new("example/b"),
            home.path()
        )),
        set(&["example/beta"])
    );
}

#[test]
fn extension_candidates_for_missing_home_are_empty() {
    let home = fakes::TempDir::new("fiber-completion-extensions-missing");
    assert!(extension_candidates_in(OsStr::new(""), &home.path().join("missing")).is_empty());
}

fn write_extension_record(home: &std::path::Path, dir: &str, name: &str) {
    let directory = home.join("extensions").join(dir);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(".fiber.json"),
        serde_json::json!({
            "name": name,
            "version": "1.0.0",
            "requested": true,
            "source": { "path": "/test" }
        })
        .to_string(),
    )
    .unwrap();
}

fn candidate_values(candidates: Vec<String>) -> BTreeSet<String> {
    candidates.into_iter().collect()
}

fn set(words: &[&str]) -> BTreeSet<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn model_candidates_keep_unique_ids_drop_ambiguous_and_logged_out_ids() {
    let root = fakes::TempDir::new("fiber-completion-models");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_provider(
        &home,
        "alpha-extension",
        "alpha",
        &["model-unique", "model-shared"],
        true,
    );
    write_provider(
        &home,
        "beta-extension",
        "beta",
        &["model-shared", "model-second"],
        true,
    );
    write_provider(
        &home,
        "offline-extension",
        "offline",
        &["model-logged-out"],
        false,
    );

    let (providers, config, notices) = cli::providers_and_config(&home, &workspace).unwrap();
    assert_eq!(
        providers.names().collect::<Vec<_>>(),
        ["alpha", "beta", "offline"],
        "notices: {notices:?}"
    );
    assert_eq!(
        providers.switched_on(&config).collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(
        config::read_model_cache(&home, "alpha")
            .unwrap()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        providers.data("alpha").models.len(),
        2,
        "notices: {notices:?}"
    );
    assert_eq!(
        candidate_values(model_candidates_in(OsStr::new("model-"), &home, &workspace)),
        set(&["model-second", "model-unique"])
    );
    assert!(
        model_candidates_in(OsStr::new("model-shared"), &home, &workspace).is_empty(),
        "ambiguous bare ids are not usable"
    );
    assert!(
        model_candidates_in(OsStr::new("model-logged"), &home, &workspace).is_empty(),
        "logged-out providers contribute no candidates"
    );
}

#[test]
fn model_candidates_without_a_cached_list_are_empty() {
    let root = fakes::TempDir::new("fiber-completion-models-cacheless");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_provider_data(&home, "cacheless-extension", "cacheless", &[], true, false);

    assert!(model_candidates_in(OsStr::new(""), &home, &workspace).is_empty());
}

#[test]
fn model_candidates_without_a_provider_source_are_empty() {
    let root = fakes::TempDir::new("fiber-completion-models-source-error");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.json"), "not json").unwrap();

    assert!(model_candidates_in(OsStr::new(""), &home, &workspace).is_empty());
}

#[test]
fn model_candidates_do_not_add_the_scripted_provider() {
    let root = fakes::TempDir::new("fiber-completion-scripted");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.json"),
        r#"{"model":"scripted/a-script.lua"}"#,
    )
    .unwrap();

    assert!(model_candidates_in(OsStr::new("a-script"), &home, &workspace).is_empty());
}

#[test]
fn both_candidate_sources_are_empty_for_hostile_homes_and_damaged_records() {
    let root = fakes::TempDir::new("fiber-completion-hostile-home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let home_file = root.path().join("home-file");
    std::fs::write(&home_file, "not a directory").unwrap();
    assert!(extension_candidates_in(OsStr::new(""), &home_file).is_empty());
    assert!(model_candidates_in(OsStr::new(""), &home_file, &workspace).is_empty());

    let damaged_home = root.path().join("damaged-home");
    let damaged = damaged_home.join("extensions/damaged");
    std::fs::create_dir_all(&damaged).unwrap();
    std::fs::write(damaged.join(".fiber.json"), "not json").unwrap();
    assert!(extension_candidates_in(OsStr::new(""), &damaged_home).is_empty());
    assert!(model_candidates_in(OsStr::new(""), &damaged_home, &workspace).is_empty());
}

fn write_provider(
    home: &std::path::Path,
    extension: &str,
    provider: &str,
    models: &[&str],
    logged_in: bool,
) {
    write_provider_data(home, extension, provider, models, logged_in, true);
}

fn write_provider_data(
    home: &std::path::Path,
    extension: &str,
    provider: &str,
    models: &[&str],
    logged_in: bool,
    write_cache: bool,
) {
    let url = "https://example.test/v1";
    let directory = home.join("extensions").join(extension);
    std::fs::create_dir_all(directory.join("providers")).unwrap();
    std::fs::write(
        directory.join(".fiber.json"),
        serde_json::json!({
            "name": extension,
            "version": "1.0.0",
            "requested": true,
            "source": { "path": "/test" }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("extension.json"),
        serde_json::json!({
            "name": extension,
            "version": "1.0.0",
            "fiber": "0.0.0",
            "api": 1,
            "providers": { provider: [url] }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("providers").join(format!("{provider}.json")),
        serde_json::json!({"name": provider, "models": []}).to_string(),
    )
    .unwrap();
    if write_cache {
        config::write_model_cache(
            home,
            provider,
            &serde_json::json!(
                models
                    .iter()
                    .map(|id| {
                        serde_json::json!({
                            "id": id,
                            "protocol": "openai-responses",
                            "base_url": url,
                            "context_window": 100000
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        )
        .unwrap();
    }
    if logged_in {
        let credential = home.join("credentials").join(provider).join("default");
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(credential, "stored credential").unwrap();
    }
}

#[cfg(unix)]
#[test]
fn registration_for_exe_rejects_non_utf8_path() {
    use std::os::unix::ffi::OsStrExt as _;

    let executable = OsStr::from_bytes(b"/test/bin/fiber\xff");
    assert!(registration_for_exe(Shell::Bash, executable).is_err());
}

#[test]
fn version_short_splitting_ignores_partial_version_matches() {
    let has_short = |cmd: &Command, id: &str| {
        cmd.get_arguments()
            .find(|arg| arg.get_id() == id)
            .and_then(|arg| arg.get_short())
    };
    // An unrelated `-v` short is not the version flag: it keeps its
    // spelling and gains no version-short sibling.
    let other = Command::new("fiber").arg(Arg::new("other").short('v').action(ArgAction::SetTrue));
    let other = separate_help_and_version_flags(other);
    assert_eq!(has_short(&other, "other"), Some('v'));
    assert!(has_short(&other, "fiber-completion-version-short").is_none());
    // A version flag without `-v` gains no version-short spelling.
    let long_only = Command::new("fiber").arg(
        Arg::new("version-flag")
            .long("version")
            .action(ArgAction::SetTrue),
    );
    let long_only = separate_help_and_version_flags(long_only);
    assert!(has_short(&long_only, "fiber-completion-version-short").is_none());
    // The real version spelling still splits: the flag loses `-v` and the
    // sibling carries it.
    let version = Command::new("fiber").arg(
        Arg::new("version-flag")
            .short('v')
            .long("version")
            .action(ArgAction::SetTrue),
    );
    let version = separate_help_and_version_flags(version);
    assert_eq!(has_short(&version, "version-flag"), None);
    assert_eq!(
        has_short(&version, "fiber-completion-version-short"),
        Some('v')
    );
}

#[test]
fn model_candidates_keep_an_id_listed_twice_by_its_only_provider() {
    let root = fakes::TempDir::new("fiber-completion-models-duplicate");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_provider_data(
        &home,
        "dup-extension",
        "dup",
        &["model-dup", "model-dup"],
        true,
        true,
    );

    // One provider listing an id twice is not ambiguity: only a second
    // provider disqualifies the bare id.
    assert_eq!(
        candidate_values(model_candidates_in(OsStr::new("model-"), &home, &workspace)),
        set(&["model-dup"])
    );
}

/// The dynamic command carries the visible grammar: the plan's Interfaces
/// attaches the wrapper to `visible()`'s output, and the two deviations
/// are the alias split the pinned engine needs and the help/version
/// respelling its short-flag dedupe needs.
#[test]
fn dynamic_command_keeps_visible_grammar_apart_from_documented_respells() {
    fn spellings(cmd: &clap::Command) -> BTreeSet<(Option<char>, Option<String>)> {
        cmd.get_arguments()
            .map(|arg| (arg.get_short(), arg.get_long().map(str::to_owned)))
            .collect()
    }
    fn paths(cmd: &clap::Command, path: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            let named = format!("{path} {}", sub.get_name());
            out.push(named.clone());
            paths(sub, &named, out);
        }
    }
    let mut dynamic = command();
    dynamic.build();
    let mut shown = visible(&crate::completion::PARSER);
    shown.build();
    // The wrapper hides its session copy, which keeps the engine from
    // offering the root `--model` completer after the `session` word.
    assert!(dynamic.find_subcommand("session").is_some());
    assert!(shown.find_subcommand("session").is_none());
    let mut dynamic_paths = Vec::new();
    paths(&dynamic, "fiber", &mut dynamic_paths);
    let mut shown_paths = Vec::new();
    paths(&shown, "fiber", &mut shown_paths);
    let shown_set: BTreeSet<_> = shown_paths.iter().cloned().collect();
    for path in &shown_paths {
        assert!(dynamic_paths.contains(path), "dynamic lost {path}");
    }
    // Extra dynamic paths are only the hidden session copy and visible
    // aliases split into siblings.
    for path in &dynamic_paths {
        if shown_set.contains(path) || path == "fiber session" || path.starts_with("fiber session ")
        {
            continue;
        }
        let (parent, alias) = path.rsplit_once(' ').unwrap();
        assert!(
            shown_aliases_contain(&shown, parent, alias),
            "dynamic added {path} beyond visible aliases"
        );
    }
    // Shared paths offer the same spellings once the help/version
    // respelling is mapped back.
    for path in &shown_paths {
        let route: Vec<&str> = path.split(' ').skip(1).collect();
        let mut dynamic_cmd = &dynamic;
        let mut shown_cmd = &shown;
        for name in &route {
            dynamic_cmd = dynamic_cmd.find_subcommand(name).unwrap();
            shown_cmd = shown_cmd.find_subcommand(name).unwrap();
        }
        let mut dynamic_spellings = BTreeSet::new();
        for (short, long) in spellings(dynamic_cmd) {
            dynamic_spellings.insert((short, long));
        }
        let mut expected = spellings(shown_cmd);
        // The respelling this wrapper owns: one help/version spelling
        // becomes two help args and a version-short sibling.
        if expected.remove(&(Some('h'), Some("help".to_owned()))) {
            expected.insert((None, Some("help".to_owned())));
            expected.insert((Some('h'), None));
        }
        if expected.remove(&(Some('v'), Some("version".to_owned()))) {
            expected.insert((None, Some("version".to_owned())));
            expected.insert((Some('v'), None));
        }
        // Untouched help/version spellings (help-only subcommands) stay.
        assert_eq!(dynamic_spellings, expected, "{path}");
    }
}

fn shown_aliases_contain(shown: &clap::Command, parent: &str, alias: &str) -> bool {
    let route: Vec<&str> = parent.split(' ').skip(1).collect();
    let mut cmd = shown;
    for name in route {
        let Some(next) = cmd.find_subcommand(name) else {
            return false;
        };
        cmd = next;
    }
    cmd.get_subcommands().any(|sub| {
        sub.get_name() == alias || sub.get_visible_aliases().any(|visible| visible == alias)
    })
}
