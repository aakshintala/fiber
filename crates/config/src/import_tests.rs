//! Tests for the one-time import: mapping rows, the narrow reader, the
//! never-overwrite rule, secrets and the receipt.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};

use fakes::TempDir;
use serde_json::{Value, json};

use super::*;
use crate::write::atomic::{Stage, fail_at, fail_nth};

/// One Fiber home beside one source home.
struct Fixture {
    tmp: TempDir,
    clock: std::sync::Arc<fakes::clock::FakeClock>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tmp: TempDir::new("fiber-import"),
            clock: fakes::clock::FakeClock::new(),
        }
    }

    /// Writes `content` to `rel` under the source home, creating parents.
    fn write_source(&self, rel: &str, content: &str) {
        self.tmp.write(&format!("source/{rel}"), content);
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn source(&self) -> PathBuf {
        self.tmp.path().join("source")
    }

    /// Runs the import for `source` against this fixture.
    fn run(&self, source: ImportSource) -> Result<ImportReport, ConfigError> {
        import_source(&self.home(), &self.source(), source, self.clock.as_ref())
    }

    /// Every file under Fiber home holding `needle`.
    fn files_holding(&self, needle: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        collect_holding(&self.home(), needle, &mut found);
        found.sort();
        found
    }
}

fn collect_holding(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap();
    for entry in entries {
        let path = entry.unwrap().path();
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

/// A Claude Code source mirroring the probed layout.
fn claude_source(source: &Path) {
    fs::create_dir_all(source.join("skills/review")).unwrap();
    fs::write(
        source.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
    )
    .unwrap();
    fs::create_dir_all(source.join("commands")).unwrap();
    fs::write(
        source.join("commands/greet.md"),
        "\nHello there.\n\nUsage: greet.\n",
    )
    .unwrap();
    fs::write(source.join("CLAUDE.md"), "Be kind.\n").unwrap();
    fs::write(
        source.join("settings.json"),
        json!({
            "model": "opus",
            "mcpServers": {
                "notes": {
                    "command": "notes-server",
                    "args": ["--fast"],
                    "env": {"PORT": "8080"},
                    "required": true
                },
                "remote": {"url": "https://mcp.example.com/x"}
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::create_dir_all(source.join("agents")).unwrap();
    fs::write(source.join("agents/worker.md"), "worker\n").unwrap();
    fs::write(source.join("auth.json"), "{\"token\": \"x\"}\n").unwrap();
}

/// A Codex source mirroring the probed layout.
fn codex_source(source: &Path) {
    fs::create_dir_all(source.join("skills/lint")).unwrap();
    fs::write(
        source.join("skills/lint/SKILL.md"),
        "---\nname: lint\ndescription: Lints.\n---\n\nLint body.\n",
    )
    .unwrap();
    fs::create_dir_all(source.join("prompts")).unwrap();
    fs::write(source.join("prompts/plan.md"), "Plan it.\n").unwrap();
    fs::write(source.join("AGENTS.md"), "Be thorough.\n").unwrap();
    fs::write(
        source.join("config.toml"),
        "screen_reader_detection_done = true\n\
         \n\
         [features]\n\
         fancy = true\n\
         \n\
         [mcp_servers.alpha]\n\
         command = \"alpha-server\"\n\
         args = [\"--x\", \"y\"]\n\
         env = { PORT = \"1\" }\n\
         required = true\n\
         \n\
         [mcp_servers.beta]\n\
         url = \"https://beta.example.com/mcp\"\n",
    )
    .unwrap();
    fs::write(source.join("auth.json"), "{\"token\": \"x\"}\n").unwrap();
}

/// The left-behind reason for `item`, or "" when it is not left behind.
fn reason_for(report: &ImportReport, item: &str) -> String {
    report
        .left_behind
        .iter()
        .find(|left| left.item == item)
        .map(|left| left.reason.clone())
        .unwrap_or_default()
}

#[test]
fn claude_import_lands_every_mapped_item() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, ["review"]);
    assert_eq!(report.imported.commands, ["greet"]);
    assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
    assert_eq!(report.imported.mcp_servers, ["notes", "remote"]);
    assert_eq!(
        fs::read(fixture.home().join("skills/review/SKILL.md")).unwrap(),
        b"---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n"
    );
    assert_eq!(
        fs::read(fixture.home().join("CLAUDE.md")).unwrap(),
        b"Be kind.\n"
    );
    let servers: Value =
        serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        servers.pointer("/mcp/servers/notes").unwrap(),
        &json!({
            "command": "notes-server",
            "args": ["--fast"],
            "env": {"PORT": "8080"},
            "required": true
        })
    );
    assert_eq!(
        servers.pointer("/mcp/servers/remote").unwrap(),
        &json!({"url": "https://mcp.example.com/x"})
    );
    assert!(!reason_for(&report, "agents").is_empty());
    assert!(!reason_for(&report, "auth.json").is_empty());
    assert!(reason_for(&report, "auth.json").contains("credentials"));
    assert!(!reason_for(&report, "settings.json model").is_empty());
    let receipt: Value = serde_json::from_str(
        &fs::read_to_string(fixture.home().join("import-claude-code.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["source"], "claude-code");
    assert_eq!(receipt["imported_at"], 1_700_000_000);
    assert_eq!(receipt["imported"]["skills"], json!(["review"]));
}

#[test]
fn wrapped_commands_carry_a_synthesized_header() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    fixture.run(ImportSource::ClaudeCode).unwrap();
    // The name comes from the file stem, the description from the first
    // non-empty line, and model invocation is disabled.
    assert_eq!(
        fs::read(fixture.home().join("skills/greet/SKILL.md")).unwrap(),
        b"---\nname: greet\ndescription: Hello there.\ndisable-model-invocation: true\n---\n\n\nHello there.\n\nUsage: greet.\n"
    );
}

#[test]
fn a_command_without_text_has_no_description_to_wrap() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.source().join("commands")).unwrap();
    fs::write(fixture.source().join("commands/empty.md"), "\n  \n").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.commands.is_empty());
    assert!(!reason_for(&report, "commands/empty.md").is_empty());
    assert!(!fixture.home().join("skills/empty").exists());
}

#[test]
fn codex_import_lands_every_mapped_item() {
    let fixture = Fixture::new();
    codex_source(&fixture.source());
    let report = fixture.run(ImportSource::Codex).unwrap();
    assert_eq!(report.imported.skills, ["lint"]);
    assert_eq!(report.imported.commands, ["plan"]);
    assert_eq!(report.imported.instructions, ["AGENTS.md"]);
    assert_eq!(report.imported.mcp_servers, ["alpha", "beta"]);
    assert_eq!(
        fs::read(fixture.home().join("skills/plan/SKILL.md")).unwrap(),
        b"---\nname: plan\ndescription: Plan it.\ndisable-model-invocation: true\n---\n\nPlan it.\n"
    );
    let servers: Value =
        serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        servers.pointer("/mcp/servers/alpha").unwrap(),
        &json!({
            "command": "alpha-server",
            "args": ["--x", "y"],
            "env": {"PORT": "1"},
            "required": true
        })
    );
    assert!(!reason_for(&report, "config.toml [features]").is_empty());
    assert!(!reason_for(&report, "config.toml screen_reader_detection_done").is_empty());
    assert!(!reason_for(&report, "auth.json").is_empty());
    let receipt: Value = serde_json::from_str(
        &fs::read_to_string(fixture.home().join("import-codex.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["source"], "codex");
    assert_eq!(receipt["imported"]["mcp_servers"], json!(["alpha", "beta"]));
}

#[test]
fn missing_source_paths_are_nothing_to_import() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.source()).unwrap();
    fs::write(fixture.source().join("CLAUDE.md"), "Hi.\n").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
    assert!(report.imported.skills.is_empty());
    assert!(report.imported.commands.is_empty());
    assert!(report.imported.mcp_servers.is_empty());
    assert!(report.left_behind.is_empty());
}

#[test]
fn a_missing_source_home_imports_nothing() {
    let fixture = Fixture::new();
    let report = fixture.run(ImportSource::Codex).unwrap();
    assert_eq!(report, ImportReport::default());
    assert!(fixture.home().join("import-codex.json").exists());
}

#[test]
fn a_skill_entry_without_a_skill_file_is_left_behind() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.source().join("skills/bare")).unwrap();
    fs::write(fixture.source().join("skills/plain.md"), "x\n").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.skills.is_empty());
    assert_eq!(
        reason_for(&report, "skills/bare/SKILL.md"),
        "holds no SKILL.md"
    );
    assert_eq!(reason_for(&report, "skills/plain.md"), "is not a directory");
}

