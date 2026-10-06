//! The shared fakes that tests and jigs run against: stand-ins for everything
//! outside Fiber that a test needs (`docs/testing.md`, "Fakes"). A test-only
//! dependency of the crates that use it; no release binary contains it
//! (`docs/architecture.md`, "The call rules").

mod blocking;
mod cancel;
pub mod children;
mod client;
pub mod clock;
mod connect_proxy;
pub mod emit;
pub mod jobs;
mod oauth_server;
mod process_group;
mod provider_server;
mod rerun;
mod scripted_provider;
mod temp_dir;
mod watchdog;

use std::path::{Path, PathBuf};

pub use blocking::BlockingProvider;
pub use cancel::CancelToken;
pub use client::Client;
pub use connect_proxy::ConnectProxy;
pub use emit::Recorder;
pub use oauth_server::{OauthReply, OauthRequest, OauthServer};
pub use process_group::{WATCHDOG_SCRIPT, kill_group, kill_pid};
pub use provider_server::{ProviderServer, Request, Response, fingerprint};
pub use rerun::rerun;
pub use scripted_provider::{Scripted, ScriptedProvider, reply};
pub use temp_dir::TempDir;
pub use watchdog::Watchdog;

/// A stand-in script the kernel may exec: `dir/name` is a symlink to the
/// checked-in trampoline, and the freshly written `body` runs through
/// `/bin/sh` as `dir/name.sh` (`docs/testing.md`, "Waits and timeouts": a
/// test does not execute a file it wrote in the same run). The link keeps
/// the pid through `exec`, and a repeat call with the same `dir`/`name`
/// rewrites the body while keeping the link.
///
/// # Panics
///
/// When the body cannot be written or the link cannot be created.
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let sh = dir.join(format!("{name}.sh"));
    std::fs::write(&sh, body).unwrap_or_else(|err| panic!("writing {}: {err}", sh.display()));
    let link = dir.join(name);
    if std::fs::symlink_metadata(&link).is_err() {
        let trampoline =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("script-fixture/trampoline.sh");
        std::os::unix::fs::symlink(&trampoline, &link)
            .unwrap_or_else(|err| panic!("linking {}: {err}", link.display()));
    }
    link
}

/// The fixture Lua extension's directory: `extension.json`, `init.lua` and
/// the module it requires, one command per runtime behaviour a test exercises.
pub fn lua_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua-fixture")
}

/// The fake MCP stdio server's `server.sh`: it reads the directory named
/// by its first argument, holding `tools.json` and one `call-<tool>.json`
/// per tool.
pub fn mcp_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mcp-fixture/server.sh")
}

#[cfg(test)]
mod fixture_tests {
    use std::path::Path;
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::{mcp_fixture, script};
    use crate::TempDir;

    /// How long a script child may take. A wait that reaches it fails the test.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// Waits for `child` on a thread, so a hang fails at [`DEADLINE`].
    fn waited(child: Child) -> Output {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        match finished.recv_timeout(DEADLINE) {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => panic!("waited {DEADLINE:?} for the script: {err}"),
            Err(err) => panic!("waited {DEADLINE:?} for the script: {err}"),
        }
    }

    fn run(path: &Path, args: &[&str]) -> Output {
        let child = Command::new(path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        waited(child)
    }

    #[test]
    fn the_script_receives_its_arguments() {
        let dir = TempDir::new("fiber-script-args");
        let path = script(dir.path(), "tool", "printf '%s\\n' \"$@\"");
        let output = run(&path, &["a", "b c"]);
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "a\nb c\n");
    }

    #[test]
    fn the_script_keeps_the_spawned_pid() {
        let dir = TempDir::new("fiber-script-pid");
        let path = script(dir.path(), "tool", "echo $$");
        let child = Command::new(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let spawned = child.id();
        let output = waited(child);
        assert!(output.status.success());
        let printed: u32 = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(printed, spawned);
    }

    #[test]
    fn the_script_exits_with_the_bodys_status() {
        let dir = TempDir::new("fiber-script-status");
        let path = script(dir.path(), "tool", "exit 3");
        assert_eq!(run(&path, &[]).status.code(), Some(3));
    }

    #[test]
    fn a_second_call_runs_the_new_body() {
        let dir = TempDir::new("fiber-script-rewrite");
        let first = script(dir.path(), "tool", "echo one");
        assert_eq!(first, dir.path().join("tool"));
        assert_eq!(String::from_utf8(run(&first, &[]).stdout).unwrap(), "one\n");
        let second = script(dir.path(), "tool", "echo two");
        assert_eq!(second, first);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tool.sh")).unwrap(),
            "echo two"
        );
        let target = std::fs::read_link(&second).unwrap();
        assert!(
            target.ends_with("script-fixture/trampoline.sh"),
            "unexpected link target: {}",
            target.display()
        );
        assert_eq!(
            String::from_utf8(run(&second, &[]).stdout).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn the_mcp_fixture_points_at_server_sh() {
        let path = mcp_fixture();
        assert!(
            path.ends_with("mcp-fixture/server.sh"),
            "unexpected fixture path: {}",
            path.display()
        );
        assert!(path.is_file(), "missing fixture: {}", path.display());
    }
}
