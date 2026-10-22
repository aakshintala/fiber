//! Tests for `prompt::section` and `prompt::fill`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::PathBuf;
use std::sync::Arc;

use super::{PromptInputs, fill, message, oversize, section, system_prompt};

/// Every optional input absent, for system prompt tests: the opening
/// message fields never reach `system_prompt`.
fn empty_inputs() -> PromptInputs {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    PromptInputs::new(
        PathBuf::from("/test-home"),
        "/bin/sh".into(),
        "/test-home/s_test/events.jsonl".into(),
        clock,
        fakes::CONTEXT_WINDOW,
    )
}

fn full_inputs() -> PromptInputs {
    let mut inputs = empty_inputs();
    inputs.system = Some("Custom system.".into());
    inputs.append = Some("Appendix.".into());
    inputs.addendum = Some("Addendum.".into());
    inputs.extensions = vec![
        ("zeta".into(), "Z text.".into()),
        ("alpha".into(), "A text.".into()),
    ];
    inputs
}

fn tools() -> Vec<(String, Option<String>, bool)> {
    vec![
        ("write".into(), Some("Write guide.".into()), false),
        ("read".into(), Some("Read guide.".into()), false),
    ]
}

#[test]
fn system_prompt_orders_all_five_parts() {
    let prompt = system_prompt(&full_inputs(), "fake/model-1", true, &tools());
    let custom = prompt.find("Custom system.").unwrap();
    let tools_at = prompt.find("# Tools").unwrap();
    let session = prompt.find("# Session").unwrap();
    let alpha = prompt.find("alpha").unwrap();
    let appendix = prompt.find("Appendix.").unwrap();
    assert!(custom < tools_at && tools_at < session && session < alpha && alpha < appendix);
    assert!(prompt.contains("Nobody is present"));
    assert!(prompt.contains("Addendum."));
    // Tools and extensions in name order regardless of input order.
    assert!(prompt.find("### read").unwrap() < prompt.find("### write").unwrap());
    assert!(alpha < prompt.find("zeta").unwrap());
}

#[test]
fn system_md_replaces_fibers_text_and_keeps_the_rest() {
    let prompt = system_prompt(&full_inputs(), "m", false, &[]);
    assert!(prompt.contains("Custom system."));
    assert!(!prompt.contains("operating inside Fiber"));
    assert!(prompt.contains("# Session"));
}

#[test]
fn default_inputs_use_fibers_text() {
    let prompt = system_prompt(&empty_inputs(), "m", false, &[]);
    assert!(prompt.contains("operating inside Fiber"));
}

#[test]
fn deferred_tools_leave_their_guidelines_out() {
    let tools = vec![
        ("read".into(), Some("Read guide.".into()), true),
        ("write".into(), Some("Write guide.".into()), false),
    ];
    let prompt = system_prompt(&empty_inputs(), "m", false, &tools);
    assert!(!prompt.contains("Read guide."));
    assert!(prompt.contains("Write guide."));
}

#[test]
fn unattended_line_only_when_unattended() {
    let plain = system_prompt(&empty_inputs(), "m", false, &[]);
    assert!(!plain.contains("Nobody is present"));
    assert!(!plain.contains("`ask_user`"));
    let away = system_prompt(&empty_inputs(), "m", true, &[]);
    assert!(away.contains("Nobody is present"));
    assert!(away.contains("Ask with `ask_user` only when you cannot go on"));
}

#[test]
fn empty_parts_leave_no_blank_line() {
    let prompt = system_prompt(&empty_inputs(), "m", false, &[]);
    assert!(!prompt.contains("\n\n\n"));
    assert!(!prompt.contains("# Tools"));
    assert!(!prompt.contains("Appendix"));
    // Whitespace-only files count as absent.
    let mut inputs = empty_inputs();
    inputs.system = Some("   \n".into());
    inputs.append = Some("\n".into());
    inputs.addendum = Some("  ".into());
    inputs.extensions = vec![("e".into(), "  ".into())];
    let prompt = system_prompt(&inputs, "m", false, &[]);
    assert!(prompt.contains("operating inside Fiber"));
    assert!(!prompt.contains("# Tools"));
}