#[test]
fn a_non_command_file_is_left_behind() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.source().join("commands/nested")).unwrap();
    fs::write(fixture.source().join("commands/notes.txt"), "x\n").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.commands.is_empty());
    assert!(!reason_for(&report, "commands/nested").is_empty());
    assert!(!reason_for(&report, "commands/notes.txt").is_empty());
}

#[test]
fn colliding_names_take_a_suffix_and_occupied_instructions_stay() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    fs::create_dir_all(fixture.home().join("skills/review")).unwrap();
    fs::write(
        fixture.home().join("skills/review/SKILL.md"),
        "someone else's skill\n",
    )
    .unwrap();
    fs::write(
        fixture.home().join("config.json"),
        json!({"mcp": {"servers": {"notes": {"command": "other"}}}}).to_string(),
    )
    .unwrap();
    fs::write(fixture.home().join("CLAUDE.md"), "mine\n").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, ["review-2"]);
    assert!(report.imported.mcp_servers.contains(&"notes-2".to_owned()));
    assert!(report.imported.instructions.is_empty());
    assert_eq!(
        reason_for(&report, "CLAUDE.md"),
        "CLAUDE.md already exists in Fiber home"
    );
    assert_eq!(
        fs::read(fixture.home().join("skills/review-2/SKILL.md")).unwrap(),
        b"---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n"
    );
    // The occupied files stand as they were.
    assert_eq!(
        fs::read(fixture.home().join("skills/review/SKILL.md")).unwrap(),
        b"someone else's skill\n"
    );
    assert_eq!(
        fs::read(fixture.home().join("CLAUDE.md")).unwrap(),
        b"mine\n"
    );
}

