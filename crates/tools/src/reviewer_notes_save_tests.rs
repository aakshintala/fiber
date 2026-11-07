//! The `reviewer_notes_save` tool: the model drafts lines, the person
//! confirms the staged diff and entries, and the save writes both layers.

use std::path::PathBuf;
use std::sync::Mutex;

use contract::events::{Answer, FileChange, Interaction};
use contract::shapes::True;
use contract::tool::{Answered, Ask, Asking, Output, Tool};
use serde_json::{Map, Value, json};

use super::ReviewerNotesSave;

struct Rig {
    _tmp: fakes::TempDir,
    home: PathBuf,
    tool: ReviewerNotesSave,
}

fn rig() -> Rig {
    let tmp = fakes::TempDir::new("fiber-reviewer-save");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let key = config::ProjectKey::new("proj").unwrap();
    let tool = ReviewerNotesSave::new(home.clone(), key);
    Rig {
        _tmp: tmp,
        home,
        tool,
    }
}

/// What one confirm carried; the tool raises only confirms.
#[derive(Debug)]
struct Recorded {
    prompt: String,
    suspends: bool,
}

struct FakeAsk {
    answerable: bool,
    answers: Mutex<Vec<Answered>>,
    asked: Mutex<Vec<Recorded>>,
    sabotage: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl FakeAsk {
    fn answering(answer: Answered) -> Self {
        Self {
            answerable: true,
            answers: Mutex::new(vec![answer]),
            asked: Mutex::new(Vec::new()),
            sabotage: Mutex::new(None),
        }
    }

    fn unanswerable() -> Self {
        Self {
            answerable: false,
            answers: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
            sabotage: Mutex::new(None),
        }
    }

    fn sabotaging(answer: Answered, sabotage: impl Fn() + Send + 'static) -> Self {
        Self {
            answerable: true,
            answers: Mutex::new(vec![answer]),
            asked: Mutex::new(Vec::new()),
            sabotage: Mutex::new(Some(Box::new(sabotage))),
        }
    }

    fn asked(&self) -> Vec<Recorded> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

impl Ask for FakeAsk {
    fn action(&self) -> contract::ActionId {
        contract::ActionId("a_1".to_owned())
    }

    fn answerable(&self) -> bool {
        self.answerable
    }

    fn ask(&self, asking: Asking) -> Answered {
        if let Some(sabotage) = self.sabotage.lock().unwrap().as_ref() {
            sabotage();
        }
        let Interaction::Confirm { prompt } = asking.interaction else {
            panic!("the tool raises only confirms");
        };
        self.asked.lock().unwrap().push(Recorded {
            prompt,
            suspends: asking.suspends,
        });
        self.answers.lock().unwrap().remove(0)
    }
}

fn line(topic: &str, layer: &str, text: &str) -> Value {
    json!({"topic": topic, "layer": layer, "text": text})
}

fn args(lines: Vec<Value>) -> Map<String, Value> {
    fakes::tool_args(json!({"lines": lines}))
}

fn run_asking(rig: &Rig, args: &Map<String, Value>, ask: &FakeAsk) -> Output {
    rig.tool.run_asking(
        args,
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
        ask,
    )
}

fn text_of(output: &Output) -> String {
    fakes::text_of(output)
}

fn global_file(rig: &Rig) -> PathBuf {
    rig.home.join("config.json")
}

fn project_file(rig: &Rig) -> PathBuf {
    rig.home.join("projects").join("proj").join("config.json")
}

fn read_file(path: &PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn yes() -> Answered {
    Answered::Reply(Answer::Confirmed { confirmed: true })
}

fn no() -> Answered {
    Answered::Reply(Answer::Confirmed { confirmed: false })
}

#[test]
fn yes_saves_both_layers_with_per_file_counts_and_changes() {
    let rig = rig();
    let arguments = args(vec![
        line("internal_hosts", "global", "corp.example"),
        line("source_control", "project", "github.com/acme/* are ours"),
        line(
            "read_only_commands",
            "project",
            "shell.read_only.\"acme-cli query\".flags = [\"--json\"]",
        ),
    ]);
    let ask = FakeAsk::answering(yes());
    let output = run_asking(&rig, &arguments, &ask);
    assert_eq!(output.error, None);
    // One confirm, never suspending, carrying the diff and the entry.
    let asked = ask.asked();
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].suspends, "the confirm never suspends");
    let prompt = &asked[0].prompt;
    for needle in [
        "Save these reviewer notes?",
        &global_file(&rig).display().to_string(),
        &project_file(&rig).display().to_string(),
        "### Internal hosts",
        "- corp.example",
        "### Source control",
        "- github.com/acme/* are ours",
        "+ shell.read_only.\"acme-cli query\".flags = [\"--json\"]",
    ] {
        assert!(
            prompt.contains(needle),
            "the confirm lacks {needle}\n{prompt}"
        );
    }
    // The report names each written file with its counts.
    let text = text_of(&output);
    assert!(
        text.contains(&format!(
            "{}: 2 added, 0 removed",
            global_file(&rig).display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "{}: 3 added, 0 removed",
            project_file(&rig).display()
        )),
        "{text}"
    );
    assert_eq!(
        output.changes,
        Some(vec![
            FileChange {
                path: global_file(&rig).display().to_string(),
                added: 2,
                removed: 0,
            },
            FileChange {
                path: project_file(&rig).display().to_string(),
                added: 3,
                removed: 0,
            },
        ])
    );
    // Both files hold the staged text and entry.
    let global = read_file(&global_file(&rig)).expect("global written");
    assert!(global.contains("### Internal hosts"));
    assert!(global.contains("- corp.example"));
    let project = read_file(&project_file(&rig)).expect("project written");
    assert!(project.contains("- github.com/acme/* are ours"));
    assert!(project.contains("\"acme-cli query\""));
}

#[test]
fn a_resent_topic_strikes_what_it_leaves_out_and_counts_the_removals() {
    let rig = rig();
    // Seed: two internal hosts, one source-control line.
    let seeded = run_asking(
        &rig,
        &args(vec![
            line("internal_hosts", "global", "old-one.example"),
            line("internal_hosts", "global", "old-two.example"),
            line("internal_hosts", "global", "kept.example"),
            line("source_control", "global", "github.com/acme/* are ours"),
        ]),
        &FakeAsk::answering(yes()),
    );
    assert_eq!(seeded.error, None);
    // Resend internal_hosts without the two old lines, with a new one:
    // the unsent source_control line stays, `kept` is neither struck nor
    // duplicated, and the two omitted lines are struck.
    let ask = FakeAsk::answering(yes());
    let output = run_asking(
        &rig,
        &args(vec![
            line("internal_hosts", "global", "kept.example"),
            line("internal_hosts", "global", "new.example"),
        ]),
        &ask,
    );
    assert_eq!(output.error, None);
    let prompt = ask.asked().remove(0).prompt;
    for needle in [
        "- - old-one.example",
        "- - old-two.example",
        "+ - new.example",
    ] {
        assert!(
            prompt.contains(needle),
            "the confirm lacks {needle}\n{prompt}"
        );
    }
    assert!(!prompt.contains("- - kept.example"), "{prompt}");
    assert!(!prompt.contains("- - github.com"), "{prompt}");
    let text = text_of(&output);
    assert!(
        text.contains(&format!(
            "{}: 1 added, 2 removed",
            global_file(&rig).display()
        )),
        "{text}"
    );
    let global = read_file(&global_file(&rig)).expect("global written");
    assert!(!global.contains("old-one.example"), "{global}");
    assert!(!global.contains("old-two.example"), "{global}");
    assert_eq!(global.matches("kept.example").count(), 1, "{global}");
    assert!(global.contains("new.example"), "{global}");
    assert!(global.contains("github.com/acme/* are ours"), "{global}");
}

#[test]
fn identical_lines_report_unchanged_without_asking_and_write_nothing() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let output = run_asking(&rig, &arguments, &FakeAsk::answering(yes()));
    assert_eq!(output.error, None);
    let global_before = read_file(&global_file(&rig));
    // The same draft again: nothing staged, so no confirm and no write.
    let ask = FakeAsk::answering(yes());
    let output = run_asking(&rig, &arguments, &ask);
    assert_eq!(text_of(&output), "unchanged");
    assert!(ask.asked().is_empty(), "no confirm when nothing changes");
    assert_eq!(read_file(&global_file(&rig)), global_before);
    assert_eq!(output.changes, None);
}

