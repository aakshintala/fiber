//! The read/filter path behind the setup questions
//! (`docs/user/install.md`, "Setup"): signed-in marking at any label,
//! model choices from signed-in providers only, empty providers skipping
//! steps 1 and 3, receipt short-circuiting, and the shell mapping.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, Cursor};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use config::{ImportSource, Secret, store_credential};
use extensions::Providers;
use serde_json::{Value, json};

use super::{
    Completions, Hub, SetupFlags, ask_line, ask_yes, completions_installed, display_home,
    display_source, import_offered, model_offers, parse_pick, pick_many, pick_one, provider_offers,
    receipt_completed, run_terminal, setup, shell_from_env, skipped_to, startup_line,
    step_completions, step_hub, step_import, step_model, step_providers, write_model,
};
use crate::test_support::write_record;

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-setup");
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Installs a provider `name` with `models`.
    fn install(&self, name: &str, models: Value) {
        let dir = self.home().join("extensions").join(name);
        let data = json!({
            "name": name,
            "models": models,
        });
        fs::create_dir_all(dir.join("providers")).unwrap();
        let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
        manifest["providers"] = Value::Object(
            [(name.to_owned(), json!(["http://x/v1"]))]
                .into_iter()
                .collect(),
        );
        fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
        write_record(&dir);
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }

    fn providers(&self) -> Providers {
        let (providers, notices) = Providers::load(&self.home()).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }
}

