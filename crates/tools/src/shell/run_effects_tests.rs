//! `Shell::run_with_effects` consumes the prepared `Effects`: the declared
//! decision survives a reload between `tool_call_started` and spawn. A call
//! declared confined either applies the sandbox or fails before its
//! command; a call declared not confined never applies a newly enabled
//! policy.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use contract::ActionId;
use contract::clock::Wake;
use contract::emit::Emit;
use contract::events::Event;
use contract::shapes::{ContentPart, DeclaredEffects};
use contract::tool::{Answered, Ask, Asking, Cancel, Effects, Output, Tool};
use serde_json::{Map, json};

use super::Shell;
use super::sandbox::Settings;
#[cfg(target_os = "linux")]
use super::sandbox::protected::ProtectedPaths;

struct NeverCancelled;

impl Cancel for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

struct NoEmit;

impl Emit for NoEmit {
    fn emit(&self, _event: &Event) {}
}

struct NoAsk;

impl Ask for NoAsk {
    fn action(&self) -> ActionId {
        ActionId("a_1".into())
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _asking: Asking) -> Answered {
        Answered::NoAnswer
    }
}

fn effects_with(confined: bool) -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
            confined,
        },
        subject: Some(String::new()),
        prefix: None,
        always_reviewed: false,
        review_when_confined: false,
        hosts: None,
    }
}

/// A fixture root under the owner's home: the sandbox grants the temp dir,
/// so the workspace and its denies live outside it.
fn fixture(prefix: &str) -> fakes::TempDir {
    let home = std::env::var_os("HOME").expect("HOME names the owner's home");
    fakes::TempDir::under(std::path::Path::new(&home), prefix)
}

fn arguments(command: &str, workdir: &std::path::Path) -> Map<String, serde_json::Value> {
    serde_json::json!({
        "command": command,
        "workdir": workdir.display().to_string(),
    })
    .as_object()
    .cloned()
    .expect("an object")
}

fn sandbox_disabled_error(output: &Output) -> &str {
    output
        .error
        .as_ref()
        .map(|error| error.message.as_str())
        .expect("the call fails")
}

