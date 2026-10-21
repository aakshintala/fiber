//! Binary-level tests of `fiber import` (`docs/configuration.md`,
//! "Importing"; `docs/testing.md`, "Levels"): the built `fiber` runs with a
//! private `HOME` holding a fake `~/.claude` or `~/.codex` beside a fresh
//! `FIBER_HOME`, and nothing here touches the network. Every run carries a
//! wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Child;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    KillGroup, Output, Setup, Socket, files_holding, first_line, group_alive, hello, run_to_exit,
    spawn_watched, until, until_close,
};

/// Runs `fiber import` with `args` to exit, reaping its process group on
/// the test's deadline.
#[track_caller]
fn import(setup: &Setup, args: &[&str]) -> Output {
    let command = setup.fiber(args);
    Output::from(run_to_exit(setup.deadline, "fiber import", command))
}

/// Writes `content` to `rel` under the test root, creating parents.
fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A fake `~/.claude` mirroring the probed layout: a skill, a flat
/// command, instruction content, two mapped servers, and unmapped entries
/// including a secret.
fn claude_home(root: &Path) {
    write(
        root,
        ".claude/skills/review/SKILL.md",
        "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
    );
    write(root, ".claude/commands/greet.md", "Hello there.\n");
    write(root, ".claude/CLAUDE.md", "Be kind.\n");
    write(
        root,
        ".claude/settings.json",
        &json!({
            "model": "opus",
            "mcpServers": {
                "notes": {"command": "notes-server", "args": ["--fast"]},
                "remote": {"url": "https://mcp.example.com/x"},
            },
        })
        .to_string(),
    );
    write(root, ".claude/agents/worker.md", "worker\n");
    write(root, ".claude/auth.json", "{\"token\": \"x\"}\n");
}

/// A fake `~/.codex` mirroring the probed layout: a skill, a flat prompt,
/// instruction content, two mapped servers, and a secret.
fn codex_home(root: &Path) {
    write(
        root,
        ".codex/skills/lint/SKILL.md",
        "---\nname: lint\ndescription: Lints.\n---\n\nLint body.\n",
    );
    write(root, ".codex/prompts/plan.md", "Plan it.\n");
    write(root, ".codex/AGENTS.md", "Be thorough.\n");
    write(
        root,
        ".codex/config.toml",
        "[mcp_servers.alpha]\n\
         command = \"alpha-server\"\n\
         \n\
         [mcp_servers.beta]\n\
         url = \"https://beta.example.com/mcp\"\n",
    );
    write(root, ".codex/auth.json", "{\"token\": \"x\"}\n");
}

fn receipt(home: &Path, name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(home.join(name)).unwrap()).unwrap()
}

