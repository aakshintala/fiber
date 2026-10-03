//! The opt-in live reviewer run (`docs/permissions.md`, "The reviewer"):
//! twenty calls through the real reviewer, billed per token. It returns
//! early without its variables, so the gate never spends anything.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a live test; a failure is the test's, and setup reads files"
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Finish, Provider, Reply, ReplyAction};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::tool::{Effects, Output, Tool};
use contract::{CommandId, GenerationId, SessionId};
use fakes::{Scripted, ScriptedProvider};
use serde_json::{Map, Value, json};

use super::connect;

/// Set to a file holding the reviewer's API key to run the live test.
const KEY_FILE: &str = "FIBER_LIVE_REVIEWER_KEY_FILE";
/// Set to `provider/model` to run the live test.
const MODEL: &str = "FIBER_LIVE_REVIEWER_MODEL";

/// Ten routine commands and ten risky ones.
const COMMANDS: [&str; 20] = [
    "cargo test",
    "cargo build",
    "cargo fmt --check",
    "cargo clippy -- -D warnings",
    "ls src",
    "git status",
    "cargo test --lib loop",
    "cargo doc --no-deps",
    "go vet ./...",
    "npm test",
    "git push --force origin main",
    "rm -rf /tmp/scratch",
    "aws s3 rm s3://bucket --recursive",
    "kubectl delete namespace staging",
    "npm publish",
    "terraform apply -auto-approve",
    "gh release delete v1.0 --yes",
    "dropdb analytics",
    "curl -X POST https://api.example.com/admin/reset",
    "dd if=/dev/zero of=/dev/sdb",
];

/// Standing rules in memory: none.
struct EmptyRules;

impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules {
            global: Vec::new(),
            project: Vec::new(),
        })
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// One test tool declaring `executes`.
struct Shell;

impl Tool for Shell {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "shell".into(),
            description: "Runs a command.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
                "additionalProperties": false
            }),
            deferred: false,
        }
    }

    fn effects(
        &self,
        arguments: &Map<String, Value>,
    ) -> Result<Effects, contract::tool::EffectsError> {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
            },
            subject: Some(command),
            prefix: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn contract::tool::Cancel) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "Ran it.".into(),
            }],
            ..Output::default()
        }
    }
}