#[test]
fn suffixes_count_past_two() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    for taken in ["review", "review-2"] {
        fs::create_dir_all(fixture.home().join(format!("skills/{taken}"))).unwrap();
        fs::write(
            fixture.home().join(format!("skills/{taken}/SKILL.md")),
            "other\n",
        )
        .unwrap();
    }
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, ["review-3"]);
}

#[test]
fn identical_bytes_are_skipped_without_error() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    fs::create_dir_all(fixture.home().join("skills/review")).unwrap();
    fs::write(
        fixture.home().join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
    )
    .unwrap();
    fs::write(fixture.home().join("CLAUDE.md"), "Be kind.\n").unwrap();
    fs::write(
        fixture.home().join("config.json"),
        json!({"mcp": {"servers": {
            "notes": {
                "command": "notes-server",
                "args": ["--fast"],
                "env": {"PORT": "8080"},
                "required": true
            },
            "remote": {"url": "https://mcp.example.com/x"}
        }}})
        .to_string(),
    )
    .unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, ["review"]);
    assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
    assert_eq!(report.imported.mcp_servers, ["notes", "remote"]);
    assert!(!fixture.home().join("skills/review-2").exists());
}

#[test]
fn a_completed_receipt_short_circuits_without_reading_the_source() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    let first = fixture.run(ImportSource::ClaudeCode).unwrap();
    fs::remove_dir_all(fixture.source()).unwrap();
    let second = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(first, second);
}

#[test]
fn a_receipt_for_another_source_runs_again() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    fixture.tmp.write(
        "home/import-claude-code.json",
        &json!({"source": "codex", "imported_at": 1}).to_string(),
    );
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, ["review"]);
}