#[cfg(target_os = "linux")]
#[test]
fn a_confined_read_only_call_needs_no_writable_protection_state() {
    let dir = fixture("fiber-read-only-protection");
    let workspace = dir.path().join("workspace");
    let fiber_home = dir.path().join("fiber-home");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&fiber_home).expect("create Fiber home");
    std::fs::write(workspace.join("note.txt"), "readable").expect("write note");
    let (protection, startup) =
        ProtectedPaths::open(&fiber_home, &workspace).expect("open protection");
    assert!(startup.is_none());
    // A read-only call needs no baseline. Refuse baseline storage so an
    // unnecessary transaction is visible as a failed tool call.
    std::fs::write(fiber_home.join("projects"), "not a directory").expect("block protection state");
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::new(Mutex::new(Settings {
            enabled: true,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home,
        })))
        .with_protection(protection);
    let args = arguments("cat note.txt", &workspace);
    let prepared = shell.effects(&args).expect("prepare read-only call");
    assert!(prepared.declared.confined);
    assert_eq!(
        prepared.declared.effects,
        vec![contract::shapes::Effect::Reads]
    );
    let output = fakes::within(
        "the confined read-only call finishes without baseline storage",
        fakes::Deadline::after(std::time::Duration::from_secs(10)),
        move || shell.run_with_effects(&args, &prepared, &NeverCancelled, &NoEmit, &NoAsk),
    );
    assert!(output.error.is_none(), "read succeeds: {:?}", output.error);
    assert!(fakes::text_of(&output).starts_with("readable"));
    assert_eq!(output.process.as_ref().and_then(|p| p.exit_code), Some(0));
    assert!(output.protected_paths.is_none());
    assert!(
        output
            .details
            .as_ref()
            .unwrap()
            .get("protected_paths")
            .is_none()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn only_confined_non_read_only_effects_restore_and_report_changes() {
    use contract::shapes::Effect::{Executes, Reads};

    for (confined, declared) in [
        (true, vec![]),
        (true, vec![Reads, Executes]),
        (false, vec![Executes]),
    ] {
        let dir = fixture("fiber-effects-protection");
        let workspace = dir.path().join("workspace");
        let fiber_home = dir.path().join("fiber-home");
        std::fs::create_dir_all(workspace.join(".fiber")).expect("create protected directory");
        std::fs::write(workspace.join(".fiber/note.txt"), "original").expect("write note");
        let (protection, _) =
            ProtectedPaths::open(&fiber_home, &workspace).expect("open protection");
        let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
            .with_sandbox(Arc::new(Mutex::new(Settings {
                enabled: true,
                allow_write: Vec::new(),
                deny_read: Vec::new(),
                fiber_home,
            })))
            .with_protection(protection);
        let args = arguments("printf changed > .fiber/note.txt; printf ran", &workspace);
        // Empty effects are not proof of a read-only call; a mixed call
        // must not inherit the exemption of its Reads component either.
        let mut prepared = effects_with(confined);
        prepared.declared.effects = declared;
        let output = fakes::within(
            "the call finishes and reports its protected-path changes",
            fakes::Deadline::after(std::time::Duration::from_secs(10)),
            move || shell.run_with_effects(&args, &prepared, &NeverCancelled, &NoEmit, &NoAsk),
        );
        assert!(output.error.is_none(), "call succeeds: {:?}", output.error);
        assert_eq!(output.process.as_ref().and_then(|p| p.exit_code), Some(0));
        let text = fakes::text_of(&output);
        assert!(text.starts_with("ran"), "the command ran: {text}");
        if confined {
            let expected = contract::shapes::ProtectedPaths {
                restored: vec![".fiber".to_owned()],
                failed: Vec::new(),
            };
            assert_eq!(output.protected_paths, Some(expected));
            assert_eq!(
                output.details.as_ref().unwrap().get("protected_paths"),
                Some(&json!({"restored": [".fiber"], "failed": []})),
            );
            assert!(text.contains("Protected path .fiber changed while a confined call ran;"));
            assert_eq!(
                std::fs::read_to_string(workspace.join(".fiber/note.txt")).unwrap(),
                "original",
            );
        } else {
            assert!(output.protected_paths.is_none());
            assert!(
                output
                    .details
                    .as_ref()
                    .unwrap()
                    .get("protected_paths")
                    .is_none()
            );
            assert!(!text.contains("Protected path"));
            assert_eq!(
                std::fs::read_to_string(workspace.join(".fiber/note.txt")).unwrap(),
                "changed",
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn an_unavailable_sandbox_does_not_declare_the_call_confined() {
    let dir = fixture("fiber-run-effects-unavailable");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![workspace.join("secret")],
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell =
        Shell::new(workspace.clone(), fakes::clock::FakeClock::new()).with_sandbox(settings);
    let args = arguments("cat secret", &workspace);

    let effects = shell.effects(&args).expect("valid shell arguments");

    assert!(
        !effects.declared.confined,
        "a deny overlapping the writable workspace stays on the reviewed path"
    );
}

#[test]
fn a_declared_confined_call_disabled_before_run_fails_without_running() {
    let dir = fixture("fiber-run-effects-off");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::clone(&settings));
    let args = arguments("touch marker", &workspace);
    // The reload lands between `tool_call_started` and the run: the shell
    // declared confined, but the policy is gone.
    *support::lock(&settings) = Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: PathBuf::new(),
    };
    let output =
        shell.run_with_effects(&args, &effects_with(true), &NeverCancelled, &NoEmit, &NoAsk);
    assert_eq!(
        sandbox_disabled_error(&output),
        "The sandbox could not be applied: the sandbox is off."
    );
    assert!(
        !workspace.join("marker").exists(),
        "the command never runs, so no unconfined retry happens"
    );
}

#[test]
fn an_unconfined_call_ignores_a_newly_enabled_policy() {
    let dir = fixture("fiber-run-effects-unconfined-on");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("secret"), "shh").expect("write the secret");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::clone(&settings));
    let mut args = arguments("cat secret", &workspace);
    args.insert("unconfined".into(), json!(true));
    // The decision is taken while the sandbox is off: the call declares
    // reviewed, as an unconfined call does with the sandbox on.
    let declared = shell.effects(&args).expect("valid shell arguments");
    assert!(!declared.declared.confined);
    // The reload turns the sandbox on with a deny over the secret after the
    // decision: the declared-not-confined call must still read it.
    *support::lock(&settings) = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![workspace.join("secret")],
        fiber_home: dir.path().join("fiber-home"),
    };
    let output = shell.run_with_effects(&args, &declared, &NeverCancelled, &NoEmit, &NoAsk);
    assert!(output.error.is_none(), "an unconfined call succeeds");
    assert!(
        matches!(output.content.first(), Some(ContentPart::Text { text }) if text.starts_with("shh")),
        "the newly denied file stays readable"
    );
}