fn models(id: &str) -> Value {
    json!([{"id": id, "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000}])
}

fn thinking_models() -> Value {
    json!([{
        "id": "m",
        "protocol": "openai-responses",
        "base_url": "http://x/v1",
        "context_window": 1000,
        "thinking_levels": ["low", "high"],
        "thinking_default": "low",
    }])
}

fn var<'v>(vars: &'v HashMap<&str, &str>) -> impl Fn(&str) -> Option<OsString> + 'v {
    move |key| vars.get(key).map(OsString::from)
}

#[test]
fn signed_in_marks_any_stored_label() {
    let setup = Setup::new();
    setup.install("fake", models("m"));
    setup.install("other", models("o1"));
    let providers = setup.providers();
    let unsigned: Vec<String> = provider_offers(&setup.home(), &providers)
        .unwrap()
        .into_iter()
        .map(|offer| format!("{}{}", offer.name, offer.signed_in))
        .collect();
    assert_eq!(unsigned, vec!["fakefalse", "otherfalse"]);
    store_credential(&setup.home(), "fake", "work", &Secret::new("k".into())).unwrap();
    let mut signed: Vec<(String, bool)> = provider_offers(&setup.home(), &providers)
        .unwrap()
        .into_iter()
        .map(|offer| (offer.name, offer.signed_in))
        .collect();
    signed.sort();
    assert_eq!(
        signed,
        vec![("fake".to_owned(), true), ("other".to_owned(), false),]
    );
}

#[test]
fn model_choices_come_from_signed_in_providers_only() {
    let setup = Setup::new();
    setup.install("fake", thinking_models());
    setup.install("other", models("o1"));
    let providers = setup.providers();
    assert!(model_offers(&setup.home(), &providers).unwrap().is_empty());
    store_credential(&setup.home(), "fake", "default", &Secret::new("k".into())).unwrap();
    assert_eq!(
        model_offers(&setup.home(), &providers).unwrap(),
        vec![super::ModelOffer {
            reference: "fake/m".to_owned(),
            levels: vec!["low".to_owned(), "high".to_owned()],
        }]
    );
    store_credential(&setup.home(), "other", "default", &Secret::new("k".into())).unwrap();
    let references: Vec<String> = model_offers(&setup.home(), &providers)
        .unwrap()
        .into_iter()
        .map(|offer| offer.reference)
        .collect();
    assert_eq!(references, vec!["fake/m", "other/o1"]);
}

#[test]
fn empty_providers_offer_nothing() {
    let setup = Setup::new();
    let providers = setup.providers();
    assert!(
        provider_offers(&setup.home(), &providers)
            .unwrap()
            .is_empty()
    );
    assert!(model_offers(&setup.home(), &providers).unwrap().is_empty());
}

#[test]
fn completed_receipts_are_done_and_malformed_ones_are_offered() {
    let setup = Setup::new();
    let home_env = setup.home();
    assert!(!receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    assert!(!import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
    fs::create_dir_all(home_env.join(".claude")).unwrap();
    assert!(import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
    fs::write(setup.home().join("import-claude-code.json"), "not json").unwrap();
    assert!(!receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    assert!(import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
    fs::write(
        setup.home().join("import-claude-code.json"),
        json!({"source": "codex"}).to_string(),
    )
    .unwrap();
    assert!(!receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    fs::write(
        setup.home().join("import-claude-code.json"),
        json!({"source": "claude-code"}).to_string(),
    )
    .unwrap();
    assert!(receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    assert!(!import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
}

#[test]
fn shells_map_by_basename_with_a_dash_and_unknowns_map_to_none() {
    for (shell, expected) in [
        ("/bin/zsh", Some("zsh")),
        ("/usr/bin/bash", Some("bash")),
        ("/usr/local/bin/fish", Some("fish")),
        ("/bin/-zsh", Some("zsh")),
        ("/bin/sh", None),
        ("elvish", None),
    ] {
        let vars = HashMap::from([("SHELL", shell)]);
        assert_eq!(shell_from_env(&var(&vars)).as_deref(), expected, "{shell}");
    }
    let vars = HashMap::new();
    assert_eq!(shell_from_env(&var(&vars)), None);
    let vars = HashMap::from([("SHELL", "/bin/")]);
    assert_eq!(shell_from_env(&var(&vars)), None);
}

#[test]
fn completions_are_installed_only_with_the_line_present() {
    let setup = Setup::new();
    let home_env = setup.home();
    assert!(!completions_installed(&home_env, "zsh"));
    assert!(!completions_installed(&home_env, "elvish"));
    fs::write(home_env.join(".zshrc"), "export X=1\n").unwrap();
    assert!(!completions_installed(&home_env, "zsh"));
    fs::write(
        home_env.join(".zshrc"),
        "export X=1\neval \"$(fiber completion zsh)\"\n",
    )
    .unwrap();
    assert!(completions_installed(&home_env, "zsh"));
}

#[test]
fn a_startup_file_that_fails_to_read_holds_no_line() {
    let setup = Setup::new();
    let home_env = setup.home();
    // A directory fails to read with an error other than `NotFound`:
    // the step asks again instead of reporting the line is held.
    fs::create_dir(home_env.join(".zshrc")).unwrap();
    assert!(!completions_installed(&home_env, "zsh"));
}

fn answers(text: &str) -> Cursor<String> {
    Cursor::new(text.to_owned())
}

fn test_clock() -> Arc<dyn contract::clock::Clock> {
    fakes::clock::FakeClock::new()
}

fn test_locks() -> Arc<dyn contract::files::PathLock> {
    Arc::new(fakes::NoLock)
}

#[test]
fn parse_pick_takes_exact_names_then_numbers_at_and_around_the_edge() {
    let names = ["a".to_owned()];
    // A singleton exact name: only `==` finds it, so `!=` finds nothing.
    assert_eq!(parse_pick("a", &names), Some("a".to_owned()));
    let names = ["a".to_owned(), "b".to_owned()];
    assert_eq!(parse_pick("b", &names), Some("b".to_owned()));
    assert_eq!(parse_pick("1", &names), Some("a".to_owned()));
    assert_eq!(parse_pick("2", &names), Some("b".to_owned()));
    assert_eq!(parse_pick("0", &names), None);
    assert_eq!(parse_pick("3", &names), None);
    assert_eq!(parse_pick("c", &names), None);
    assert_eq!(parse_pick("", &names), None);
}

#[test]
fn display_source_names_each_shell() {
    assert_eq!(display_source(ImportSource::ClaudeCode), "Claude Code");
    assert_eq!(display_source(ImportSource::Codex), "Codex");
}

#[test]
fn display_home_shows_the_tilde_name_and_falls_back_without_one() {
    assert_eq!(
        display_home(std::path::Path::new("/home/u/.claude")),
        "~/.claude"
    );
    assert_eq!(display_home(std::path::Path::new("/")), "/");
}

#[test]
fn skipped_to_prints_the_step_and_its_command() {
    let mut err = Vec::new();
    skipped_to(&mut err, "providers", "run fiber login");
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "skipped providers: run fiber login\n"
    );
}

#[test]
fn ask_line_trims_and_reads_blank_as_blank() {
    let mut err = Vec::new();
    assert_eq!(
        ask_line(&mut answers("  hi  \n"), &mut err, "p: ").unwrap(),
        "hi"
    );
    assert_eq!(ask_line(&mut answers(""), &mut err, "p: ").unwrap(), "");
    assert_eq!(String::from_utf8(err).unwrap(), "p: p: ");
}

#[test]
fn ask_yes_takes_y_and_yes_and_reprompts_anything_else() {
    let mut err = Vec::new();
    assert!(ask_yes(&mut answers("y\n"), &mut err, "p: ").unwrap());
    assert!(!ask_yes(&mut answers("n\n"), &mut err, "p: ").unwrap());
    // Without `y` or `yes` the first `no` reprompts, so the `n` answers.
    assert!(ask_yes(&mut answers("yes\nn\n"), &mut err, "p: ").unwrap());
    // Without the negative words the first `no` reprompts, so the `y` answers.
    assert!(!ask_yes(&mut answers("no\ny\n"), &mut err, "p: ").unwrap());
}

#[test]
fn pick_one_takes_numbers_and_names_and_reprompts() {
    let names = vec!["a".to_owned(), "b".to_owned()];
    let mut err = Vec::new();
    assert_eq!(
        pick_one(&mut answers("2\n"), &mut err, "p: ", &names).unwrap(),
        Some("b".to_owned())
    );
    assert_eq!(
        pick_one(&mut answers("\n"), &mut err, "p: ", &names).unwrap(),
        None
    );
    assert_eq!(
        pick_one(&mut answers("zzz\n1\n"), &mut err, "p: ", &names).unwrap(),
        Some("a".to_owned())
    );
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("`zzz` is not listed; choose a number or name.\n")
    );
}

#[test]
fn pick_many_takes_numbers_and_names_dedupes_and_reprompts() {
    let names = vec!["a".to_owned(), "b".to_owned()];
    let mut err = Vec::new();
    assert_eq!(
        pick_many(&mut answers("a b\n"), &mut err, "p: ", &names).unwrap(),
        vec!["a".to_owned(), "b".to_owned()]
    );
    assert_eq!(
        pick_many(&mut answers("a a\n"), &mut err, "p: ", &names).unwrap(),
        vec!["a".to_owned()]
    );
    assert_eq!(
        pick_many(&mut answers("\n"), &mut err, "p: ", &names)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        pick_many(&mut answers("zzz\nb\n"), &mut err, "p: ", &names).unwrap(),
        vec!["b".to_owned()]
    );
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("`zzz` is not listed; choose numbers or names.\n")
    );
}

#[test]
fn startup_line_names_each_shell_beside_its_file() {
    let home = PathBuf::from("/h");
    assert_eq!(
        startup_line(&home, "zsh").unwrap(),
        (
            PathBuf::from("/h/.zshrc"),
            "eval \"$(fiber completion zsh)\"".to_owned()
        )
    );
    assert_eq!(
        startup_line(&home, "bash").unwrap(),
        (
            PathBuf::from("/h/.bashrc"),
            "eval \"$(fiber completion bash)\"".to_owned()
        )
    );
    assert_eq!(
        startup_line(&home, "fish").unwrap(),
        (
            PathBuf::from("/h/.config/fish/config.fish"),
            "eval \"$(fiber completion fish)\"".to_owned()
        )
    );
    assert!(startup_line(&home, "elvish").is_err());
}

#[test]
fn write_model_writes_the_model_then_the_thinking() {
    let setup = Setup::new();
    write_model(&setup.home(), Some("fake/m"), Some("high")).unwrap();
    assert_eq!(
        config::get_global(&setup.home(), "model").unwrap(),
        Some(json!("fake/m"))
    );
    assert_eq!(
        config::get_global(&setup.home(), "models.\"fake/m\".thinking").unwrap(),
        Some(json!("high"))
    );
}

#[test]
fn write_model_rides_thinking_on_the_stored_model_and_refuses_without_one() {
    let setup = Setup::new();
    assert!(
        write_model(&setup.home(), None, Some("high"))
            .unwrap_err()
            .contains("--thinking needs a model")
    );
    // Without the `Some` arm the stored model could never carry thinking.
    write_model(&setup.home(), Some("fake/m"), None).unwrap();
    write_model(&setup.home(), None, Some("low")).unwrap();
    assert_eq!(
        config::get_global(&setup.home(), "models.\"fake/m\".thinking").unwrap(),
        Some(json!("low"))
    );
}

#[test]
fn step_model_flags_write_and_complain() {
    let setup = Setup::new();
    let mut err = Vec::new();
    let flags = SetupFlags {
        model: Some("fake/m".to_owned()),
        ..Default::default()
    };
    // Without `||` the flag never writes: no config, and a skip line instead.
    assert_eq!(
        step_model(&setup.home(), None, &flags, None, &mut err).unwrap(),
        0
    );
    assert!(err.is_empty());
    assert_eq!(
        config::get_global(&setup.home(), "model").unwrap(),
        Some(json!("fake/m"))
    );
    // A fresh home: with no stored model the thinking flag has nothing to ride.
    let setup = Setup::new();
    let flags = SetupFlags {
        thinking: Some("high".to_owned()),
        ..Default::default()
    };
    let mut err = Vec::new();
    assert_eq!(
        step_model(&setup.home(), None, &flags, None, &mut err).unwrap(),
        1
    );
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("--thinking needs a model")
    );
}

#[test]
fn step_model_terminal_asks_with_the_picked_models_levels() {
    let setup = Setup::new();
    setup.install("fake", thinking_models());
    setup.install("other", models("o1"));
    store_credential(&setup.home(), "fake", "default", &Secret::new("k".into())).unwrap();
    store_credential(&setup.home(), "other", "default", &Secret::new("k".into())).unwrap();
    let providers = setup.providers();
    let flags = SetupFlags::default();
    let mut err = Vec::new();
    // Without `==` the first non-matching model answers: no thinking asked.
    assert_eq!(
        step_model(
            &setup.home(),
            Some(&providers),
            &flags,
            Some(&mut answers("1\n2\n")),
            &mut err
        )
        .unwrap(),
        0
    );
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("Thinking for fake/m (current: unset): low, high"));
    assert_eq!(
        config::get_global(&setup.home(), "model").unwrap(),
        Some(json!("fake/m"))
    );
    assert_eq!(
        config::get_global(&setup.home(), "models.\"fake/m\".thinking").unwrap(),
        Some(json!("high"))
    );
    // A model without levels writes with no thinking question: without
    // `&&` the step returns before writing.
    let mut err = Vec::new();
    assert_eq!(
        step_model(
            &setup.home(),
            Some(&providers),
            &flags,
            Some(&mut answers("2\n")),
            &mut err
        )
        .unwrap(),
        0
    );
    assert_eq!(
        config::get_global(&setup.home(), "model").unwrap(),
        Some(json!("other/o1"))
    );
}

#[test]
fn steps_without_an_answer_print_their_skip_lines() {
    let setup = Setup::new();
    let flags = SetupFlags::default();
    let mut err = Vec::new();
    assert_eq!(
        step_providers(&setup.home(), None, &flags, &test_clock(), None, &mut err).unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "skipped providers: run fiber login\n"
    );
    let providers = setup.providers();
    let mut err = Vec::new();
    assert_eq!(
        step_providers(
            &setup.home(),
            Some(&providers),
            &flags,
            &test_clock(),
            Some(&mut answers("")),
            &mut err
        )
        .unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "skipped providers: run fiber login\n"
    );
    let mut err = Vec::new();
    assert_eq!(
        step_import(&setup.home(), &flags, &test_clock(), None, &mut err).unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "skipped import: run fiber import\n"
    );
    let mut err = Vec::new();
    assert_eq!(step_completions(&flags, None, &mut err).unwrap(), 0);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "skipped completions: run fiber setup --completions <shell>\n"
    );
    let flags = SetupFlags {
        completions: Completions::No,
        ..Default::default()
    };
    let mut err = Vec::new();
    assert_eq!(step_completions(&flags, None, &mut err).unwrap(), 0);
    assert!(err.is_empty());
    let mut err = Vec::new();
    assert_eq!(
        step_hub(&setup.home(), &flags, &test_clock(), None, &mut err).unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "skipped hub: run fiber setup --hub\n"
    );
    let mut err = Vec::new();
    let flags = SetupFlags {
        hub: Hub::No,
        ..Default::default()
    };
    assert_eq!(
        step_hub(&setup.home(), &flags, &test_clock(), None, &mut err).unwrap(),
        0
    );
    assert!(err.is_empty());
}

/// The re-run child's marker: the parent sets it to one case name, the
/// child runs only that case's steps against the child's environment and
/// prints the codes, so env-reading steps stay hermetic
/// (`docs/testing/fakes.md`, "Re-running a test", and `login_tests.rs`).
const CHILD: &str = "FIBER_CLI_SETUP_CHILD";

/// The bound on a re-run child: half of nextest's per-test timeout less
/// the cleanup bound, like the login re-runs.
const CHILD_DEADLINE: Duration = Duration::from_secs(50);

fn child_case() -> Option<String> {
    std::env::var_os(CHILD).map(|value| value.to_string_lossy().into_owned())
}

fn child_test_name(function: &str) -> String {
    format!("{}::{function}", module_path!().split_once("::").unwrap().1)
}

fn child_home() -> PathBuf {
    PathBuf::from(std::env::var_os("FIBER_HOME").expect("the child sets FIBER_HOME"))
}

/// Re-runs this test's child with `case` set and `vars` added: the parent
/// arranges dirs and the child's standard input first.
fn spawn_child(
    setup: &Setup,
    function: &str,
    case: &str,
    vars: &[(&str, &str)],
    remove: &[&str],
    stdin: Option<&std::path::Path>,
) -> std::process::Output {
    let mut pairs = vec![(CHILD, case)];
    pairs.extend(vars.iter().copied());
    fakes::rerun_prepared(
        &child_test_name(function),
        &pairs,
        fakes::Deadline::after(CHILD_DEADLINE),
        |command| {
            for (key, value) in vars {
                command.env(key, value);
            }
            for key in remove {
                command.env_remove(key);
            }
            if let Some(input) = stdin {
                let file = std::fs::File::open(input).expect("the parent wrote the child's input");
                command.stdin(std::process::Stdio::from(file));
            }
            command.current_dir(setup.root.path());
        },
    )
}

/// The child's standard error from the `marker` line to the next marker
/// or the end, so batched cases assert their own output exactly. The
/// `### result` lines go to standard output, so a prompt without a line
/// feed never glues to one.
fn child_segment<'a>(stderr: &'a str, marker: &str) -> &'a str {
    let start = stderr
        .find(marker)
        .unwrap_or_else(|| panic!("the child printed {marker}"));
    let rest = &stderr[start + marker.len()..];
    let end = rest.find("### case ").unwrap_or(rest.len());
    &rest[..end]
}

/// The `### result <name>=<code>` tokens in the child's standard output:
/// split on the marker, since the test runner's own lines glue to them.
fn child_codes(stdout: &str) -> Vec<(&str, i32)> {
    stdout
        .split("### result ")
        .skip(1)
        .filter_map(|chunk| chunk.split_ascii_whitespace().next())
        .filter_map(|line| line.split_once('='))
        .map(|(name, code)| (name, code.parse().expect("the child printed codes")))
        .collect()
}

fn child_result(stdout: &str, name: &str) -> i32 {
    child_codes(stdout)
        .into_iter()
        .find(|(case, _)| *case == name)
        .unwrap_or_else(|| panic!("the child reported {name}: {stdout:?}"))
        .1
}

/// A child-to-parent protocol marker on standard error: the case whose
/// output the parent asserts next.
#[allow(
    clippy::print_stderr,
    reason = "child-to-parent test protocol, asserted by the parent"
)]
fn child_case_marker(case: &str) {
    eprintln!("### case {case}");
}