#[test]
fn empty_lines_report_unchanged_without_asking() {
    let rig = rig();
    let ask = FakeAsk::answering(yes());
    let output = run_asking(&rig, &args(Vec::new()), &ask);
    assert_eq!(text_of(&output), "unchanged");
    assert!(ask.asked().is_empty());
    assert!(read_file(&global_file(&rig)).is_none());
    assert!(read_file(&project_file(&rig)).is_none());
}

#[test]
fn no_reports_unchanged_and_writes_nothing() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let ask = FakeAsk::answering(no());
    let output = run_asking(&rig, &arguments, &ask);
    assert_eq!(text_of(&output), "unchanged");
    assert_eq!(ask.asked().len(), 1);
    assert!(read_file(&global_file(&rig)).is_none());
    assert_eq!(output.changes, None);
}

#[test]
fn declined_returns_declined_and_writes_nothing() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let ask = FakeAsk::answering(Answered::Reply(Answer::Declined { declined: True }));
    let output = run_asking(&rig, &arguments, &ask);
    assert_eq!(text_of(&output), "declined");
    assert!(read_file(&global_file(&rig)).is_none());
}

#[test]
fn an_answer_for_no_confirm_is_a_mismatch() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    for answer in [
        Answered::Reply(Answer::Labels {
            labels: vec!["yes".to_owned()],
        }),
        Answered::Reply(Answer::Text {
            text: "yes".to_owned(),
        }),
    ] {
        let output = run_asking(&rig, &arguments, &FakeAsk::answering(answer));
        let error = output.error.expect("a mismatch fails");
        assert_eq!(error.code, contract::ErrorCode::ToolError);
        assert!(read_file(&global_file(&rig)).is_none());
    }
}