#[test]
fn same_inputs_give_equal_strings_twice() {
    let inputs = full_inputs();
    let tools = tools();
    assert_eq!(
        system_prompt(&inputs, "m", true, &tools),
        system_prompt(&inputs, "m", true, &tools)
    );
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

fn pinned(bytes: &[u8], len: usize, hash: u64) {
    assert_eq!(
        bytes.len(),
        len,
        "length changed: edit is a reviewed change"
    );
    assert_eq!(
        fnv1a(bytes),
        hash,
        "bytes changed: edit is a reviewed change"
    );
}

#[test]
fn system_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/system.md"),
        1219,
        0xd3272595a6580bfd,
    );
}

#[test]
fn opening_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/opening.md"),
        424,
        0xf50a9f974a4b1e60,
    );
}

#[test]
fn messages_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/messages.md"),
        6050,
        0xb00c6940125d8a76,
    );
}

#[test]
fn reviewer_md_bytes_are_pinned() {
    pinned(
        include_bytes!("../prompt/reviewer.md"),
        8563,
        0xe84e383df7459969,
    );
}

#[test]
fn messages_md_holds_every_section_the_doc_names() {
    for name in [
        "tools",
        "tool",
        "session",
        "unattended",
        "instruction-file",
        "no-instruction-files",
        "subdirectory-file",
        "created-file",
        "created-section-file",
        "replaced-file",
        "diff-file",
        "deleted-file",
        "date",
        "extension",
        "nudge",
        "handoff-note",
        "handoff-focus",
        "handoff-jobs",
        "job-line",
        "job-line-suppressed",
        "session-message",
        "session-message-unnamed",
        "delegate-finished",
        "delegate-questions",
        "fork-task",
        "extension-section",
        "extension-file",
        "budget-line",
        "rewind-note",
        "rewind-summary",
        "rewind-summary-point",
        "rewind-written",
        "rewind-ran",
        "rewind-unchanged",
        "rewind-jobs",
        "skill-added",
        "skill-removed",
        "quota-notice",
        "quota-notice-reset",
        "search-cancelled",
    ] {
        assert!(
            !message(name).is_empty(),
            "messages.md has no ## {name} section"
        );
    }
}

#[test]
fn an_unknown_message_is_empty() {
    assert_eq!(message("no-such-section"), "");
}

#[test]
fn a_cancelled_search_reads_its_result_line() {
    assert_eq!(
        message("search-cancelled"),
        "The search was cancelled; no tool was loaded."
    );
}

#[test]
fn oversize_passes_exactly_ten_percent_and_marks_a_byte_more() {
    assert_eq!(oversize(400, 1000, vec![("a", 300), ("b", 200)]), None);
    assert_eq!(
        oversize(401, 1000, vec![("a", 300), ("b", 200)]),
        Some("a (300 bytes), b (200 bytes)".into())
    );
}

#[test]
fn oversize_marks_a_total_too_big_to_multiply() {
    assert_eq!(
        oversize(u128::MAX, 1, Vec::<(&str, u128)>::new()),
        Some(String::new())
    );
}

#[test]
fn fill_replaces_known_and_leaves_unknown_alone() {
    assert_eq!(
        fill("hello {name}, {x}!", &[("name", "world")]),
        "hello world, {x}!"
    );
}

#[test]
fn fill_does_not_rescan_inserted_text() {
    assert_eq!(
        fill("at {path}", &[("path", "/tmp/{date}"), ("date", "today")]),
        "at /tmp/{date}"
    );
}

#[test]
fn fill_names_may_hold_dashes_underscores_slashes_and_digits() {
    assert_eq!(fill("{a-b}", &[("a-b", "X")]), "X");
    assert_eq!(fill("{a_b}", &[("a_b", "X")]), "X");
    assert_eq!(fill("{a/b}", &[("a/b", "X")]), "X");
    assert_eq!(fill("{a1}", &[("a1", "X")]), "X");
    assert_eq!(fill("{a-b_c/d1}", &[("a-b_c/d1", "X")]), "X");
}