/// A child-to-parent file dump on standard error, asserted exactly.
#[allow(
    clippy::print_stderr,
    reason = "child-to-parent test protocol, asserted by the parent"
)]
fn child_dump(case: &str, text: &str) {
    eprintln!("### dump {case}\n{text}### end dump");
}

/// A child-to-parent exit code on standard output, away from prompts.
#[allow(
    clippy::print_stdout,
    reason = "child-to-parent test protocol, asserted by the parent"
)]
fn child_report(case: &str, code: i32) {
    println!("### result {case}={code}");
}

#[test]
fn setup_providers_flag_failure_stops_at_the_first_name() {
    if child_case().as_deref() == Some("providers-flags-ok") {
        let home = child_home();
        // Two unknown names: the first failure stops the loop, so the
        // failure line prints once, not twice.
        let flags = SetupFlags {
            provider: vec![
                "no-such-provider-for-the-test".to_owned(),
                "no-such-other-provider".to_owned(),
            ],
            ..Default::default()
        };
        child_case_marker("flags-ok");
        let code = step_providers(
            &home,
            None,
            &flags,
            &test_clock(),
            None,
            &mut std::io::stderr(),
        )
        .unwrap();
        child_report("flags-ok", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_providers_flag_failure_stops_at_the_first_name",
        "providers-flags-ok",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &[],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    // The step failed: the result carries the code, the process is clean.
    assert_eq!(child_result(&stdout, "flags-ok"), 2);
    // With `==` the failures fall through: code 0 and two failure lines.
    // The login names the bad flag first; the step names itself once.
    assert_eq!(
        child_segment(&stderr, "### case flags-ok\n"),
        "fiber: `no-such-provider-for-the-test` is neither an installed provider nor a declared secret; no provider is installed, and no installed extension declares a secret. Run `fiber --help` for usage.\nfiber: setup providers failed\n"
    );
}

#[test]
fn setup_providers_terminal_failure_names_the_step() {
    if child_case().as_deref() == Some("providers-terminal") {
        let home = child_home();
        let providers = Providers::load(&home).expect("the parent installed fake").0;
        let flags = SetupFlags::default();
        // The logins read this cursor. `fake` is stored, so its login
        // refuses, and the first failure stops the loop before `other`:
        // the failure line prints once, not twice.
        let mut stdin = Cursor::new("1,2\n".to_owned());
        let code = step_providers(
            &home,
            Some(&providers),
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        std::process::exit(code);
    }
    let setup = Setup::new();
    setup.install("fake", models("m"));
    setup.install("other", models("m"));
    // One stored credential: the listing marks fake signed in and other not.
    store_credential(&setup.home(), "fake", "default", &Secret::new("k".into())).unwrap();
    let output = spawn_child(
        &setup,
        "setup_providers_terminal_failure_names_the_step",
        "providers-terminal",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &[],
        None,
    );
    // The stored label refuses the login: usage failure, and the step
    // names itself on the way out.
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("credentials/fake/default is already stored"),
        "{stderr:?}"
    );
    assert!(!setup.home().join("credentials/other").exists());
    // The live marks pin `signed_in`: always false drops the fake mark,
    // always true adds an other mark.
    assert!(
        stderr.contains("Providers:\n  1) fake (signed in)\n  2) other\n"),
        "{stderr:?}"
    );
    // With `==` the failures fall through: code 0 and two failure lines.
    assert_eq!(stderr.matches("fiber: setup providers failed\n").count(), 1);
}

#[test]
fn setup_providers_pick_reads_the_key_from_setups_reader() {
    if child_case().as_deref() == Some("providers-pick-key") {
        let home = child_home();
        let providers = Providers::load(&home).expect("the parent installed fake").0;
        let flags = SetupFlags::default();
        // The pick and the key come from one reader: the login reads the
        // line after the pick.
        let mut stdin = Cursor::new("2\nk2\n".to_owned());
        let code = step_providers(
            &home,
            Some(&providers),
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        std::process::exit(code);
    }
    let setup = Setup::new();
    setup.install("fake", models("m"));
    setup.install("other", models("m"));
    store_credential(&setup.home(), "fake", "default", &Secret::new("k".into())).unwrap();
    let output = spawn_child(
        &setup,
        "setup_providers_pick_reads_the_key_from_setups_reader",
        "providers-pick-key",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &[],
        None,
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr:?}");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/other/default")).unwrap(),
        "k2"
    );
    assert_eq!(
        stderr.matches("stored credentials/").count(),
        1,
        "{stderr:?}"
    );
}