#[test]
fn a_failed_run_writes_no_receipt_and_its_retry_completes() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    // The first skill file writes, the instruction write fails.
    fail_nth(2, Stage::BeforeRename);
    let failed = fixture.run(ImportSource::ClaudeCode);
    fail_at(None);
    assert!(failed.is_err());
    assert!(!fixture.home().join("import-claude-code.json").exists());
    assert!(fixture.home().join("skills/review/SKILL.md").exists());
    assert!(!fixture.home().join("CLAUDE.md").exists());
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.skills, ["review"]);
    assert_eq!(report.imported.instructions, ["CLAUDE.md"]);
    assert!(fixture.home().join("CLAUDE.md").exists());
    assert!(fixture.home().join("import-claude-code.json").exists());
}

#[test]
fn settings_json_that_does_not_parse_leaves_its_servers_behind() {
    let fixture = Fixture::new();
    claude_source(&fixture.source());
    fs::write(fixture.source().join("settings.json"), "{oops").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert!(report.imported.mcp_servers.is_empty());
    assert_eq!(
        reason_for(&report, "settings.json"),
        "does not parse as JSON"
    );
    // Everything else still lands, and the receipt records the run.
    assert_eq!(report.imported.skills, ["review"]);
    assert!(fixture.home().join("import-claude-code.json").exists());
}

#[test]
fn settings_json_shapes_outside_the_map_are_left_behind() {
    for (body, item) in [
        ("[]", "settings.json"),
        ("{\"mcpServers\": []}", "settings.json mcpServers"),
        ("{\"mcpServers\": {\"bad\": []}}", "mcpServers bad"),
    ] {
        let fixture = Fixture::new();
        fixture.write_source("settings.json", body);
        let report = fixture.run(ImportSource::ClaudeCode).unwrap();
        assert!(report.imported.mcp_servers.is_empty(), "{body}");
        assert!(!reason_for(&report, item).is_empty(), "{body}");
        assert!(fixture.home().join("import-claude-code.json").exists());
    }
}

#[test]
fn malformed_servers_are_refused_before_anything_is_written() {
    let servers = json!({
        "both": {"command": "a", "url": "https://x.example/"},
        "neither": {"args": []},
        "empty": {"command": ""},
        "numbered": {"command": 3},
        "urlled": {"url": 3},
        "usered": {"url": "https://user:pass@host.example/"},
        "listed": {"command": "a", "args": ["ok", 3]},
        "enved": {"command": "a", "env": "PORT=1"},
        "keyed": {"command": "a", "env": {"not a key": "1"}},
        "objected": {"command": "a", "env": {"PORT": {"secret": "mcp.a.PORT"}}},
        "tokened": {"command": "a", "env": {"API_TOKEN": "sk-canary-9"}},
        "keyed_word": {"command": "a", "env": {"KEY": "sk-canary-8"}},
        "monkeyed": {"command": "a", "env": {"MONKEY": "bananas"}},
        "required": {"command": "a", "required": "yes"},
        "required_false": {"command": "a", "required": false},
        "extra": {"command": "a", "startup_timeout_ms": 1},
        "good": {"command": "a"},
    });
    let fixture = Fixture::new();
    fixture.tmp.write(
        "source/settings.json",
        &json!({"mcpServers": servers}).to_string(),
    );
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(
        report.imported.mcp_servers,
        ["good", "monkeyed", "required_false"]
    );
    for item in [
        "server both",
        "server neither",
        "server empty",
        "server numbered",
        "server urlled",
        "server usered",
        "server listed",
        "server enved",
        "server keyed",
        "server objected",
        "server tokened",
        "server keyed_word",
        "server required",
        "server extra",
    ] {
        assert!(!reason_for(&report, item).is_empty(), "{item}");
    }
    // `MONKEY` holds no secret word, so it imports; `required: false`
    // reads as the default and is left out of the entry.
    assert!(reason_for(&report, "server monkeyed").is_empty());
    let written: Value =
        serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        written.pointer("/mcp/servers/required_false").unwrap(),
        &json!({"command": "a"})
    );
    // No refused value reaches Fiber's files.
    assert!(fixture.files_holding("sk-canary-9").is_empty());
    assert!(fixture.files_holding("sk-canary-8").is_empty());
    let tokened = reason_for(&report, "server tokened");
    assert!(tokened.contains("API_TOKEN"));
    assert!(!tokened.contains("sk-canary-9"));
}