#[test]
fn fill_leaves_unclosed_and_empty_braces_alone() {
    assert_eq!(fill("a {oops", &[]), "a {oops");
    assert_eq!(fill("a {", &[]), "a {");
    assert_eq!(fill("a {}", &[]), "a {}");
    assert_eq!(fill("{} {name}", &[("name", "X")]), "{} X");
}

#[test]
fn fill_leaves_a_brace_inside_a_name_alone() {
    assert_eq!(fill("{a{b}", &[]), "{a{b}");
    assert_eq!(fill("{a{b}", &[("b", "B")]), "{aB");
}

#[test]
fn fill_keeps_multibyte_text_around_placeholders() {
    assert_eq!(
        fill("héllo {name} wörld ", &[("name", "X")]),
        "héllo X wörld "
    );
    assert_eq!(fill("日{name}本", &[("name", "X")]), "日X本");
}

#[test]
fn fill_value_holding_placeholders_is_not_rescanned() {
    assert_eq!(
        fill("{a} {date}", &[("a", "{date}"), ("date", "today")]),
        "{date} today"
    );
}

#[test]
fn section_strips_blank_lines_at_its_ends() {
    let md = "## a\n\nbody\n\n\n## b\nother\n";
    assert_eq!(section(md, "a"), "## a\n\nbody");
    assert_eq!(section(md, "b"), "## b\nother");
}

#[test]
fn missing_section_is_empty() {
    assert_eq!(section("## a\nbody\n", "b"), "");
}

fn sent(name: &str, by: &str, deferred: bool, pad: usize) -> contract::events::SentTool {
    let mut definition = serde_json::Map::new();
    definition.insert("pad".into(), "x".repeat(pad).into());
    contract::events::SentTool {
        name: name.into(),
        registered_by: by.into(),
        deferred,
        definition,
    }
}

fn built(window: u64, tools: Vec<contract::events::SentTool>) -> contract::events::PreambleBuilt {
    contract::events::PreambleBuilt {
        reason: contract::events::PreambleReason::Start,
        model: "m".into(),
        context_window: window,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        credential: None,
        system_prompt: String::new(),
        tools,
        replaced: Vec::new(),
    }
}

#[test]
fn definitions_notice_names_the_three_largest_sources_by_size() {
    // Each definition is its pad plus 10 bytes of JSON: {"pad":"..."}.
    let tools = vec![
        sent("a", "small", false, 10),
        sent("b", "big", false, 300),
        sent("c", "mid", false, 100),
        sent("d", "smaller", false, 20),
    ];
    let notice = super::definitions_notice(&built(100, tools)).unwrap();
    let text = notice.message;
    let big = text.find("big (310 bytes)").unwrap();
    let mid = text.find("mid (110 bytes)").unwrap();
    let smaller = text.find("smaller (30 bytes)").unwrap();
    assert!(big < mid && mid < smaller, "{text}");
    assert!(!text.contains("small ("), "{text}");
}

#[test]
fn definitions_notice_ignores_deferred_tools() {
    let deferred = vec![sent("a", "mcp__s", true, 10_000)];
    assert!(super::definitions_notice(&built(100, deferred)).is_none());
}

/// A test tool with a fixed definition and guidelines, for build tests.
struct BuildTool {
    definition: contract::provider::ToolDefinition,
    guidelines: Option<String>,
}

impl contract::tool::Tool for BuildTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        self.definition.clone()
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: contract::shapes::DeclaredEffects {
                effects: vec![contract::shapes::Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        contract::tool::Output::default()
    }

    fn guidelines(&self) -> Option<String> {
        self.guidelines.clone()
    }
}

fn tool_definition(name: &str, deferred: bool) -> contract::provider::ToolDefinition {
    contract::provider::ToolDefinition {
        name: name.into(),
        description: format!("The test tool {name}."),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
            "additionalProperties": false
        }),
        deferred,
        hosted: None,
    }
}