#[test]
fn an_unparseable_read_only_line_fails_before_confirm() {
    let rig = rig();
    let arguments = args(vec![line("read_only_commands", "global", "not an entry")]);
    let ask = FakeAsk::answering(yes());
    let output = run_asking(&rig, &arguments, &ask);
    let error = output.error.expect("an unparseable line fails");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert!(
        error.message.contains("not an entry"),
        "the failure names the line: {}",
        error.message
    );
    assert!(ask.asked().is_empty(), "no confirm after a draft failure");
    assert!(read_file(&global_file(&rig)).is_none());
}

#[test]
fn a_changed_entry_fails_before_confirm() {
    let rig = rig();
    std::fs::write(
        global_file(&rig),
        r#"{"shell": {"read_only": {"acme-cli query": {"flags": ["--json"]}}}}"#,
    )
    .unwrap();
    let arguments = args(vec![line(
        "read_only_commands",
        "global",
        "shell.read_only.\"acme-cli query\".flags = [\"--yaml\"]",
    )]);
    let ask = FakeAsk::answering(yes());
    let output = run_asking(&rig, &arguments, &ask);
    let error = output.error.expect("a changed entry fails");
    assert_eq!(error.code, contract::ErrorCode::ConfigInvalid);
    assert!(
        error.message.contains("acme-cli query"),
        "the failure names the entry: {}",
        error.message
    );
    assert!(ask.asked().is_empty(), "no confirm after a draft failure");
}

#[test]
fn no_answer_is_a_refusal_that_writes_nothing() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let output = run_asking(&rig, &arguments, &FakeAsk::answering(Answered::NoAnswer));
    let error = output.error.expect("no answer refuses");
    assert!(
        error.message.contains("fiber ask"),
        "the refusal names fiber ask: {}",
        error.message
    );
    assert!(read_file(&global_file(&rig)).is_none());
}