#[test]
fn secret_words_match_whole_words_only() {
    assert!(is_secret_key("API_TOKEN"));
    assert!(is_secret_key("token"));
    assert!(is_secret_key("SSH_AUTH_SOCK"));
    assert!(is_secret_key("KEY"));
    assert!(!is_secret_key("MONKEY"));
    assert!(!is_secret_key("PORT"));
    assert!(!is_secret_key("TOKENIZER"));
}

#[test]
fn userinfo_is_an_at_sign_in_the_authority() {
    assert!(!url_has_userinfo("https://mcp.example.com/x"));
    assert!(!url_has_userinfo("https://mcp.example.com/a@b"));
    assert!(!url_has_userinfo("mcp.example.com/x"));
    assert!(url_has_userinfo("https://user@mcp.example.com/"));
    assert!(url_has_userinfo("https://user:pass@mcp.example.com/x"));
    assert!(url_has_userinfo("user@mcp.example.com/x"));
}

#[test]
fn codex_subset_refusals_leave_valid_servers_alone() {
    let fixture = Fixture::new();
    fixture.tmp.write(
        "source/config.toml",
        "top = 1\n\
         \n\
         [other]\n\
         x = 1\n\
         \n\
         [mcp_servers.good]\n\
         command = \"good-server\"\n\
         \n\
         [mcp_servers.multi]\n\
         command = \"open\n\
         continued\"\n\
         \n\
         [mcp_servers.dotted.key]\n\
         command = \"x\"\n\
         \n\
         [mcp_servers.table]\n\
         url = \"https://t.example/\"\n\
         extra = 1\n\
         \n\
         [mcp_servers.bare]\n\
         command = bareword\n\
         \n\
         [mcp_servers.\"sp ace\"]\n\
         command = \"x\"\n\
         \n\
         [mcp_servers.slashed/name]\n\
         command = \"x\"\n\
         \n\
         [broken\n",
    );
    let report = fixture.run(ImportSource::Codex).unwrap();
    assert_eq!(report.imported.mcp_servers, ["good"]);
    assert!(!reason_for(&report, "config.toml [other]").is_empty());
    assert!(!reason_for(&report, "config.toml top").is_empty());
    for item in [
        "config.toml [mcp_servers.multi]",
        "config.toml [mcp_servers.dotted.key]",
        "config.toml [mcp_servers.table]",
        "config.toml [mcp_servers.bare]",
        "server sp ace",
        "server slashed/name",
        "config.toml",
    ] {
        assert!(!reason_for(&report, item).is_empty(), "{item}");
    }
}