#[test]
fn a_failed_confined_call_is_not_retried_but_an_explicit_unconfined_call_succeeds() {
    let dir = fixture("fiber-run-effects-retry");
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(home.join("secret"), "shh").expect("write the secret");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::clone(&settings));
    // The confined call is declared, then the policy disappears before the
    // run: it fails with the sandbox error and nothing runs, so nothing
    // reruns it unconfined.
    let marker = workspace.join("marker");
    let failed_args = arguments("touch marker", &workspace);
    *support::lock(&settings) = Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: PathBuf::new(),
    };
    let failed = shell.run_with_effects(
        &failed_args,
        &effects_with(true),
        &NeverCancelled,
        &NoEmit,
        &NoAsk,
    );
    assert_eq!(
        sandbox_disabled_error(&failed),
        "The sandbox could not be applied: the sandbox is off."
    );
    assert!(
        !marker.exists(),
        "the command never runs, so no unconfined retry happens"
    );
    // The model retries explicitly with `unconfined: true`: the declaration
    // leaves the sandbox even though the reloaded policy denies the secret,
    // and the retry reads it.
    *support::lock(&settings) = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![home.join("secret")],
        fiber_home: dir.path().join("fiber-home"),
    };
    let mut retry = arguments("cat secret", &home);
    retry.insert("unconfined".into(), json!(true));
    let declared = shell.effects(&retry).expect("valid shell arguments");
    assert!(
        !declared.declared.confined,
        "the explicit unconfined retry declares reviewed, not confined"
    );
    let output = shell.run_with_effects(&retry, &declared, &NeverCancelled, &NoEmit, &NoAsk);
    assert!(output.error.is_none(), "the explicit retry succeeds");
    assert!(
        matches!(output.content.first(), Some(ContentPart::Text { text }) if text.starts_with("shh")),
        "the retry reads the denied file outside the sandbox"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_unconfined_direct_run_reads_a_denied_path_while_a_confined_one_is_refused() {
    let dir = fixture("fiber-run-effects-direct");
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(home.join("secret"), "shh").expect("write the secret");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![home.join("secret")],
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::clone(&settings));
    // The confined direct run is refused the denied file.
    let confined_args = arguments("cat secret", &home);
    let declared = shell
        .effects(&confined_args)
        .expect("valid shell arguments");
    assert!(declared.declared.confined);
    let refused = shell.run(&confined_args, &NeverCancelled, &NoEmit);
    assert!(
        refused.error.is_some(),
        "a confined read of the denied file fails"
    );
    assert!(
        !matches!(&refused.content.first(), Some(ContentPart::Text { text }) if text.contains("shh")),
        "no denied byte reaches the caller"
    );
    // The unconfined direct run reads it with the sandbox on.
    let mut unconfined_args = arguments("cat secret", &home);
    unconfined_args.insert("unconfined".into(), json!(true));
    let declared = shell
        .effects(&unconfined_args)
        .expect("valid shell arguments");
    assert!(!declared.declared.confined);
    let read = shell.run(&unconfined_args, &NeverCancelled, &NoEmit);
    assert!(read.error.is_none(), "an unconfined read succeeds");
    assert!(
        matches!(read.content.first(), Some(ContentPart::Text { text }) if text.starts_with("shh")),
        "the denied file stays readable outside the sandbox"
    );
}

#[test]
fn a_declared_not_confined_call_ignores_a_newly_enabled_policy() {
    let dir = fixture("fiber-run-effects-on");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("secret"), "shh").expect("write the secret");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::clone(&settings));
    // The reload turns the sandbox on with a deny over the secret after the
    // decision: the declared-not-confined call must still read it.
    *support::lock(&settings) = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![workspace.join("secret")],
        fiber_home: dir.path().join("fiber-home"),
    };
    let args = arguments("cat secret", &workspace);
    let output = shell.run_with_effects(
        &args,
        &effects_with(false),
        &NeverCancelled,
        &NoEmit,
        &NoAsk,
    );
    assert!(output.error.is_none(), "an unconfined call succeeds");
    assert!(
        matches!(output.content.first(), Some(ContentPart::Text { text }) if text.starts_with("shh")),
        "the newly denied file stays readable"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_declared_confined_call_still_on_applies_the_sandbox() {
    let dir = fixture("fiber-run-effects-confined");
    // The sandbox workspace holds no deny: the denied home sits beside it,
    // so the read-deny contract applies instead of the overlap notice.
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(home.join("secret"), "shh").expect("write the secret");
    std::fs::write(home.join("sibling"), "ok").expect("write the sibling");
    let settings = Arc::new(Mutex::new(Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![home.join("secret")],
        fiber_home: dir.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), fakes::clock::FakeClock::new())
        .with_sandbox(Arc::clone(&settings));
    let secret = shell.run_with_effects(
        &arguments("cat secret", &home),
        &effects_with(true),
        &NeverCancelled,
        &NoEmit,
        &NoAsk,
    );
    // A confined denial is a nonzero exit naming the refusal: the policy
    // applied and the kernel refused the read. "Permission denied" is the
    // sandbox's signature; a missing file would say "No such file".
    assert!(
        secret
            .error
            .as_ref()
            .is_some_and(|error| error.code == contract::ErrorCode::NonzeroExit),
        "the denied read fails"
    );
    assert!(
        matches!(&secret.content.first(), Some(ContentPart::Text { text }) if text.contains("Permission denied") && !text.contains("shh")),
        "the sandbox refused the read"
    );
    let sibling = shell.run_with_effects(
        &arguments("cat sibling", &home),
        &effects_with(true),
        &NeverCancelled,
        &NoEmit,
        &NoAsk,
    );
    assert!(sibling.error.is_none(), "a sibling read succeeds");
    assert!(
        matches!(sibling.content.first(), Some(ContentPart::Text { text }) if text.starts_with("ok")),
        "the sibling stays readable under confinement"
    );
}