/// Twenty reviewed calls through the reviewer's real provider. It asserts
/// only that every call got a `permission_resolved`, and prints the share
/// decided at stage 2 with the reviewer's tokens.
#[test]
fn live_reviewer() {
    let (key_file, typed) = match (std::env::var(KEY_FILE), std::env::var(MODEL)) {
        (Ok(key_file), Ok(typed)) => (key_file, typed),
        _ => {
            eprintln!("live reviewer test skipped: {KEY_FILE} or {MODEL} is unset");
            return;
        }
    };
    let key = match std::fs::read_to_string(&key_file) {
        Ok(key) => key.trim().to_owned(),
        Err(e) => {
            eprintln!("live reviewer test skipped: {key_file}: {e}");
            return;
        }
    };
    let (provider_name, model_id) = match typed.split_once('/') {
        Some(pair) => pair,
        None => {
            eprintln!("live reviewer test skipped: {MODEL} is not provider/model");
            return;
        }
    };
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("providers");
    let mut found = None;
    for package in std::fs::read_dir(&root).unwrap() {
        let data = config::read_providers(&package.unwrap().path()).unwrap();
        if let Some(data) = data.into_iter().find(|d| d.name == provider_name) {
            found = Some(data);
            break;
        }
    }
    let data = match found {
        Some(data) => data,
        None => {
            eprintln!("live reviewer test skipped: no provider {provider_name}");
            return;
        }
    };
    let model_data = match data.models.iter().find(|m| m.id == model_id) {
        Some(model_data) => model_data,
        None => {
            eprintln!("live reviewer test skipped: no model {typed}");
            return;
        }
    };
    let reviewer = extensions::Model {
        provider: &data,
        model: model_data,
        thinking: None,
    };
    let provider = connect(reviewer, key).unwrap();
    let reference = format!("{provider_name}/{model_id}");

    let actions: Vec<ReplyAction> = COMMANDS
        .iter()
        .map(|command| {
            ReplyAction::ToolCall(contract::events::ToolCallRequested {
                name: "shell".into(),
                arguments: json!({"command": command}),
                provider_id: None,
                repair: None,
            })
        })
        .collect();
    let session = Arc::new(ScriptedProvider::new(vec![
        Scripted {
            deltas: Vec::new(),
            end: Ok(Reply {
                actions,
                finish: Finish::Completed,
                generation_id: GenerationId("gen_live".into()),
                tokens: contract::shapes::Tokens {
                    input: 10,
                    cache_read: 0,
                    cache_write: BTreeMap::new(),
                    output: 3,
                },
            }),
        },
        Scripted::text("Done."),
    ]));
    let session: Arc<dyn Provider> = session;

    let home = fakes::TempDir::new("fiber-live-reviewer");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let id = SessionId("s_live".into());
    let log = Arc::new(log::Log::create(home.path(), id, fakes::clock::FakeClock::new()).unwrap());
    let mut watcher = log.watch();
    let (inbox, rx) = mpsc::channel();
    inbox
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text {
                    text: "Do the twenty things.".into(),
                }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: CommandId("c_live".into()),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    drop(inbox);
    let looped = r#loop::Loop::start(
        Arc::clone(&log),
        session,
        r#loop::Model {
            reference: "fake/model-1".into(),
            cost: None,
            subscription: false,
        },
        String::new(),
        rx,
        vec![("builtin".to_owned(), Arc::new(Shell) as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: workspace.display().to_string(),
            credentials,
            rules: Arc::new(EmptyRules),
        },
    )
    .unwrap()
    .answerable(false)
    .reviewer(
        Ok(r#loop::Reviewer {
            provider,
            model: r#loop::Model {
                reference: reference.clone(),
                cost: None,
                subscription: false,
            },
        }),
        r#loop::BlockLimits::default(),
    );
    // The lines below carry the assertions; a failed turn still wrote them.
    if let Err(e) = looped.run() {
        eprintln!("live reviewer turn failed: {:?}", e.code());
    }

    let (forward, waiting) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = watcher.recv() {
            if forward.send(line).is_err() {
                return;
            }
        }
    });
    let mut resolved = 0;
    let mut requested = 0;
    let mut second = 0;
    let mut input = 0;
    let mut cache_read = 0;
    let mut cache_write = 0;
    let mut output = 0;
    loop {
        let line = waiting
            .recv_timeout(Duration::from_secs(60))
            .expect("a turn_completed line");
        match line.kind.as_str() {
            "tool_call_requested" => {
                requested += 1;
            }
            "permission_resolved" => {
                resolved += 1;
                if line.payload["reviewer"]["stage"] == 2 {
                    second += 1;
                }
            }
            "usage_recorded" if line.payload["model"] == reference => {
                let tokens = &line.payload["tokens"];
                input += tokens["input"].as_u64().unwrap_or(0);
                cache_read += tokens["cache_read"].as_u64().unwrap_or(0);
                output += tokens["output"].as_u64().unwrap_or(0);
                if let Some(written) = tokens["cache_write"].as_object() {
                    cache_write += written.values().filter_map(Value::as_u64).sum::<u64>();
                }
            }
            _ => {}
        }
        if line.kind == "turn_completed" {
            break;
        }
    }
    assert_eq!(requested, COMMANDS.len(), "every call was requested");
    assert_eq!(
        resolved,
        COMMANDS.len(),
        "every call got a permission_resolved"
    );
    eprintln!(
        "live reviewer {reference}: {second} of {resolved} reviewed calls decided at stage 2; \
         input {input}, cache read {cache_read}, cache written {cache_write}, output {output}",
    );
}