#[test]
fn setup_providers_flag_reads_the_key_from_setups_reader() {
    if child_case().as_deref() == Some("providers-flag-key") {
        let home = child_home();
        let flags = SetupFlags {
            provider: vec!["other".to_owned()],
            ..Default::default()
        };
        let mut stdin = Cursor::new("k3\n".to_owned());
        let code = step_providers(
            &home,
            None,
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        std::process::exit(code);
    }
    let setup = Setup::new();
    setup.install("other", models("m"));
    let output = spawn_child(
        &setup,
        "setup_providers_flag_reads_the_key_from_setups_reader",
        "providers-flag-key",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &[],
        None,
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr:?}");
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/other/default")).unwrap(),
        "k3"
    );
}

#[test]
fn setup_import_flag_and_answers_run_and_report() {
    if child_case().as_deref() == Some("import-ok") {
        let home = child_home();
        // The yes answer first: it runs the import before any receipt exists.
        let flags = SetupFlags::default();
        child_case_marker("terminal-yes");
        let mut stdin = Cursor::new("y\n".to_owned());
        let code = step_import(
            &home,
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        child_report("terminal-yes", code);
        let flags = SetupFlags {
            import: vec![ImportSource::ClaudeCode],
            ..Default::default()
        };
        child_case_marker("flags");
        let code = step_import(&home, &flags, &test_clock(), None, &mut std::io::stderr()).unwrap();
        child_report("flags", code);
        // The receipt landed: nothing pends, so the no answer is only shown done.
        child_case_marker("terminal-no");
        let flags = SetupFlags::default();
        let mut stdin = Cursor::new("n\n".to_owned());
        let code = step_import(
            &home,
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        child_report("terminal-no", code);
        return;
    }
    let setup = Setup::new();
    let person = setup.root.path().join("person");
    fs::create_dir_all(person.join(".claude")).unwrap();
    let output = spawn_child(
        &setup,
        "setup_import_flag_and_answers_run_and_report",
        "import-ok",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", person.to_str().unwrap()),
        ],
        &[],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "terminal-yes"), 0);
    assert_eq!(child_result(&stdout, "flags"), 0);
    assert_eq!(child_result(&stdout, "terminal-no"), 0);
    // Without `!` the flag never runs: a skip line instead of silence.
    // With `==` the accepted import prints the failure line instead of nothing.
    assert_eq!(
        child_segment(&stderr, "### case terminal-yes\n"),
        "Import Claude Code's configuration (~/.claude)? [y/N]: "
    );
    assert_eq!(child_segment(&stderr, "### case flags\n"), "");
    assert_eq!(
        child_segment(&stderr, "### case terminal-no\n"),
        "Claude Code: already imported (done)\n"
    );
    // Three results and nothing else: an empty source imports silently.
    assert_eq!(child_codes(&stdout).len(), 3);
    assert!(
        !stdout.contains("imported "),
        "an empty source imports silently: {stdout:?}"
    );
    // The yes answer ran the import: the receipt landed.
    assert!(
        setup.home().join("import-claude-code.json").exists(),
        "the import ran"
    );
}