fn registered(
    name: &str,
    by: &str,
    deferred: bool,
    guidelines: Option<&str>,
) -> (
    String,
    std::sync::Arc<dyn contract::tool::Tool>,
    contract::provider::ToolDefinition,
) {
    let tool = std::sync::Arc::new(BuildTool {
        definition: tool_definition(name, deferred),
        guidelines: guidelines.map(str::to_owned),
    });
    (by.into(), tool, tool_definition(name, deferred))
}

/// A provider answering `defers`, never called by `build`.
struct DefersProvider {
    defers: bool,
}

struct NoCall;

impl contract::provider::ModelCall for NoCall {
    fn run(
        &self,
        _: &mut dyn FnMut(contract::provider::Delta),
    ) -> Result<contract::provider::Reply, contract::provider::CallError> {
        Err(contract::provider::CallError::Cancelled {
            usage: Box::new(contract::provider::CallUsage::unnamed(
                contract::provider::InputSize {
                    bytes: 0,
                    media: false,
                },
            )),
        })
    }

    fn cancel(&self) {}
}

impl contract::provider::Provider for DefersProvider {
    fn call(&self, _: &contract::provider::ModelRequest) -> Box<dyn contract::provider::ModelCall> {
        Box::new(NoCall)
    }

    fn wire_tools(
        &self,
        tools: &[contract::provider::ToolDefinition],
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        fakes::wire_tools(tools)
    }

    fn defers(&self) -> bool {
        self.defers
    }
}

/// A factory building the search tool `tool_search`, recording its inputs.
#[allow(
    clippy::type_complexity,
    reason = "the test observes the factory's catalogue and sources"
)]
fn search_factory(
    seen: &std::sync::Arc<
        std::sync::Mutex<Vec<(Vec<contract::provider::ToolDefinition>, Vec<String>)>>,
    >,
) -> crate::ToolSearchFactory {
    let seen = std::sync::Arc::clone(seen);
    std::sync::Arc::new(
        move |catalogue: &[contract::provider::ToolDefinition], sources: &[String]| {
            seen.lock()
                .unwrap()
                .push((catalogue.to_vec(), sources.to_vec()));
            std::sync::Arc::new(BuildTool {
                definition: contract::provider::ToolDefinition {
                    name: "tool_search".into(),
                    description: "Finds deferred tools.".into(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "minLength": 1},
                            "limit": {"type": "integer", "minimum": 1, "default": 8}
                        },
                        "required": ["query"],
                        "additionalProperties": false
                    }),
                    deferred: false,
                    hosted: None,
                },
                guidelines: Some("Search guide.".into()),
            }) as std::sync::Arc<dyn contract::tool::Tool>
        },
    )
}

fn build_inputs() -> PromptInputs {
    empty_inputs()
}

#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "test helper wires one build; the tuple is the build's"
)]
fn run_build(
    inputs: &PromptInputs,
    tools: &std::collections::BTreeMap<
        String,
        (
            String,
            std::sync::Arc<dyn contract::tool::Tool>,
            contract::provider::ToolDefinition,
        ),
    >,
    defers: bool,
    factory: Option<&crate::ToolSearchFactory>,
    replaced: Vec<contract::events::ToolReplaced>,
) -> (
    String,
    Vec<contract::provider::ToolDefinition>,
    contract::events::PreambleBuilt,
    Option<(
        std::sync::Arc<dyn contract::tool::Tool>,
        contract::provider::ToolDefinition,
    )>,
) {
    super::build(
        inputs,
        "m",
        false,
        tools,
        &DefersProvider { defers },
        contract::events::PreambleReason::Start,
        replaced,
        None,
        factory,
    )
}

fn tools_map(
    entries: Vec<(
        String,
        std::sync::Arc<dyn contract::tool::Tool>,
        contract::provider::ToolDefinition,
    )>,
) -> std::collections::BTreeMap<
    String,
    (
        String,
        std::sync::Arc<dyn contract::tool::Tool>,
        contract::provider::ToolDefinition,
    ),