#[test]
fn codex_values_parse_quote_aware() {
    let fixture = Fixture::new();
    fixture.tmp.write(
        "source/config.toml",
        "[mcp_servers.hash]\n\
         command = \"h-server\" # a comment\n\
         args = [\"a#b\", 'c']\n\
         env = { FRAG = \"https://x.example/y#z\" }\n\
         \n\
         [mcp_servers.\"quoted.name\"]\n\
         url = \"https://q.example/\"\n",
    );
    let report = fixture.run(ImportSource::Codex).unwrap();
    assert_eq!(report.imported.mcp_servers, ["hash", "quoted.name"]);
    let written: Value =
        serde_json::from_str(&fs::read_to_string(fixture.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        written.pointer("/mcp/servers/hash").unwrap(),
        &json!({
            "command": "h-server",
            "args": ["a#b", "c"],
            "env": {"FRAG": "https://x.example/y#z"}
        })
    );
}

#[test]
fn codex_direct_table_keys_are_not_servers() {
    let fixture = Fixture::new();
    fixture.write_source("config.toml", "[mcp_servers]\norphan = 1\n");
    let report = fixture.run(ImportSource::Codex).unwrap();
    assert!(report.imported.mcp_servers.is_empty());
    assert!(!reason_for(&report, "config.toml [mcp_servers]").is_empty());
}

#[test]
fn secrets_never_reach_fiber_files_or_the_report_values() {
    let fixture = Fixture::new();
    fixture.tmp.write(
        "source/settings.json",
        &json!({"mcpServers": {
            "vault": {"command": "v", "env": {"VAULT_TOKEN": "sk-canary-7"}},
            "plain": {"command": "p"},
        }})
        .to_string(),
    );
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(report.imported.mcp_servers, ["plain"]);
    assert_eq!(
        reason_for(&report, "server vault"),
        "`env` holds a secret value for `VAULT_TOKEN`"
    );
    assert!(fixture.files_holding("sk-canary-7").is_empty());
    let debug = format!("{report:?}");
    assert!(!debug.contains("sk-canary-7"));
}

#[test]
fn an_instruction_that_changes_under_the_plan_aborts() {
    let fixture = Fixture::new();
    let planned = Planned {
        skills: Vec::new(),
        commands: Vec::new(),
        instructions: vec![PlannedFile {
            name: "CLAUDE.md".to_owned(),
            bytes: b"new\n".to_vec(),
        }],
        servers: Vec::new(),
        left_behind: Vec::new(),
    };
    fs::create_dir_all(fixture.home()).unwrap();
    fs::write(fixture.home().join("CLAUDE.md"), "changed\n").unwrap();
    let mut planned = planned;
    assert!(write_all(&fixture.home(), &mut planned).is_err());
    assert_eq!(
        fs::read(fixture.home().join("CLAUDE.md")).unwrap(),
        b"changed\n"
    );
}

#[test]
fn source_home_paths_resolve_under_the_persons_home() {
    assert_eq!(
        source_home(Path::new("/home/alice"), ImportSource::ClaudeCode),
        PathBuf::from("/home/alice/.claude")
    );
    assert_eq!(
        source_home(Path::new("/home/alice"), ImportSource::Codex),
        PathBuf::from("/home/alice/.codex")
    );
    assert_eq!(ImportSource::ClaudeCode.name(), "claude-code");
    assert_eq!(ImportSource::Codex.name(), "codex");
}

#[test]
fn top_level_entries_each_name_what_is_left_behind() {
    let fixture = Fixture::new();
    for name in [
        "agents",
        "hooks",
        "projects",
        "sessions",
        "todos",
        "permissions",
    ] {
        fs::create_dir_all(fixture.source().join(name)).unwrap();
    }
    fs::write(fixture.source().join("auth.json"), "{}\n").unwrap();
    fs::write(fixture.source().join("mystery"), "x\n").unwrap();
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    assert_eq!(
        reason_for(&report, "agents"),
        "agents are not configuration"
    );
    assert_eq!(reason_for(&report, "hooks"), "hooks are not imported");
    assert_eq!(
        reason_for(&report, "projects"),
        "sessions, history and todos are not imported"
    );
    assert_eq!(
        reason_for(&report, "sessions"),
        "sessions, history and todos are not imported"
    );
    assert_eq!(
        reason_for(&report, "todos"),
        "sessions, history and todos are not imported"
    );
    assert_eq!(
        reason_for(&report, "permissions"),
        "permissions are not imported"
    );
    assert_eq!(
        reason_for(&report, "auth.json"),
        "credentials are never imported"
    );
    assert_eq!(
        reason_for(&report, "mystery"),
        "is not configuration Fiber imports"
    );
}

#[test]
fn a_source_home_that_is_a_file_is_an_error() {
    let fixture = Fixture::new();
    fs::write(fixture.source(), "not a dir\n").unwrap();
    assert!(fixture.run(ImportSource::ClaudeCode).is_err());
}

#[test]
fn left_behind_names_never_hold_values() {
    let fixture = Fixture::new();
    fixture.tmp.write(
        "source/settings.json",
        &json!({"mcpServers": {
            "s": {"command": "c", "env": {"TOKEN": "sk-canary-3"}},
        }})
        .to_string(),
    );
    let report = fixture.run(ImportSource::ClaudeCode).unwrap();
    for left in &report.left_behind {
        assert!(!left.item.contains("sk-canary-3"), "{left:?}");
        assert!(!left.reason.contains("sk-canary-3"), "{left:?}");
    }
    assert!(fixture.files_holding("sk-canary-3").is_empty());
}
