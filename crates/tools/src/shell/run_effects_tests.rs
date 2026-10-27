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
use serde_json::Map;

use super::Shell;
use super::sandbox::Settings;

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