#[test]
fn claude_import_lands_mapped_items_and_names_the_rest() {
    let setup = Setup::new();
    claude_home(setup.root.path());
    let run = import(&setup, &["import", "claude-code"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "imported skill review\n\
         imported command greet\n\
         imported instruction CLAUDE.md\n\
         imported server notes\n\
         imported server remote\n\
         left behind agents: agents are not configuration\n\
         left behind auth.json: credentials are never imported\n\
         left behind settings.json model: only `mcpServers` is imported from settings.json\n"
    );
    let home = setup.home();
    assert_eq!(
        fs::read(home.join("skills/review/SKILL.md")).unwrap(),
        b"---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n"
    );
    assert_eq!(
        fs::read(home.join("skills/greet/SKILL.md")).unwrap(),
        b"---\nname: greet\ndescription: \"Hello there.\"\ndisable-model-invocation: true\n---\n\nHello there.\n"
    );
    assert_eq!(fs::read(home.join("CLAUDE.md")).unwrap(), b"Be kind.\n");
    let servers: Value =
        serde_json::from_str(&fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        servers.pointer("/mcp/servers/notes").unwrap(),
        &json!({"command": "notes-server", "args": ["--fast"]})
    );
    assert_eq!(
        servers.pointer("/mcp/servers/remote").unwrap(),
        &json!({"url": "https://mcp.example.com/x"})
    );
    let receipt = receipt(&home, "import-claude-code.json");
    assert_eq!(receipt["source"], "claude-code");
    assert_eq!(receipt["imported"]["skills"], json!(["review"]));
    assert_eq!(
        receipt["imported"]["mcp_servers"],
        json!(["notes", "remote"])
    );
}

#[test]
fn codex_import_lands_mapped_items_and_names_the_rest() {
    let setup = Setup::new();
    codex_home(setup.root.path());
    let run = import(&setup, &["import", "codex"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "imported skill lint\n\
         imported command plan\n\
         imported instruction AGENTS.md\n\
         imported server alpha\n\
         imported server beta\n\
         left behind auth.json: credentials are never imported\n"
    );
    let home = setup.home();
    assert!(home.join("skills/lint/SKILL.md").exists());
    assert!(home.join("skills/plan/SKILL.md").exists());
    assert_eq!(fs::read(home.join("AGENTS.md")).unwrap(), b"Be thorough.\n");
    let servers: Value =
        serde_json::from_str(&fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        servers.pointer("/mcp/servers/alpha").unwrap(),
        &json!({"command": "alpha-server"})
    );
    let receipt = receipt(&home, "import-codex.json");
    assert_eq!(receipt["source"], "codex");
    assert_eq!(receipt["imported"]["mcp_servers"], json!(["alpha", "beta"]));
}

#[test]
fn secret_values_reach_neither_files_nor_output() {
    let setup = Setup::new();
    write(
        setup.root.path(),
        ".claude/settings.json",
        &json!({"mcpServers": {
            "vault": {"command": "v", "env": {"VAULT_TOKEN": "sk-canary-4"}},
            "plain": {"command": "p"},
        }})
        .to_string(),
    );
    let run = import(&setup, &["import", "claude-code"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains("server vault"),
        "stdout: {}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("sk-canary-4"),
        "stdout: {}",
        run.stdout
    );
    assert!(
        !run.stderr.contains("sk-canary-4"),
        "stderr: {}",
        run.stderr
    );
    assert!(files_holding(&[setup.home()], b"sk-canary-4").is_empty());
}

#[test]
fn duplicate_run_after_source_removal_succeeds_unchanged() {
    let setup = Setup::new();
    claude_home(setup.root.path());
    let first = import(&setup, &["import", "claude-code"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    fs::remove_dir_all(setup.root.path().join(".claude")).unwrap();
    let second = import(&setup, &["import", "claude-code"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.stdout, first.stdout);
    assert!(setup.home().join("skills/review/SKILL.md").exists());
}

#[test]
fn failed_run_writes_no_receipt_and_retry_completes() {
    let setup = Setup::new();
    claude_home(setup.root.path());
    // A directory where the global file goes aborts the import before
    // anything is written.
    fs::create_dir_all(setup.home().join("config.json")).unwrap();
    let failed = import(&setup, &["import", "claude-code"]);
    assert_ne!(failed.code, Some(0));
    assert!(!setup.home().join("import-claude-code.json").exists());
    assert!(!setup.home().join("skills/review/SKILL.md").exists());
    fs::remove_dir_all(setup.home().join("config.json")).unwrap();
    let retried = import(&setup, &["import", "claude-code"]);
    assert_eq!(retried.code, Some(0), "stderr: {}", retried.stderr);
    assert!(setup.home().join("import-claude-code.json").exists());
    assert!(setup.home().join("skills/review/SKILL.md").exists());
    let receipt = receipt(&setup.home(), "import-claude-code.json");
    assert_eq!(
        receipt["imported"]["mcp_servers"],
        json!(["notes", "remote"])
    );
}

#[test]
fn unknown_source_is_refused_naming_the_two_sources() {
    let setup = Setup::new();
    let run = import(&setup, &["import", "vim"]);
    assert_eq!(run.code, Some(2), "stdout: {}", run.stdout);
    assert!(
        run.stderr.contains("claude-code") && run.stderr.contains("codex"),
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn missing_source_home_imports_nothing() {
    let setup = Setup::new();
    let run = import(&setup, &["import", "codex"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(setup.home().join("import-codex.json").exists());
}

/// A session driven only for `import`: start, subscribe, and wait for the
/// startup every command needs. No provider: the import answers on the
/// reader thread and never reaches the loop.
struct Driven {
    child: Child,
    guard: KillGroup,
    watchdog: Watchdog,
    stderr: mpsc::Receiver<String>,
    client: Socket,
    deadline: fakes::Deadline,
}

impl Driven {
    /// Starts `fiber session` on the setup's home: the person's home
    /// stays the setup's root, so both import runs below read identical
    /// source fixtures.
    fn start(setup: &Setup) -> Self {
        let session = doors::mint("s_");
        let workspace = setup.workspace();
        let mut command = setup.fiber(&[
            "session",
            "--id",
            &session,
            "--workspace",
            workspace.to_str().unwrap(),
        ]);
        let (mut child, watchdog) = spawn_watched(&mut command);
        let guard = KillGroup(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (tx, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            stderr_pipe
                .take(u64::MAX)
                .read_to_string(&mut text)
                .unwrap();
            tx.send(text).unwrap();
        });
        let deadline = setup.deadline;
        first_line(deadline, &lines);
        let client = Socket::connect(deadline, &setup.session_socket(&session));
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // The startup every command needs: `clients` for the subscribe
        // and `extensions_loaded`, in any order.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        until(&client, "startup", |line| {
            if line["kind"] == "clients" {
                seen_clients = true;
            }
            if line["kind"] == "extensions_loaded" {
                seen_extensions = true;
            }
            seen_clients && seen_extensions
        });
        Self {
            child,
            guard,
            watchdog,
            stderr,
            client,
            deadline,
        }
    }

    /// The driver's accepted `result` for `import`: the session socket
    /// speaks the driver protocol, where the session is the connection
    /// itself and carries no `session_id`.
    fn import(&self, source: Option<&str>) -> Value {
        let id = doors::mint("c_");
        let line = match source {
            None => json!({"id": id, "command": "import"}),
            Some(source) => json!({"id": id, "command": "import", "args": {"source": source}}),
        };
        self.client.send(&line.to_string());
        let answers = until(&self.client, "the import answer", |line| {
            line["kind"] == "command_accepted"
                && line["payload"]["command_id"].as_str() == Some(id.as_str())
        });
        let answer = answers.into_iter().next_back().unwrap();
        answer["payload"]["result"].clone()
    }

    fn finish(self) {
        self.client.send(r#"{"id":"close","command":"close"}"#);
        until_close(&self.client);
        let Driven {
            mut child,
            guard,
            watchdog,
            stderr,
            client: _,
            deadline,
        } = self;
        let group = child.id();
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            tx.send(child.wait()).unwrap();
        });
        let status = deadline.recv(&done).unwrap().unwrap();
        let text = deadline.recv(&stderr).unwrap();
        assert!(status.success(), "{text}");
        assert!(!group_alive(deadline, group));
        std::mem::forget(guard);
        watchdog.stand_down(deadline.cleanup_phase());
    }
}

/// Every import-landed file under `home` but the receipts, as relative
/// paths with bytes, in order: the session home's own durable files
/// under `projects/` and its log are the harness's, not the import's.
fn tree(home: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending = vec![home.to_path_buf()];
    while let Some(path) = pending.pop() {
        if path.is_dir() && !path.is_symlink() {
            for entry in fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
            continue;
        }
        if path.is_file() {
            let rel = path.strip_prefix(home).unwrap().display().to_string();
            if rel == "import-claude-code.json"
                || rel == "import-codex.json"
                || rel.starts_with("projects/")
                || rel.starts_with("logs/")
            {
                continue;
            }
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
    out.sort();
    out
}

/// Copies one directory tree: the binary's Fiber home starts from the
/// same provider harness the session's home holds.
fn copy_dir(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `fiber import` lands the same files and left-behind list the driver
/// command answers: the session imports into the setup's home first,
/// then the binary imports into a second Fiber home seeded with the same
/// provider harness, both reading the same fake person home.
#[test]
fn fiber_import_matches_the_driver_command() {
    let setup = Setup::new();
    claude_home(setup.root.path());
    codex_home(setup.root.path());
    // The session needs a model to start, though the import never
    // reaches the loop; the fake server sees no request.
    let provider = ProviderServer::start_with_fallback([], hello()).unwrap();
    setup.provider(&provider);
    let driven = Driven::start(&setup);
    let claude_result = driven.import(Some("claude-code"));
    let codex_result = driven.import(Some("codex"));
    driven.finish();
    let binary_home = setup.root.path().join("binary-home");
    fs::create_dir_all(&binary_home).unwrap();
    fs::copy(
        setup.home().join("config.json"),
        binary_home.join("config.json"),
    )
    .unwrap();
    copy_dir(
        &setup.home().join("extensions"),
        &binary_home.join("extensions"),
    );
    for source in ["claude-code", "codex"] {
        let mut command = setup.fiber(&["import", source]);
        command.env("FIBER_HOME", &binary_home);
        let run = Output::from(run_to_exit(setup.deadline, "fiber import", command));
        assert_eq!(run.code, Some(0), "stdout: {}", run.stdout);
    }
    for (result, name) in [
        (claude_result, "import-claude-code.json"),
        (codex_result, "import-codex.json"),
    ] {
        let receipt = receipt(&binary_home, name);
        assert_eq!(
            result,
            json!({"imported": receipt["imported"].clone(), "left_behind": receipt["left_behind"].clone()}),
            "{name}",
        );
        assert!(setup.home().join(name).is_file());
        assert!(binary_home.join(name).is_file());
    }
    assert_eq!(tree(&setup.home()), tree(&binary_home));
}
