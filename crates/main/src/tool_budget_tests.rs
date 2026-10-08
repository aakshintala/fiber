//! The built-in tool definitions' byte budget (`docs/tools.md`, "Size
//! budget in CI"): every built-in definition is serialised in every spoken
//! protocol's request shape, each size is printed, and the largest
//! per-protocol total may not pass `BUDGET`. Raising `BUDGET` is an explicit
//! change in the same pull request that grows a definition.

use std::sync::Arc;

use config::{Protocol, ProviderData};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::provider::{Provider, ToolDefinition};
use contract::shapes::Failure;
use serde_json::{Map, Value, json};

/// The largest per-protocol total the built-in definitions may take, in
/// bytes. It started at the largest total at the commit that added the
/// check, with no headroom.
const BUDGET: usize = 8_881;

/// The one hosted tool type a protocol reads back
/// (`config::Protocol::reads_web_search`).
const HOSTED_SEARCH: &str = "web_search_20250305";

/// Every wire protocol; `protocol_name`'s exhaustive match keeps this list
/// whole when a protocol is added. `scripted` builds no request, so it has
/// no wire tools to measure (`docs/model-routing.md`, "The scripted
/// provider").
const PROTOCOLS: [Protocol; 5] = [
    Protocol::AnthropicMessages,
    Protocol::BedrockConverse,
    Protocol::GoogleGenerativeAi,
    Protocol::OpenaiCompletions,
    Protocol::OpenaiResponses,
];

/// The protocol as provider data spells it.
fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::AnthropicMessages => "anthropic-messages",
        Protocol::BedrockConverse => "bedrock-converse",
        Protocol::GoogleGenerativeAi => "google-generative-ai",
        Protocol::OpenaiCompletions => "openai-completions",
        Protocol::OpenaiResponses => "openai-responses",
        Protocol::Scripted => "scripted",
    }
}

/// The delegates the budget measures tools for: nothing resolves, so
/// `delegate_spawn` is declared but never runs one.
fn delegates() -> crate::delegates::Delegates {
    let root = fakes::TempDir::new("fiber-tool-delegate");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    crate::delegates::Delegates::new(
        root.path().join("fiber-stub"),
        root.path().join("home"),
        contract::SessionId("s_test".into()),
        root.path().to_path_buf(),
        root.path().join("sessions"),
        jobs,
        clock,
        Arc::new(|_| Err(Vec::new())),
    )
}

/// Every definition `builtin` registers, the hosted search included.
fn builtin_definitions() -> Vec<ToolDefinition> {
    let root = fakes::TempDir::new("fiber-tool-budget");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        Some(HOSTED_SEARCH),
        &delegates(),
    )
    .unwrap();
    let definitions: Vec<ToolDefinition> =
        tools.iter().map(|(_, tool)| tool.definition()).collect();
    assert!(!definitions.is_empty(), "builtin registered no tool");
    definitions
}

/// One protocol's bytes: each tool's in name order, and their sum.
struct Sizes {
    protocol: &'static str,
    tools: Vec<(String, usize)>,
    total: usize,
}

/// What `measure` found: the spoken protocols' sizes and the protocols
/// this Fiber does not speak.
struct Measured {
    sizes: Vec<Sizes>,
    not_spoken: Vec<&'static str>,
}

/// The provider a session reaches for `protocol`, through the session's own
/// `connect`, or `None` when the protocol is not spoken.
fn provider(protocol: Protocol) -> Result<Option<Arc<dyn Provider>>, String> {
    let data: ProviderData = serde_json::from_value(json!({
        "name": "budget",
        "models": [{
            "id": "model",
            "protocol": protocol_name(protocol),
            "base_url": "https://x/v1",
        }],
    }))
    .map_err(|error| format!("{}: provider data: {error}", protocol_name(protocol)))?;
    let model = extensions::Model {
        provider: &data,
        model: &data.models[0],
        thinking: None,
    };
    let here = crate::connect::Here {
        workspace: std::path::PathBuf::new(),
        clock: fakes::clock::FakeClock::new(),
    };
    spoken(crate::connect::connect(model, None, None, None, &here))
}