#[test]
fn setup_import_terminal_reports_done_and_skips_when_nothing_pends() {
    if child_case().as_deref() == Some("import-empty") {
        let home = child_home();
        let flags = SetupFlags::default();
        // Readable but never read: both runs return before any question.
        let mut stdin = Cursor::new("\n".to_owned());
        child_case_marker("skip");
        let code = step_import(
            &home,
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        child_report("skip", code);
        fs::write(
            home.join("import-claude-code.json"),
            "{\"source\":\"claude-code\"}",
        )
        .expect("the child writes the receipt");
        child_case_marker("done");
        let mut stdin = Cursor::new("\n".to_owned());
        let code = step_import(
            &home,
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        child_report("done", code);
        return;
    }
    let setup = Setup::new();
    let person = setup.root.path().join("person");
    fs::create_dir_all(&person).unwrap();
    let output = spawn_child(
        &setup,
        "setup_import_terminal_reports_done_and_skips_when_nothing_pends",
        "import-empty",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", person.to_str().unwrap()),
        ],
        &[],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "skip"), 0);
    assert_eq!(child_result(&stdout, "done"), 0);
    // Without `!` the nothing-pending case prints no skip line; with the
    // receipt the done case prints a skip line it must not.
    assert_eq!(
        child_segment(&stderr, "### case skip\n"),
        "skipped import: run fiber import\n"
    );
    assert_eq!(
        child_segment(&stderr, "### case done\n"),
        "Claude Code: already imported (done)\n"
    );
}

