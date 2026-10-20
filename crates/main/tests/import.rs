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
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use support::{Output, Setup, run_to_exit};

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

/// Every file under `dir` holding `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_holding(dir, needle, &mut found);
    found.sort();
    found
}

fn collect_holding(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            collect_holding(&path, needle, found);
        } else if path.is_file()
            && fs::read(&path)
                .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                .unwrap_or(false)
        {
            found.push(path);
        }
    }
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
        b"---\nname: greet\ndescription: Hello there.\ndisable-model-invocation: true\n---\n\nHello there.\n"
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
    assert!(files_holding(&setup.home(), "sk-canary-4").is_empty());
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