/// `connect`'s result: a protocol this Fiber does not speak yet is `None`;
/// any other failure is an error.
fn spoken(result: Result<Arc<dyn Provider>, Failure>) -> Result<Option<Arc<dyn Provider>>, String> {
    match result {
        Ok(provider) => Ok(Some(provider)),
        Err(failure) if failure.code == ErrorCode::ProtocolUnsupported => Ok(None),
        Err(failure) => Err(format!("{:?}: {}", failure.code, failure.message)),
    }
}

/// Each definition's bytes in every spoken protocol's request shape. A
/// hosted definition counts only where the protocol reads its type. The
/// whole set goes through `wire_tools` at once, since the strict-tool
/// budget is spent across it.
fn measure(definitions: &[ToolDefinition]) -> Measured {
    let mut measured = Measured {
        sizes: Vec::new(),
        not_spoken: Vec::new(),
    };
    for protocol in PROTOCOLS {
        let name = protocol_name(protocol);
        let Some(provider) = provider(protocol).unwrap() else {
            measured.not_spoken.push(name);
            continue;
        };
        let kept: Vec<ToolDefinition> = definitions
            .iter()
            .filter(|definition| {
                definition
                    .hosted
                    .as_deref()
                    .is_none_or(|kind| protocol.reads_web_search(kind))
            })
            .cloned()
            .collect();
        let mut names: Vec<String> = kept.iter().map(|tool| tool.name.clone()).collect();
        names.sort();
        let objects = provider.wire_tools(&kept);
        if let Err(error) = aligned(&names, &objects) {
            panic!("{name}: {error}");
        }
        let tools: Vec<(String, usize)> = names
            .into_iter()
            .zip(&objects)
            .map(|(tool, object)| {
                assert!(
                    !object.is_empty(),
                    "{name}: `{tool}` has an empty wire object"
                );
                let bytes = serde_json::to_vec(&Value::Object(object.clone())).unwrap();
                (tool, bytes.len())
            })
            .collect();
        let total = tools.iter().map(|(_, bytes)| bytes).sum();
        measured.sizes.push(Sizes {
            protocol: name,
            tools,
            total,
        });
    }
    measured
}

/// Whether each wire object carries the name it is paired with: `name` at
/// the top level, or `function.name` in `openai-completions`' shape.
fn aligned(names: &[String], objects: &[Map<String, Value>]) -> Result<(), String> {
    if names.len() != objects.len() {
        return Err(format!(
            "{} wire objects for {} definitions",
            objects.len(),
            names.len()
        ));
    }
    for (name, object) in names.iter().zip(objects) {
        let wire = object
            .get("name")
            .or_else(|| object.get("function")?.get("name"))
            .and_then(Value::as_str);
        match wire {
            Some(wire) if wire == name => {}
            Some(wire) => {
                return Err(format!(
                    "the wire object named `{wire}` is paired with `{name}`"
                ));
            }
            None => return Err(format!("the wire object paired with `{name}` has no name")),
        }
    }
    Ok(())
}

/// The protocol with the largest total, if any protocol was measured.
fn largest(sizes: &[Sizes]) -> Option<&Sizes> {
    sizes.iter().max_by_key(|sizes| sizes.total)
}

/// One line per protocol and tool, one total per protocol, the protocols
/// not spoken, then the largest total and the budget.
fn report(measured: &Measured, budget: usize) -> String {
    let mut lines = vec![String::from(
        "built-in tool definition bytes per protocol (docs/tools.md, \"Size budget in CI\")",
    )];
    for sizes in &measured.sizes {
        for (tool, bytes) in &sizes.tools {
            lines.push(format!("{} {tool} {bytes}", sizes.protocol));
        }
        lines.push(format!("{} total {}", sizes.protocol, sizes.total));
    }
    for protocol in &measured.not_spoken {
        lines.push(format!("{protocol} not spoken"));
    }
    lines.push(match largest(&measured.sizes) {
        Some(sizes) => format!(
            "largest {} ({}), budget {budget}",
            sizes.total, sizes.protocol
        ),
        None => format!("no protocol measured, budget {budget}"),
    });
    lines.join("\n")
}