#[test]
fn setup_import_flag_failure_without_a_home_names_the_step() {
    if child_case().as_deref() == Some("import-nohome") {
        let home = child_home();
        let flags = SetupFlags {
            import: vec![ImportSource::ClaudeCode],
            ..Default::default()
        };
        child_case_marker("flags");
        let code = step_import(&home, &flags, &test_clock(), None, &mut std::io::stderr()).unwrap();
        child_report("flags", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_import_flag_failure_without_a_home_names_the_step",
        "import-nohome",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &["HOME"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "flags"), 1);
    assert_eq!(
        child_segment(&stderr, "### case flags\n"),
        "fiber: HOME is not set, so the source home cannot be found.\nfiber: setup import failed\n"
    );
}

#[test]
fn setup_completions_flag_writes_each_shell_once() {
    if child_case().as_deref() == Some("completions-files") {
        let home = child_home();
        let mut flags = SetupFlags::default();
        // A directory reads as neither missing nor lined: the step reports
        // reading, while the always-true guard would report writing.
        fs::create_dir(home.join(".zshrc")).expect("the child seeds the directory");
        child_case_marker("zsh-dir");
        flags.completions = Completions::Yes("zsh".into());
        let code = step_completions(&flags, None, &mut std::io::stderr()).unwrap();
        child_report("zsh-dir", code);
        fs::remove_dir(home.join(".zshrc")).expect("the child reseeds");
        for (case, seed) in [("zsh-nonl", "x"), ("zsh-nl", "x\n"), ("zsh-empty", "")] {
            fs::write(home.join(".zshrc"), seed).expect("the child reseeds");
            child_case_marker(case);
            let code = step_completions(&flags, None, &mut std::io::stderr()).unwrap();
            let text = fs::read_to_string(home.join(".zshrc")).expect("the line landed");
            child_dump(case, &text);
            child_report(case, code);
        }
        for (case, shell, file) in [
            ("zsh-missing", "zsh", ".zshrc"),
            ("bash-missing", "bash", ".bashrc"),
            ("fish-missing", "fish", ".config/fish/config.fish"),
        ] {
            if home.join(".zshrc").exists() {
                fs::remove_file(home.join(".zshrc")).expect("the child reseeds");
            }
            child_case_marker(case);
            flags.completions = Completions::Yes(shell.into());
            let code = step_completions(&flags, None, &mut std::io::stderr()).unwrap();
            let text = fs::read_to_string(home.join(file)).expect("the line landed");
            child_dump(case, &text);
            child_report(case, code);
        }
        // The written line is held: asking again changes nothing.
        child_case_marker("zsh-again");
        flags.completions = Completions::Yes("zsh".into());
        let code = step_completions(&flags, None, &mut std::io::stderr()).unwrap();
        let text = fs::read_to_string(home.join(".zshrc")).expect("the line landed");
        child_dump("zsh-again", &text);
        child_report("zsh-again", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_completions_flag_writes_each_shell_once",
        "completions-files",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.home().to_str().unwrap()),
        ],
        &[],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "zsh-dir"), 1);
    for case in [
        "zsh-nonl",
        "zsh-nl",
        "zsh-empty",
        "zsh-missing",
        "bash-missing",
        "fish-missing",
        "zsh-again",
    ] {
        assert_eq!(child_result(&stdout, case), 0);
    }
    assert!(
        child_segment(&stderr, "### case zsh-dir\n").contains("reading"),
        "a directory fails reading, not writing: {stderr:?}"
    );
    let zsh = "eval \"$(fiber completion zsh)\"\n";
    assert_eq!(
        child_segment(&stderr, "### case zsh-nonl\n"),
        format!("### dump zsh-nonl\nx\n{zsh}### end dump\n")
    );
    assert_eq!(
        child_segment(&stderr, "### case zsh-nl\n"),
        format!("### dump zsh-nl\nx\n{zsh}### end dump\n")
    );
    assert_eq!(
        child_segment(&stderr, "### case zsh-empty\n"),
        format!("### dump zsh-empty\n{zsh}### end dump\n")
    );
    assert_eq!(
        child_segment(&stderr, "### case zsh-missing\n"),
        format!("### dump zsh-missing\n{zsh}### end dump\n")
    );
    assert_eq!(
        child_segment(&stderr, "### case bash-missing\n"),
        "### dump bash-missing\neval \"$(fiber completion bash)\"\n### end dump\n"
    );
    assert_eq!(
        child_segment(&stderr, "### case fish-missing\n"),
        "### dump fish-missing\neval \"$(fiber completion fish)\"\n### end dump\n"
    );
    assert_eq!(
        child_segment(&stderr, "### case zsh-again\n"),
        format!("### dump zsh-again\n{zsh}### end dump\n")
    );
}

#[test]
fn setup_completions_flag_failure_without_a_home_names_the_step() {
    if child_case().as_deref() == Some("completions-nohome") {
        let flags = SetupFlags {
            completions: Completions::Yes("zsh".into()),
            ..Default::default()
        };
        child_case_marker("flags");
        let code = step_completions(&flags, None, &mut std::io::stderr()).unwrap();
        child_report("flags", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_completions_flag_failure_without_a_home_names_the_step",
        "completions-nohome",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &["HOME"],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "flags"), 1);
    assert_eq!(
        child_segment(&stderr, "### case flags\n"),
        "fiber: setup completions failed: HOME must be an absolute path to install completions\n"
    );
}

#[test]
fn setup_completions_terminal_no_leaves_the_file_alone() {
    if child_case().as_deref() == Some("completions-ask-no") {
        // Reads the home to fail fast when the parent forgets it.
        child_home();
        let flags = SetupFlags::default();
        child_case_marker("terminal-no");
        let mut stdin = Cursor::new("n\n".to_owned());
        let code = step_completions(&flags, Some(&mut stdin), &mut std::io::stderr()).unwrap();
        child_report("terminal-no", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_completions_terminal_no_leaves_the_file_alone",
        "completions-ask-no",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.home().to_str().unwrap()),
            ("SHELL", "/bin/zsh"),
        ],
        &[],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "terminal-no"), 0);
    // Without `!` a no installs anyway: the file appears.
    assert!(
        stderr.contains("Install completions for zsh ("),
        "{stderr:?}"
    );
    assert!(
        !setup.home().join(".zshrc").exists(),
        "no answer installs nothing"
    );
}

#[test]
fn setup_completions_terminal_sees_what_the_file_holds() {
    if child_case().as_deref() == Some("completions-ask-sees") {
        let home = child_home();
        let flags = SetupFlags::default();
        // Without the line the question names the file, not a rerun.
        child_case_marker("without-line");
        let mut stdin = Cursor::new("n\n".to_owned());
        let code = step_completions(&flags, Some(&mut stdin), &mut std::io::stderr()).unwrap();
        child_report("without-line", code);
        // With the line the step reports done and asks about a rerun.
        fs::write(home.join(".zshrc"), "x\neval \"$(fiber completion zsh)\"\n").unwrap();
        child_case_marker("with-line");
        let mut stdin = Cursor::new("n\n".to_owned());
        let code = step_completions(&flags, Some(&mut stdin), &mut std::io::stderr()).unwrap();
        child_report("with-line", code);
        return;
    }
    let setup = Setup::new();
    fs::write(setup.home().join(".zshrc"), "x").unwrap();
    let output = spawn_child(
        &setup,
        "setup_completions_terminal_sees_what_the_file_holds",
        "completions-ask-sees",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.home().to_str().unwrap()),
            ("SHELL", "/bin/zsh"),
        ],
        &[],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "without-line"), 0);
    assert_eq!(child_result(&stdout, "with-line"), 0);
    // With `!=` the file without the line reads as installed: done and
    // rerun instead of the plain question.
    assert_eq!(
        child_segment(&stderr, "### case without-line\n"),
        "Install completions for zsh (~/.zshrc)? [y/N]: "
    );
    // The installed file stays exactly as it was: no rerun, no rewrite.
    assert_eq!(
        child_segment(&stderr, "### case with-line\n"),
        "Completions for zsh are installed (done).\n\
         Install completions for zsh again? [y/N]: "
    );
    assert_eq!(
        fs::read_to_string(setup.home().join(".zshrc")).unwrap(),
        "x\neval \"$(fiber completion zsh)\"\n"
    );
}