> {
    entries
        .into_iter()
        .map(|(by, tool, definition)| (definition.name.clone(), (by, tool, definition)))
        .collect()
}

fn sent_deferred(event: &contract::events::PreambleBuilt, name: &str) -> bool {
    event
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap()
        .deferred
}

fn sent_names(event: &contract::events::PreambleBuilt) -> Vec<String> {
    event.tools.iter().map(|tool| tool.name.clone()).collect()
}

#[test]
fn build_with_factory_defers_a_deferred_tool_and_adds_the_search() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let tools = tools_map(vec![
        registered("alpha", "fx", true, Some("Alpha guide.")),
        registered("beta", "builtin", false, Some("Beta guide.")),
    ]);
    let (system, definitions, event, built) =
        run_build(&build_inputs(), &tools, true, Some(&factory), Vec::new());
    assert!(sent_deferred(&event, "alpha"));
    assert!(!sent_deferred(&event, "beta"));
    let (tool, definition) = built.expect("a search is due");
    assert_eq!(definition.name, "tool_search");
    assert!(!definition.deferred);
    assert_eq!(tool.definition().name, "tool_search");
    assert_eq!(sent_names(&event), ["alpha", "beta", "tool_search"]);
    let search = event
        .tools
        .iter()
        .find(|tool| tool.name == "tool_search")
        .unwrap();
    assert!(!search.deferred);
    assert_eq!(search.registered_by, "builtin");
    assert!(
        definitions
            .iter()
            .any(|definition| definition.name == "tool_search")
    );
    assert!(!system.contains("Alpha guide."));
    assert!(system.contains("Beta guide."));
    assert!(system.contains("Search guide."));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0]
            .0
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
    assert_eq!(seen[0].1, ["fx"]);
}

#[test]
fn build_merges_the_search_ahead_of_later_registered_names() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let tools = tools_map(vec![
        registered("alpha", "fx", true, Some("Alpha guide.")),
        registered("zebra", "fy", false, Some("Zebra guide.")),
    ]);
    let (_, definitions, event, built) =
        run_build(&build_inputs(), &tools, true, Some(&factory), Vec::new());
    built.expect("a search is due");
    assert_eq!(sent_names(&event), ["alpha", "tool_search", "zebra"]);
    assert_eq!(
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>(),
        ["alpha", "tool_search", "zebra"]
    );
}

#[test]
fn build_without_a_factory_defers_nothing() {
    let tools = tools_map(vec![registered("alpha", "fx", true, Some("Alpha guide."))]);
    let (system, definitions, event, built) =
        run_build(&build_inputs(), &tools, true, None, Vec::new());
    assert!(built.is_none());
    assert!(!sent_deferred(&event, "alpha"));
    assert!(!sent_names(&event).contains(&"tool_search".to_owned()));
    assert!(
        !definitions
            .iter()
            .any(|definition| definition.name == "tool_search")
    );
    assert!(system.contains("Alpha guide."));
}

#[test]
fn build_on_a_model_without_deferral_defers_nothing() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let tools = tools_map(vec![registered("alpha", "fx", true, Some("Alpha guide."))]);
    let (system, definitions, event, built) =
        run_build(&build_inputs(), &tools, false, Some(&factory), Vec::new());
    assert!(built.is_none());
    assert!(!sent_deferred(&event, "alpha"));
    assert!(!sent_names(&event).contains(&"tool_search".to_owned()));
    assert!(
        !definitions
            .iter()
            .any(|definition| definition.name == "tool_search")
    );
    assert!(system.contains("Alpha guide."));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn build_with_no_deferred_tool_builds_no_search() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let tools = tools_map(vec![registered("beta", "builtin", false, None)]);
    let (_, _, event, built) = run_build(&build_inputs(), &tools, true, Some(&factory), Vec::new());
    assert!(built.is_none());
    assert!(!sent_names(&event).contains(&"tool_search".to_owned()));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn build_per_tool_key_overrides_the_definition() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let mut inputs = build_inputs();
    inputs.deferrals = crate::prompt::Deferrals {
        tools: [("x".to_owned(), false), ("y".to_owned(), true)]
            .into_iter()
            .collect(),
        full_servers: std::collections::BTreeSet::new(),
    };
    let tools = tools_map(vec![
        registered("x", "fx", true, None),
        registered("y", "ex", false, None),
    ]);
    let (_, _, event, built) = run_build(&inputs, &tools, true, Some(&factory), Vec::new());
    assert!(!sent_deferred(&event, "x"));
    assert!(sent_deferred(&event, "y"));
    assert!(built.is_some());
}