/// The largest total when it is within `budget`; otherwise what to do,
/// followed by every size.
fn check(measured: &Measured, budget: usize) -> Result<usize, String> {
    let Some(sizes) = largest(&measured.sizes) else {
        return Err(format!(
            "no protocol was measured\n{}",
            report(measured, budget)
        ));
    };
    if sizes.total <= budget {
        return Ok(sizes.total);
    }
    Err(format!(
        "the built-in tool definitions take {} bytes in {}, past the budget of {budget}: \
         raise `BUDGET` in crates/main/src/tool_budget_tests.rs in this pull request \
         (docs/tools.md, \"Size budget in CI\")\n{}",
        sizes.total,
        sizes.protocol,
        report(measured, budget)
    ))
}

/// A protocol with one tool of `total` bytes.
fn synthetic(protocol: &'static str, total: usize) -> Sizes {
    Sizes {
        protocol,
        tools: vec![(String::from("tool"), total)],
        total,
    }
}

/// A definition whose schema fits the strict subset.
fn strict_tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: String::from("Does one thing."),
        input_schema: json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a"],
            "additionalProperties": false,
        }),
        deferred: false,
        hosted: None,
    }
}

/// The measured bytes of `tool` in `protocol`.
fn bytes_of(measured: &Measured, protocol: Protocol, tool: &str) -> usize {
    let sizes = measured
        .sizes
        .iter()
        .find(|sizes| sizes.protocol == protocol_name(protocol))
        .unwrap();
    sizes
        .tools
        .iter()
        .find(|(name, _)| name == tool)
        .map(|(_, bytes)| *bytes)
        .unwrap()
}

#[test]
fn the_check_passes_at_the_budget_and_fails_one_byte_over() {
    let measured = measure(&builtin_definitions());
    let largest = largest(&measured.sizes).unwrap();
    let (protocol, total) = (largest.protocol, largest.total);

    assert_eq!(check(&measured, total), Ok(total));
    let error = check(&measured, total - 1).unwrap_err();
    assert!(error.contains(protocol), "{error}");
    assert!(error.contains(&format!("take {total} bytes")), "{error}");
    assert!(
        error.contains(&format!("budget of {}", total - 1)),
        "{error}"
    );
    assert!(error.contains("`BUDGET`"), "{error}");
}

#[test]
fn the_largest_protocol_decides() {
    let measured = Measured {
        sizes: vec![synthetic("small", 100), synthetic("large", 120)],
        not_spoken: Vec::new(),
    };

    let error = check(&measured, 110).unwrap_err();
    assert!(error.contains("take 120 bytes in large"), "{error}");
    assert_eq!(check(&measured, 120), Ok(120));
}

#[test]
fn no_protocol_measured_fails() {
    let measured = Measured {
        sizes: Vec::new(),
        not_spoken: Vec::new(),
    };

    assert!(check(&measured, 1_000_000).is_err());
}

#[test]
fn a_definition_pushed_over_the_budget_fails_the_check() {
    let definitions = builtin_definitions();
    let before = measure(&definitions);
    let largest = largest(&before.sizes).unwrap().total;
    let grown_by = BUDGET.saturating_sub(largest) + 1;
    let mut grown = definitions;
    let first = grown
        .iter_mut()
        .find(|definition| definition.hosted.is_none())
        .unwrap();
    first.description.push_str(&"x".repeat(grown_by));

    let after = measure(&grown);

    assert!(check(&after, BUDGET).is_err());
    assert_eq!(before.sizes.len(), after.sizes.len());
    for (before, after) in before.sizes.iter().zip(&after.sizes) {
        assert_eq!(before.protocol, after.protocol);
        assert_eq!(after.total, before.total + grown_by, "{}", after.protocol);
    }
}