#[test]
fn setup_hub_failures_name_the_step() {
    if child_case().as_deref() == Some("hub") {
        let home = child_home();
        let flags = SetupFlags {
            hub: Hub::Yes,
            ..Default::default()
        };
        child_case_marker("yes");
        let code = step_hub(&home, &flags, &test_clock(), None, &mut std::io::stderr()).unwrap();
        child_report("yes", code);
        child_case_marker("terminal-yes");
        let flags = SetupFlags::default();
        let mut stdin = Cursor::new("y\n".to_owned());
        // A short home for the installed check: the long Fiber home only
        // breaks the install the yes answer runs.
        let short =
            PathBuf::from(std::env::var_os("SHORT_HOME").expect("the parent sets SHORT_HOME"));
        let code = step_hub(
            &short,
            &flags,
            &test_clock(),
            Some(&mut stdin),
            &mut std::io::stderr(),
        )
        .unwrap();
        child_report("terminal-yes", code);
        return;
    }
    let setup = Setup::new();
    // Too long to name a login service: locating it fails before anything
    // runs, on every platform.
    let long = setup.root.path().join(format!("h{}", "x".repeat(220)));
    let output = spawn_child(
        &setup,
        "setup_hub_failures_name_the_step",
        "hub",
        &[
            ("FIBER_HOME", long.to_str().unwrap()),
            ("HOME", setup.home().to_str().unwrap()),
            ("SHORT_HOME", setup.home().to_str().unwrap()),
        ],
        &[],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "yes"), 2);
    assert_eq!(child_result(&stdout, "terminal-yes"), 2);
    // With `==` the failure falls through: code 0 and no failure line.
    // The usage line names the generated byte count, so only its shape pins.
    let yes = child_segment(&stderr, "### case yes\n");
    assert!(
        yes.starts_with("fiber: Fiber home's path is too long to name the hub's login service"),
        "{yes:?}"
    );
    assert!(yes.ends_with("fiber: setup hub failed\n"), "{yes:?}");
    assert!(
        child_segment(&stderr, "### case terminal-yes\n").contains("fiber: setup hub failed\n")
    );
}

fn child_setup(flags: &SetupFlags) -> i32 {
    setup(
        flags,
        test_clock(),
        test_locks(),
        &fakes::children::grouped_output,
    )
}

#[test]
fn setup_without_answers_skips_every_step() {
    if child_case().as_deref() == Some("run-skip") {
        child_case_marker("skip");
        let flags = SetupFlags::default();
        let code = child_setup(&flags);
        child_report("skip", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_without_answers_skips_every_step",
        "run-skip",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
        ],
        &[],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(child_result(&stdout, "skip"), 0);
    // A flipped check stops here instead of running on: every later skip
    // line pins its check.
    assert_eq!(
        child_segment(&stderr, "### case skip\n"),
        "skipped providers: run fiber login\n\
         skipped import: run fiber import\n\
         skipped model: run fiber setup --model <model>\n\
         skipped completions: run fiber setup --completions <shell>\n\
         skipped hub: run fiber setup --hub\n"
    );
}

#[test]
fn setup_provider_failure_stops_before_the_later_steps() {
    if child_case().as_deref() == Some("run-provider") {
        child_case_marker("flags");
        let flags = SetupFlags {
            provider: vec!["no-such-provider-for-the-test".to_owned()],
            ..Default::default()
        };
        let code = child_setup(&flags);
        child_report("flags", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_provider_failure_stops_before_the_later_steps",
        "run-provider",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
        ],
        &[],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "flags"), 2);
    let segment = child_segment(&stderr, "### case flags\n");
    assert!(
        segment.contains("fiber: setup providers failed\n"),
        "{segment:?}"
    );
    assert!(!segment.contains("skipped"), "{segment:?}");
}

#[test]
fn setup_import_failure_without_a_home_stops_before_the_model_step() {
    if child_case().as_deref() == Some("run-import") {
        child_case_marker("flags");
        let flags = SetupFlags {
            import: vec![ImportSource::ClaudeCode],
            ..Default::default()
        };
        let code = child_setup(&flags);
        child_report("flags", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_import_failure_without_a_home_stops_before_the_model_step",
        "run-import",
        &[("FIBER_HOME", setup.home().to_str().unwrap())],
        &["HOME"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "flags"), 1);
    let segment = child_segment(&stderr, "### case flags\n");
    assert!(
        segment.contains("fiber: setup import failed\n"),
        "{segment:?}"
    );
    assert!(!segment.contains("skipped model"), "{segment:?}");
}

#[test]
fn setup_model_and_completions_flag_failures_stop_the_later_steps() {
    if child_case().as_deref() == Some("run-flags") {
        child_case_marker("thinking");
        let flags = SetupFlags {
            thinking: Some("high".to_owned()),
            ..Default::default()
        };
        let code = child_setup(&flags);
        child_report("thinking", code);
        child_case_marker("elvish");
        let flags = SetupFlags {
            completions: Completions::Yes("elvish".into()),
            ..Default::default()
        };
        let code = child_setup(&flags);
        child_report("elvish", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_model_and_completions_flag_failures_stop_the_later_steps",
        "run-flags",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
        ],
        &[],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "thinking"), 1);
    assert_eq!(child_result(&stdout, "elvish"), 1);
    let thinking = child_segment(&stderr, "### case thinking\n");
    assert!(
        thinking.contains("fiber: setup model failed: --thinking needs a model"),
        "{thinking:?}"
    );
    assert!(!thinking.contains("skipped completions"), "{thinking:?}");
    let elvish = child_segment(&stderr, "### case elvish\n");
    assert!(
        elvish.contains("fiber: setup completions failed:"),
        "{elvish:?}"
    );
    assert!(!elvish.contains("skipped hub"), "{elvish:?}");
}

fn child_workspace() -> PathBuf {
    PathBuf::from(std::env::var_os("WORKSPACE").expect("the parent sets WORKSPACE"))
}

fn child_run_terminal(flags: &SetupFlags, stdin: &mut dyn BufRead) -> i32 {
    run_terminal(
        &child_home(),
        &child_workspace(),
        flags,
        &test_clock(),
        &test_locks(),
        &fakes::children::grouped_output,
        stdin,
        &mut std::io::stderr(),
    )
    .unwrap_or_else(|error| panic!("the terminal run loads: {error}"))
}

