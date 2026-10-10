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
use contract::search::{Domains, SearchBackend, SearchResult};
use contract::shapes::Failure;
use contract::tool::Tool;
use serde_json::{Map, Value, json};

/// The largest per-protocol total the built-in definitions may take, in
/// bytes. It started at the largest total at the commit that added the
/// check, with no headroom.
const BUDGET: usize = 10_174;

/// The Anthropic hosted tool type the budget measures
/// (`config::Protocol::reads_web_search`); `openai-responses` reads
/// `web_search` and `google-generative-ai` reads `google_search` instead.
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
        super::web_search(Some(HOSTED_SEARCH), None).unwrap(),
        &super::tests::delegates(),
        super::tests::skills(),
    )
    .unwrap();
    let definitions: Vec<ToolDefinition> =
        tools.iter().map(|(_, tool)| tool.definition()).collect();
    assert!(!definitions.is_empty(), "builtin registered no tool");
    definitions
}

/// A search backend answering nothing, for measuring Fiber's own
/// `web_search` in place of the hosted one.
struct StubBackend;

impl SearchBackend for StubBackend {
    fn search(
        &self,
        _query: &str,
        _domains: &Domains,
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<Option<Vec<SearchResult>>, Failure> {
        Ok(Some(Vec::new()))
    }
}

/// Every definition `builtin` registers with Fiber's own `web_search` over
/// a backend in place of the hosted one: the backend tool counts on every
/// protocol, as an ordinary function tool does.
fn backend_definitions() -> Vec<ToolDefinition> {
    let mut definitions = builtin_definitions();
    let backend: Arc<dyn SearchBackend> = Arc::new(StubBackend);
    let own = tools::BackendSearch::new(backend).definition();
    let hosted = definitions
        .iter_mut()
        .find(|definition| definition.hosted.is_some())
        .expect("builtin registered the hosted search");
    *hosted = own;
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

/// The largest total over both definition sets when each is within
/// `budget`; otherwise the first set past it, with its sizes.
fn check_both(hosted: &Measured, backend: &Measured, budget: usize) -> Result<usize, String> {
    match (check(hosted, budget), check(backend, budget)) {
        (Ok(hosted), Ok(backend)) => Ok(hosted.max(backend)),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error),
    }
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

#[test]
fn the_check_passes_at_the_budget_and_fails_one_byte_over() {
    let hosted = measure(&builtin_definitions());
    let backend = measure(&backend_definitions());
    // The expected largest, folded out of `the_largest_protocol_decides`:
    // the largest total over both measurements and the protocol that
    // holds it, without calling `largest`, so a `largest` that picked
    // the smallest total fails below.
    let (measured, largest) = [&hosted, &backend]
        .into_iter()
        .flat_map(|measured| measured.sizes.iter().map(move |sizes| (measured, sizes)))
        .max_by_key(|(_, sizes)| sizes.total)
        .unwrap();
    let (protocol, total) = (largest.protocol, largest.total);

    assert_eq!(check(measured, total), Ok(total));
    let error = check(measured, total - 1).unwrap_err();
    assert!(error.contains(protocol), "{error}");
    assert!(error.contains(&format!("take {total} bytes")), "{error}");
    assert!(
        error.contains(&format!("budget of {}", total - 1)),
        "{error}"
    );
    assert!(error.contains("`BUDGET`"), "{error}");
    assert_eq!(check_both(&hosted, &backend, total), Ok(total));
    assert!(check_both(&hosted, &backend, total - 1).is_err());
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

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_built_in_definitions_fit_their_budget() {
    let hosted = measure(&builtin_definitions());
    let backend = measure(&backend_definitions());

    eprintln!("{}", report(&hosted, BUDGET));
    eprintln!("{}", report(&backend, BUDGET));
    if let Err(error) = check_both(&hosted, &backend, BUDGET) {
        panic!("{error}");
    }
}