#[test]
fn build_full_servers_declare_their_tools_in_full() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let mut inputs = build_inputs();
    inputs.deferrals = crate::prompt::Deferrals {
        tools: std::collections::BTreeMap::new(),
        full_servers: ["fx".to_owned()].into_iter().collect(),
    };
    let tools = tools_map(vec![
        registered("fx_echo", "fx", true, None),
        registered("mcp_resources", "builtin", true, None),
    ]);
    let (_, _, event, built) = run_build(&inputs, &tools, true, Some(&factory), Vec::new());
    assert!(!sent_deferred(&event, "fx_echo"));
    assert!(sent_deferred(&event, "mcp_resources"));
    assert!(built.is_some());
}

#[test]
fn build_per_tool_key_wins_over_full_servers() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let mut inputs = build_inputs();
    inputs.deferrals = crate::prompt::Deferrals {
        tools: [("fx_tool".to_owned(), true)].into_iter().collect(),
        full_servers: ["fx".to_owned()].into_iter().collect(),
    };
    let tools = tools_map(vec![registered("fx_tool", "fx", true, None)]);
    let (_, _, event, _) = run_build(&inputs, &tools, true, Some(&factory), Vec::new());
    assert!(sent_deferred(&event, "fx_tool"));
}

#[test]
fn build_leaves_a_configured_search_name_in_full() {
    let mut inputs = build_inputs();
    inputs.deferrals = crate::prompt::Deferrals {
        tools: [("tool_search".to_owned(), true)].into_iter().collect(),
        full_servers: std::collections::BTreeSet::new(),
    };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let tools = tools_map(vec![
        registered("tool_search", "ex", false, Some("Ext search guide.")),
        registered("alpha", "fx", true, None),
    ]);
    let (system, _, event, built) = run_build(&inputs, &tools, true, Some(&factory), Vec::new());
    assert!(built.is_none(), "the factory tool is not registered");
    assert!(!sent_deferred(&event, "tool_search"));
    assert!(system.contains("Ext search guide."));
    assert_eq!(
        event.replaced,
        vec![contract::events::ToolReplaced {
            name: "tool_search".into(),
            from: "builtin".into(),
            to: "ex".into(),
        }]
    );
}

#[test]
fn build_without_a_factory_records_no_search_replacement() {
    let mut inputs = build_inputs();
    inputs.deferrals = crate::prompt::Deferrals {
        tools: [("tool_search".to_owned(), true)].into_iter().collect(),
        full_servers: std::collections::BTreeSet::new(),
    };
    let tools = tools_map(vec![
        registered("tool_search", "ex", false, None),
        registered("alpha", "fx", true, None),
    ]);
    let (_, _, event, built) = run_build(&inputs, &tools, true, None, Vec::new());
    assert!(built.is_none());
    assert!(event.replaced.is_empty());
}

#[test]
fn build_sources_are_sorted_unique_and_leave_builtin_out() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory = search_factory(&seen);
    let tools = tools_map(vec![
        registered("a1", "zeta", true, None),
        registered("a2", "alpha", true, None),
        registered("a3", "alpha", true, None),
        registered("b", "builtin", true, None),
    ]);
    let (_, _, event, built) = run_build(&build_inputs(), &tools, true, Some(&factory), Vec::new());
    assert!(built.is_some());
    assert!(sent_deferred(&event, "b"), "a builtin tool still defers");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, ["alpha", "zeta"]);
}