#[test]
fn setup_terminal_run_without_answers_skips_every_step() {
    if child_case().as_deref() == Some("runterm-blank") {
        child_case_marker("blank");
        let flags = SetupFlags::default();
        let mut stdin = Cursor::new("\n".to_owned());
        let code = child_run_terminal(&flags, &mut stdin);
        child_report("blank", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_terminal_run_without_answers_skips_every_step",
        "runterm-blank",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
            ("WORKSPACE", setup.workspace().to_str().unwrap()),
        ],
        &["SHELL"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "blank"), 0);
    // A flipped check stops here: every later skip line pins its check,
    // and the hub step asks the one blank line.
    assert_eq!(
        child_segment(&stderr, "### case blank\n"),
        "skipped providers: run fiber login\n\
         skipped import: run fiber import\n\
         skipped model: run fiber setup --model <model>\n\
         skipped completions: run fiber setup --completions <shell>\n\
         Register the hub as a login service? [y/N]: "
    );
}

#[test]
fn setup_terminal_run_provider_failure_stops_before_the_import_step() {
    if child_case().as_deref() == Some("runterm-provider") {
        child_case_marker("flags");
        let flags = SetupFlags {
            provider: vec!["no-such-provider-for-the-test".to_owned()],
            ..Default::default()
        };
        let code = child_run_terminal(&flags, &mut Cursor::new(String::new()));
        child_report("flags", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_terminal_run_provider_failure_stops_before_the_import_step",
        "runterm-provider",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
            ("WORKSPACE", setup.workspace().to_str().unwrap()),
        ],
        &["SHELL"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "flags"), 2);
    let segment = child_segment(&stderr, "### case flags\n");
    assert!(
        segment.contains("fiber: setup providers failed\n"),
        "{segment:?}"
    );
    assert!(!segment.contains("skipped import"), "{segment:?}");
}

#[test]
fn setup_terminal_run_import_failure_without_a_home_names_the_step() {
    if child_case().as_deref() == Some("runterm-import") {
        child_case_marker("flags");
        let flags = SetupFlags {
            import: vec![ImportSource::ClaudeCode],
            ..Default::default()
        };
        let code = child_run_terminal(&flags, &mut Cursor::new(String::new()));
        child_report("flags", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_terminal_run_import_failure_without_a_home_names_the_step",
        "runterm-import",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("WORKSPACE", setup.workspace().to_str().unwrap()),
        ],
        &["HOME", "SHELL"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "flags"), 1);
    assert!(child_segment(&stderr, "### case flags\n").contains("fiber: setup import failed\n"));
}

#[test]
fn setup_terminal_run_model_and_completions_flag_failures_name_their_steps() {
    if child_case().as_deref() == Some("runterm-flags") {
        child_case_marker("thinking");
        let flags = SetupFlags {
            thinking: Some("high".to_owned()),
            ..Default::default()
        };
        let code = child_run_terminal(&flags, &mut Cursor::new(String::new()));
        child_report("thinking", code);
        child_case_marker("elvish");
        let flags = SetupFlags {
            completions: Completions::Yes("elvish".into()),
            ..Default::default()
        };
        let code = child_run_terminal(&flags, &mut Cursor::new(String::new()));
        child_report("elvish", code);
        return;
    }
    let setup = Setup::new();
    let output = spawn_child(
        &setup,
        "setup_terminal_run_model_and_completions_flag_failures_name_their_steps",
        "runterm-flags",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
            ("WORKSPACE", setup.workspace().to_str().unwrap()),
        ],
        &["SHELL"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "thinking"), 1);
    assert_eq!(child_result(&stdout, "elvish"), 1);
    assert!(
        child_segment(&stderr, "### case thinking\n")
            .contains("fiber: setup model failed: --thinking needs a model")
    );
    assert!(
        child_segment(&stderr, "### case elvish\n").contains("fiber: setup completions failed:")
    );
}

/// A placeholder variable name no test process sets.
const UNCONFIGURED_HOST: &str = "FIBER_TEST_SETUP_UNCONFIGURED_HOST";

/// Installs `acme`, whose one model's base URL fills `{workspace}` from
/// [`UNCONFIGURED_HOST`]: unset, the model is left out with a
/// `model_unconfigured` notice.
fn install_unconfigured(setup: &Setup) {
    let dir = setup.home().join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let manifest = json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
        "providers": {"acme": ["https://{workspace}/v1"]}});
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers/acme.json"),
        json!({
            "name": "acme",
            "placeholders": {"workspace": {"env": UNCONFIGURED_HOST}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn setup_terminal_run_prints_other_notices_but_no_model_unconfigured() {
    if child_case().as_deref() == Some("runterm-notices") {
        child_case_marker("notices");
        let flags = SetupFlags::default();
        let code = child_run_terminal(&flags, &mut Cursor::new("\n\n".to_owned()));
        child_report("notices", code);
        return;
    }
    let setup = Setup::new();
    install_unconfigured(&setup);
    // A cached model with no context window: a `model_invalid` notice.
    setup.install("bad", models("m1"));
    config::write_model_cache(
        &setup.home(),
        "bad",
        &json!([{"id": "m1", "protocol": "openai-responses",
                 "base_url": "http://x/v1", "context_window": 0}]),
    )
    .unwrap();
    let output = spawn_child(
        &setup,
        "setup_terminal_run_prints_other_notices_but_no_model_unconfigured",
        "runterm-notices",
        &[
            ("FIBER_HOME", setup.home().to_str().unwrap()),
            ("HOME", setup.root.path().join("person").to_str().unwrap()),
            ("WORKSPACE", setup.workspace().to_str().unwrap()),
        ],
        &["SHELL", UNCONFIGURED_HOST],
        None,
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(child_result(&stdout, "notices"), 0, "{stderr:?}");
    let segment = child_segment(&stderr, "### case notices\n");
    let lines: Vec<&str> = segment.lines().collect();
    // The invalid notice prints once, and the first question follows it.
    assert_eq!(segment.matches("bad/m1").count(), 1, "{segment:?}");
    assert!(lines[0].contains("bad/m1"), "{segment:?}");
    assert!(lines[0].contains("context_window"), "{segment:?}");
    assert_eq!(lines[1], "Providers:", "{segment:?}");
    assert!(!segment.contains("acme/m"), "{segment:?}");
    assert!(!segment.contains(UNCONFIGURED_HOST), "{segment:?}");
    assert!(!segment.contains("needs the setting"), "{segment:?}");
}