#[test]
fn a_program_driven_session_refuses_naming_fiber_ask() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let output = run_asking(&rig, &arguments, &FakeAsk::unanswerable());
    let error = output.error.expect("a program-driven session refuses");
    assert!(
        error.message.contains("fiber ask"),
        "the refusal names fiber ask: {}",
        error.message
    );
    assert!(read_file(&global_file(&rig)).is_none());
    // The plain run refuses the same way.
    let output = rig.tool.run(
        &arguments,
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(
        text_of(&output).contains("fiber ask"),
        "run names fiber ask: {}",
        text_of(&output)
    );
}

#[test]
fn a_stale_save_reports_stale_and_writes_nothing() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let home = rig.home.clone();
    let ask = FakeAsk::sabotaging(yes(), move || {
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.json"),
            r#"{"reviewer": {"context": "someone else wrote first"}}"#,
        )
        .unwrap();
    });
    let output = run_asking(&rig, &arguments, &ask);
    assert_eq!(output.error, None);
    assert!(
        text_of(&output).contains("nothing was written"),
        "a stale save says so: {}",
        text_of(&output)
    );
    assert!(
        read_file(&global_file(&rig))
            .expect("the concurrent write stands")
            .contains("someone else wrote first")
    );
    assert_eq!(output.changes, None);
}

#[test]
fn an_io_failure_in_the_save_is_a_tool_error() {
    let rig = rig();
    let arguments = args(vec![line("internal_hosts", "global", "corp.example")]);
    let home = rig.home.clone();
    let ask = FakeAsk::sabotaging(yes(), move || {
        // Nothing is written before the confirm, so the file is absent.
        std::fs::create_dir_all(home.join("config.json")).unwrap();
    });
    let output = run_asking(&rig, &arguments, &ask);
    let error = output.error.expect("an io failure fails");
    assert_eq!(error.code, contract::ErrorCode::IoFailed);
}

#[test]
fn lines_that_do_not_parse_fail() {
    let rig = rig();
    for lines in [
        vec![line("no_such_topic", "global", "x")],
        vec![line("internal_hosts", "everywhere", "x")],
        vec![json!({"topic": "internal_hosts", "layer": "global"})],
    ] {
        let output = run_asking(&rig, &args(lines), &FakeAsk::answering(yes()));
        let error = output.error.expect("bad lines fail");
        assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    }
    let output = run_asking(
        &rig,
        &fakes::tool_args(json!({})),
        &FakeAsk::answering(yes()),
    );
    assert!(output.error.is_some(), "lines are required");
}

#[test]
fn the_definition_names_the_tool_and_fits_the_budgets() {
    let definition = rig().tool.definition();
    assert_eq!(definition.name, "reviewer_notes_save");
    // The topic enum is the Topic serde names.
    let schema = serde_json::to_value(&definition.input_schema).expect("schema serializes");
    let topic = schema
        .pointer("/properties/lines/items/properties/topic/enum")
        .expect("a topic enum");
    let names: Vec<String> = contract::notes::Topic::ALL
        .into_iter()
        .map(|topic| serde_json::to_value(topic).expect("a topic serializes"))
        .map(|name| name.as_str().expect("a string").to_owned())
        .collect();
    assert_eq!(
        topic,
        &serde_json::to_value(names).expect("names serialize")
    );
    // Only the strict keywords: the definition fits the strict shape.
    strict_schema(&schema, true);
    let bytes = serde_json::to_string(&definition).expect("definition serializes");
    assert!(bytes.len() <= 1200, "definition is {} bytes", bytes.len());
}

/// Every keyword the definition uses is a strict one.
const STRICT_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

fn strict_schema(schema: &Value, root: bool) {
    let node = schema.as_object().expect("an object");
    for (keyword, value) in node {
        assert!(
            STRICT_KEYWORDS.contains(&keyword.as_str()),
            "the schema uses only strict keywords: {keyword}"
        );
        if keyword == "properties" {
            for (_, property) in value.as_object().expect("properties") {
                strict_schema(property, false);
            }
        }
        if keyword == "items" {
            strict_schema(value, false);
        }
    }
    if root {
        assert_eq!(node.get("type"), Some(&json!("object")));
    }
}

#[test]
fn the_tool_declares_no_effects() {
    let effects = rig().tool.effects(&Map::new()).expect("effects");
    assert!(effects.declared.effects.is_empty());
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
}