#[test]
fn every_spoken_protocol_measures_every_builtin() {
    let definitions = builtin_definitions();
    let (expected_spoken, expected_not_spoken): (Vec<Protocol>, Vec<Protocol>) = PROTOCOLS
        .into_iter()
        .partition(|protocol| provider(*protocol).unwrap().is_some());

    let measured = measure(&definitions);

    let spoken: Vec<&str> = measured.sizes.iter().map(|sizes| sizes.protocol).collect();
    let names = |protocols: Vec<Protocol>| -> Vec<&str> {
        protocols.into_iter().map(protocol_name).collect()
    };
    assert!(!spoken.is_empty());
    assert_eq!(spoken, names(expected_spoken.clone()));
    assert_eq!(measured.not_spoken, names(expected_not_spoken));
    for (protocol, sizes) in expected_spoken.into_iter().zip(&measured.sizes) {
        let mut expected: Vec<&str> = definitions
            .iter()
            .filter(|definition| {
                definition
                    .hosted
                    .as_deref()
                    .is_none_or(|kind| protocol.reads_web_search(kind))
            })
            .map(|definition| definition.name.as_str())
            .collect();
        expected.sort_unstable();
        let measured: Vec<&str> = sizes.tools.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(measured, expected, "{}", sizes.protocol);
        assert_eq!(
            sizes.total,
            sizes.tools.iter().map(|(_, bytes)| bytes).sum::<usize>()
        );
    }
}

#[test]
fn an_unsupported_protocol_is_not_spoken() {
    let unsupported = spoken(Err(crate::failed(ErrorCode::ProtocolUnsupported, "no")));
    assert!(matches!(unsupported, Ok(None)));

    let other = spoken(Err(crate::failed(ErrorCode::IoFailed, "broken")));
    assert!(other.is_err());
}

#[test]
fn the_hosted_search_counts_only_where_a_protocol_reads_it() {
    let measured = measure(&builtin_definitions());
    for sizes in &measured.sizes {
        let protocol = PROTOCOLS
            .into_iter()
            .find(|protocol| protocol_name(*protocol) == sizes.protocol)
            .unwrap();
        let counted = sizes.tools.iter().any(|(name, _)| name == "web_search");
        assert_eq!(
            counted,
            protocol.reads_web_search(HOSTED_SEARCH),
            "{}",
            sizes.protocol
        );
    }

    let google = ToolDefinition {
        name: String::from("web_search"),
        description: String::new(),
        input_schema: json!({}),
        deferred: false,
        hosted: Some(String::from("google_search")),
    };
    let measured = measure(&[google]);
    assert!(!measured.sizes.is_empty());
    for sizes in &measured.sizes {
        assert!(sizes.tools.is_empty(), "{}", sizes.protocol);
        assert_eq!(sizes.total, 0);
    }
}

#[test]
fn misaligned_names_fail() {
    let names = [String::from("a"), String::from("b")];
    let object = |value: Value| value.as_object().cloned().unwrap();
    let swapped = [
        object(json!({"name": "b", "description": "a"})),
        object(json!({"name": "a", "description": "b"})),
    ];
    assert!(aligned(&names, &swapped).is_err());
    let ordered = [
        object(json!({"name": "a", "description": "b"})),
        object(json!({"name": "b", "description": "a"})),
    ];
    assert_eq!(aligned(&names, &ordered), Ok(()));

    let function = |name: &str, description: &str| {
        object(json!({"type": "function", "function": {"name": name, "description": description}}))
    };
    let swapped = [function("b", "a"), function("a", "b")];
    assert!(aligned(&names, &swapped).is_err());
    let ordered = [function("a", "b"), function("b", "a")];
    assert_eq!(aligned(&names, &ordered), Ok(()));

    let nameless = [object(json!({"description": "a"})), function("b", "a")];
    assert!(aligned(&names, &nameless).is_err());
}

#[test]
fn measure_sends_the_whole_set_through_wire_tools() {
    let definitions: Vec<ToolDefinition> = (0..22)
        .map(|index| strict_tool(&format!("tool_{index:02}")))
        .collect();

    let measured = measure(&definitions);

    let capped = bytes_of(&measured, Protocol::AnthropicMessages, "tool_19");
    for tool in ["tool_20", "tool_21"] {
        assert_eq!(
            bytes_of(&measured, Protocol::AnthropicMessages, tool),
            capped + 1,
            "{tool}"
        );
    }
    let first = bytes_of(&measured, Protocol::OpenaiResponses, "tool_00");
    for index in 0..22 {
        let tool = format!("tool_{index:02}");
        assert_eq!(
            bytes_of(&measured, Protocol::OpenaiResponses, &tool),
            first,
            "{tool}"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_built_in_definitions_fit_their_budget() {
    let measured = measure(&builtin_definitions());

    eprintln!("{}", report(&measured, BUDGET));
    if let Err(error) = check(&measured, BUDGET) {
        panic!("{error}");
    }
}
